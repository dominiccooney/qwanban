use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, State},
    http::{Request, StatusCode},
    routing::{get, post},
};
use control_tower::{
    config::Config,
    controller::App,
    model::{Outcome, Phase, Submission},
};
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tower::ServiceExt;
use uuid::Uuid;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("ct-test-{}", Uuid::new_v4())))
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct Fake {
    instance: Option<Value>,
    inserted: Vec<Value>,
    registrations: Vec<Value>,
    removed: Vec<String>,
    deleted: Vec<String>,
    pending_delete: bool,
    registration_error: bool,
    removal_error: bool,
    lookup_error: bool,
    insert_error: bool,
    delete_error: bool,
    lookup_count: usize,
    request_ids: Vec<String>,
    pending_insert: bool,
    operation_override: Option<Value>,
    lookup_delay: bool,
    insert_status: Option<StatusCode>,
    lookup_status: Option<StatusCode>,
    delete_status: Option<StatusCode>,
    uncertain_insert: bool,
    operation_count: usize,
    token_error: Option<(StatusCode, &'static str)>,
    token_count: usize,
}
type FakeState = Arc<Mutex<Fake>>;

async fn insert(
    State(state): State<FakeState>,
    Query(query): Query<std::collections::HashMap<String, String>>,
    Json(body): Json<Value>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let mut state = state.lock().unwrap();
    state
        .request_ids
        .push(query.get("requestId").unwrap().clone());
    state.inserted.push(body.clone());
    if let Some(status) = state.insert_status {
        return cloud_error(status).into_response();
    }
    if !state.insert_error {
        state.instance = Some(
            json!({"id":(123455+state.inserted.len()).to_string(),"name":body["name"],"status":"RUNNING","labels":body["labels"],"networkInterfaces":[{"networkIP":"127.0.0.1"}]}),
        );
    }
    if state.uncertain_insert {
        return cloud_error(StatusCode::SERVICE_UNAVAILABLE).into_response();
    }
    Json(
        json!({"name":"insert-1","status":if state.pending_insert{"RUNNING"}else{"DONE"},"error":if state.insert_error{json!({"errors":[{"code":"QUOTA_EXCEEDED","message":"quota"}]})}else{Value::Null}}),
    ).into_response()
}
fn cloud_error(status: StatusCode) -> (StatusCode, Json<Value>) {
    let error = if status == StatusCode::FORBIDDEN {
        json!({"code":403,"message":"Request had insufficient authentication scopes.","status":"PERMISSION_DENIED","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"ACCESS_TOKEN_SCOPE_INSUFFICIENT","domain":"googleapis.com"}]})
    } else {
        json!({"code":status.as_u16(),"message":status.canonical_reason().unwrap_or("error")})
    };
    (status, Json(json!({"error":error})))
}
async fn token(State(state): State<FakeState>) -> (StatusCode, Json<Value>) {
    let mut state = state.lock().unwrap();
    state.token_count += 1;
    match state.token_error {
        Some((status, error)) => (
            status,
            Json(json!({"error":error,"error_description":"test token failure"})),
        ),
        None => (
            StatusCode::OK,
            Json(json!({"access_token":"fixture","token_type":"Bearer","expires_in":3600})),
        ),
    }
}
async fn instance(
    State(state): State<FakeState>,
    Path(id): Path<String>,
    request: Request<Body>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let delay = state.lock().unwrap().lookup_delay;
    if delay {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    let mut state = state.lock().unwrap();
    if request.method() == axum::http::Method::GET {
        state.lookup_count += 1;
        if let Some(status) = state.lookup_status {
            return cloud_error(status).into_response();
        }
    }
    if !state.lookup_error
        && !state
            .instance
            .as_ref()
            .is_some_and(|instance| instance["id"] == id || instance["name"] == id)
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    if request.method() == axum::http::Method::DELETE {
        state.deleted.push(id);
        if let Some(status) = state.delete_status {
            return cloud_error(status).into_response();
        }
        if !state.pending_delete {
            state.instance = None;
        }
        return Json(
            json!({"name":"delete-1","status":if state.pending_delete{"RUNNING"}else{"DONE"}}),
        )
        .into_response();
    }
    if state.lookup_error {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match &state.instance {
        Some(instance) => Json(instance.clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
async fn operation(State(state): State<FakeState>, Path(name): Path<String>) -> Json<Value> {
    let mut state = state.lock().unwrap();
    state.operation_count += 1;
    if let Some(operation) = &state.operation_override {
        return Json(operation.clone());
    }
    Json(
        json!({"name":name,"status":if name=="delete-1" && state.pending_delete || name=="insert-1" && state.pending_insert{"RUNNING"}else{"DONE"},"error":if name=="insert-1" && state.insert_error || name=="delete-1" && state.delete_error{json!({"errors":[{"code":"QUOTA_EXCEEDED"}]})}else{Value::Null}}),
    )
}
async fn register(State(state): State<FakeState>, Json(body): Json<Value>) -> StatusCode {
    let mut state = state.lock().unwrap();
    state.registrations.push(body);
    if state.registration_error {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::CREATED
    }
}
async fn unregister(State(state): State<FakeState>, Path(id): Path<String>) -> StatusCode {
    let mut state = state.lock().unwrap();
    state.removed.push(id);
    if state.removal_error {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::NO_CONTENT
    }
}

struct Fixture {
    _temp: Temp,
    fake: FakeState,
    config: Config,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new() -> Self {
        let temp = Temp::new();
        let fake = Arc::new(Mutex::new(Fake::default()));
        let router = Router::new()
            .route("/token", post(token))
            .route(
                "/compute/v1/projects/test-project/zones/test-zone/instances",
                post(insert),
            )
            .route(
                "/compute/v1/projects/test-project/zones/test-zone/instances/{id}",
                get(instance).delete(instance),
            )
            .route(
                "/compute/v1/projects/test-project/zones/test-zone/operations/{name}",
                get(operation),
            )
            .route("/api/containers", post(register))
            .route("/api/containers/{id}", axum::routing::delete(unregister))
            .with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let mut config: Config =
            serde_yaml::from_str(include_str!("../config.example.yaml")).unwrap();
        config.cloud.endpoint = format!("http://{addr}");
        config.cloud.project = "test-project".into();
        config.friendzone_url = format!("http://{addr}");
        config.data_dir = temp.0.clone();
        config.ui_dir = temp.0.join("ui");
        config.worker_profiles.get_mut("windows").unwrap().zone = "test-zone".into();
        config.limits.max_artifact_bytes = 32;
        config.limits.artifact_budget_bytes = 64;
        Self {
            _temp: temp,
            fake,
            config,
            server,
        }
    }
    fn app(&self) -> Arc<App> {
        App::open(
            self.config.clone(),
            google_cloud_auth::credentials::anonymous::Builder::new().build(),
        )
        .unwrap()
    }
}
fn submission() -> Submission {
    Submission {
        id: Uuid::new_v4(),
        worker_profile: "windows".into(),
        title: "test job".into(),
        source: "cron".into(),
        prompt: "inspect this".into(),
        setup_script: "Write-Output hello".into(),
    }
}
async fn running(app: &App) -> Uuid {
    let id = app.submit(submission()).await.unwrap().submission.id;
    for _ in 0..4 {
        app.tick().await.unwrap();
    }
    assert!(
        app.changed.borrow().jobs[&id].running(),
        "{:?}",
        app.changed.borrow().jobs[&id]
    );
    id
}
// Advance a persisted retry deadline without sleeping; all RPCs still run
// through reconciliation and the real SDK against the wire fixture.
async fn retry_due(app: &App, id: Uuid) {
    let mut store = app.store.lock().await;
    let mut next = store.ledger.clone();
    let Phase::Retrying {
        next_attempt_at, ..
    } = &mut next.jobs.get_mut(&id).unwrap().phase
    else {
        panic!("expected retrying job");
    };
    *next_attempt_at = 0;
    store.commit(next).unwrap();
    app.publish(&store);
}
fn request(path: &str, method: &str, peer: &str, body: Body) -> Request<Body> {
    let mut request = Request::builder()
        .uri(path)
        .method(method)
        .header("host", "127.0.0.1:8090")
        .header("content-type", "application/json")
        .body(body)
        .unwrap();
    request.extensions_mut().insert(axum::extract::ConnectInfo(
        peer.parse::<SocketAddr>().unwrap(),
    ));
    request
}

#[tokio::test]
async fn lifecycle_restarts_reuses_submission_and_deletes_the_exact_instance() {
    let fixture = Fixture::new().await;
    let app = fixture.app();
    let input = submission();
    let id = input.id;
    app.submit(input.clone()).await.unwrap();
    app.submit(input.clone()).await.unwrap();
    let mut conflicting = input;
    conflicting.prompt = "different".into();
    assert!(app.submit(conflicting).await.is_err());
    app.tick().await.unwrap();
    app.tick().await.unwrap();
    drop(app);
    let app = fixture.app();
    app.tick().await.unwrap();
    app.tick().await.unwrap();
    assert!(
        app.changed.borrow().jobs[&id].running(),
        "{:?}",
        app.changed.borrow().jobs[&id]
    );
    {
        let fake = fixture.fake.lock().unwrap();
        assert_eq!(fake.inserted.len(), 1);
        assert_eq!(fake.request_ids, vec![id.to_string()]);
        let body = &fake.inserted[0];
        assert!(
            body["networkInterfaces"][0]["accessConfigs"]
                .as_array()
                .is_none_or(Vec::is_empty)
        );
        assert!(body["serviceAccounts"].as_array().is_none_or(Vec::is_empty));
        assert_eq!(body["disks"][0]["autoDelete"], true);
        let metadata = &body["metadata"]["items"];
        assert!(
            metadata.as_array().unwrap().iter().any(
                |item| item["key"] == "control-tower-prompt" && item["value"] == "inspect this"
            )
        );
    }
    app.finish(id, Some("127.0.0.1".parse().unwrap()), Outcome::Completed)
        .await
        .unwrap();
    app.tick().await.unwrap();
    assert!(matches!(
        app.changed.borrow().jobs[&id].phase,
        Phase::Finished
    ));
    assert_eq!(fixture.fake.lock().unwrap().deleted, vec!["123456"]);
    assert_eq!(
        fixture.fake.lock().unwrap().removed,
        vec![format!("ct-{}", id.simple())]
    );
}

#[tokio::test]
async fn cleanup_waits_for_gce_and_retries_friendzone_without_leaving_the_vm_running() {
    let fixture = Fixture::new().await;
    let app = fixture.app();
    let id = running(&app).await;
    {
        let mut fake = fixture.fake.lock().unwrap();
        fake.pending_delete = true;
        fake.removal_error = true;
    }
    app.finish(id, None, Outcome::Cancelled).await.unwrap();
    app.tick().await.unwrap();
    assert!(matches!(
        app.changed.borrow().jobs[&id].phase.work(),
        Phase::Cleaning {
            unregister: true,
            delete: true,
            ..
        }
    ));
    assert_eq!(fixture.fake.lock().unwrap().deleted.len(), 1);
    {
        let mut fake = fixture.fake.lock().unwrap();
        fake.pending_delete = false;
        fake.instance = None;
        fake.removal_error = false;
    }
    retry_due(&app, id).await;
    app.tick().await.unwrap();
    assert!(matches!(
        app.changed.borrow().jobs[&id].phase,
        Phase::Finished
    ));
}

#[tokio::test]
async fn registration_retries_and_cloud_outage_is_not_a_dead_worker() {
    let fixture = Fixture::new().await;
    let app = fixture.app();
    let id = app.submit(submission()).await.unwrap().submission.id;
    fixture.fake.lock().unwrap().registration_error = true;
    for _ in 0..4 {
        app.tick().await.unwrap();
    }
    assert!(matches!(
        app.changed.borrow().jobs[&id].phase.work(),
        Phase::Registering
    ));
    fixture.fake.lock().unwrap().registration_error = false;
    retry_due(&app, id).await;
    app.tick().await.unwrap();
    fixture.fake.lock().unwrap().lookup_error = true;
    let lookups = fixture.fake.lock().unwrap().lookup_count;
    app.tick().await.unwrap();
    assert_eq!(
        fixture.fake.lock().unwrap().lookup_count,
        lookups + 1,
        "SDK must not retry outside ledger reconciliation"
    );
    assert!(app.changed.borrow().jobs[&id].running());
    {
        let mut fake = fixture.fake.lock().unwrap();
        fake.lookup_error = false;
        fake.instance = None;
    }
    retry_due(&app, id).await;
    app.tick().await.unwrap();
    assert_eq!(app.changed.borrow().jobs[&id].outcome, Some(Outcome::Lost));
    app.tick().await.unwrap();
    assert!(!app.changed.borrow().jobs[&id].active());
}

#[tokio::test]
async fn insufficient_scopes_stop_launch_calls_across_restart_and_retry_resumes_the_same_job() {
    for lookup in [true, false] {
        let fixture = Fixture::new().await;
        {
            let mut fake = fixture.fake.lock().unwrap();
            if lookup {
                fake.lookup_status = Some(StatusCode::FORBIDDEN);
            } else {
                fake.insert_status = Some(StatusCode::FORBIDDEN);
            }
        }
        let app = fixture.app();
        let submission = submission();
        let id = submission.id;
        app.submit(submission.clone()).await.unwrap();
        app.tick().await.unwrap();
        app.tick().await.unwrap();
        let job = app.changed.borrow().jobs[&id].clone();
        assert!(matches!(job.phase, Phase::Blocked { failures: 1, .. }));
        assert!(
            job.last_error
                .as_deref()
                .unwrap()
                .contains("insufficient authentication scopes")
        );
        let calls = fixture.fake.lock().unwrap().lookup_count;
        for _ in 0..10 {
            app.tick().await.unwrap();
        }
        assert_eq!(fixture.fake.lock().unwrap().lookup_count, calls);
        drop(app);
        let app = fixture.app();
        app.tick().await.unwrap();
        assert!(matches!(
            app.changed.borrow().jobs[&id].phase,
            Phase::Blocked { .. }
        ));
        assert_eq!(fixture.fake.lock().unwrap().lookup_count, calls);
        let repeated = app.submit(submission).await.unwrap();
        assert_eq!(repeated.submission.id, id);
        {
            let mut fake = fixture.fake.lock().unwrap();
            fake.lookup_status = None;
            fake.insert_status = None;
        }
        let router = control_tower::http::operator(app.clone());
        let response = router
            .oneshot(request(
                &format!("/api/jobs/{id}/retry"),
                "POST",
                "127.0.0.1:1234",
                Body::from("{}"),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(app.retry(id).await.is_err());
        for _ in 0..3 {
            app.tick().await.unwrap();
        }
        assert!(app.changed.borrow().jobs[&id].running());
        let fake = fixture.fake.lock().unwrap();
        assert_eq!(
            fake.request_ids,
            vec![id.to_string(); if lookup { 1 } else { 2 }]
        );
        assert_eq!(fake.registrations.len(), 1);
    }
}

#[tokio::test]
async fn transient_failures_back_off_and_block_after_five_attempts_even_across_restart() {
    for status in [
        StatusCode::TOO_MANY_REQUESTS,
        StatusCode::SERVICE_UNAVAILABLE,
    ] {
        let fixture = Fixture::new().await;
        let mut app = fixture.app();
        fixture.fake.lock().unwrap().lookup_status = Some(status);
        let id = app.submit(submission()).await.unwrap().submission.id;
        app.tick().await.unwrap();
        for failures in 1..=5 {
            let before = control_tower::model::now();
            app.tick().await.unwrap();
            let phase = app.changed.borrow().jobs[&id].phase.clone();
            if failures < 5 {
                let Phase::Retrying {
                    failures: count,
                    next_attempt_at,
                    ..
                } = phase
                else {
                    panic!("{phase:?}");
                };
                assert_eq!(count, failures);
                assert!(next_attempt_at >= before + (5u64 << (failures - 1)));
                app.tick().await.unwrap();
                assert_eq!(fixture.fake.lock().unwrap().lookup_count, failures as usize);
                if failures == 2 {
                    drop(app);
                    app = fixture.app();
                    app.tick().await.unwrap();
                    assert_eq!(fixture.fake.lock().unwrap().lookup_count, 2);
                }
                retry_due(&app, id).await;
            } else {
                assert!(matches!(phase, Phase::Blocked { failures: 5, .. }));
            }
        }
        for _ in 0..10 {
            app.tick().await.unwrap();
        }
        assert_eq!(fixture.fake.lock().unwrap().lookup_count, 5);
        fixture.fake.lock().unwrap().lookup_status = None;
        app.retry(id).await.unwrap();
        for _ in 0..3 {
            app.tick().await.unwrap();
        }
        assert!(app.changed.borrow().jobs[&id].running());
    }
}

#[tokio::test]
async fn invalid_requests_and_unauthenticated_insertions_block_immediately() {
    for status in [
        StatusCode::BAD_REQUEST,
        StatusCode::UNAUTHORIZED,
        StatusCode::CONFLICT,
    ] {
        let fixture = Fixture::new().await;
        let app = fixture.app();
        fixture.fake.lock().unwrap().insert_status = Some(status);
        let id = app.submit(submission()).await.unwrap().submission.id;
        app.tick().await.unwrap();
        app.tick().await.unwrap();
        assert!(matches!(
            app.changed.borrow().jobs[&id].phase,
            Phase::Blocked { failures: 1, .. }
        ));
        for _ in 0..5 {
            app.tick().await.unwrap();
        }
        assert_eq!(fixture.fake.lock().unwrap().inserted.len(), 1);
    }
}

#[tokio::test]
async fn actual_sdk_credentials_classify_invalid_grant_and_transient_token_service_errors() {
    for (status, error, transient) in [
        (StatusCode::BAD_REQUEST, "invalid_grant", false),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "temporarily_unavailable",
            true,
        ),
    ] {
        let fixture = Fixture::new().await;
        fixture.fake.lock().unwrap().token_error = Some((status, error));
        let credentials=google_cloud_auth::credentials::user_account::Builder::new(json!({"type":"authorized_user","client_id":"fixture","client_secret":"fixture","refresh_token":"fixture"}))
            .with_token_uri(format!("{}/token",fixture.config.cloud.endpoint))
            .with_retry_policy(google_cloud_gax::retry_policy::NeverRetry).build().unwrap();
        let app = App::open(fixture.config.clone(), credentials).unwrap();
        let id = app.submit(submission()).await.unwrap().submission.id;
        app.tick().await.unwrap();
        app.tick().await.unwrap();
        let job = app.changed.borrow().jobs[&id].clone();
        if transient {
            assert!(
                matches!(job.phase, Phase::Retrying { failures: 1, .. }),
                "{job:?}"
            );
        } else {
            assert!(
                matches!(job.phase, Phase::Blocked { failures: 1, .. }),
                "{job:?}"
            );
        }
        assert!(job.last_error.is_some());
        assert_eq!(fixture.fake.lock().unwrap().token_count, 1);
        app.tick().await.unwrap();
        assert_eq!(fixture.fake.lock().unwrap().token_count, 1);
        assert!(fixture.fake.lock().unwrap().inserted.is_empty());
    }
}

#[tokio::test]
async fn worker_can_upload_and_complete_when_only_cloud_monitoring_is_blocked() {
    let fixture = Fixture::new().await;
    let app = fixture.app();
    let id = running(&app).await;
    fixture.fake.lock().unwrap().lookup_status = Some(StatusCode::FORBIDDEN);
    app.tick().await.unwrap();
    let response = control_tower::http::worker(app.clone())
        .oneshot(request(
            &format!("/jobs/{id}/artifact"),
            "POST",
            "127.0.0.1:1234",
            Body::from(vec![0x28, 0xb5, 0x2f, 0xfd, 1]),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    app.finish(id, Some("127.0.0.1".parse().unwrap()), Outcome::Completed)
        .await
        .unwrap();
    app.tick().await.unwrap();
    assert!(!app.changed.borrow().jobs[&id].active());
    assert_eq!(fixture.fake.lock().unwrap().deleted, vec!["123456"]);
}

#[tokio::test]
async fn uncertain_insert_is_retained_when_permissions_then_block_and_cancel_deletes_the_incarnation()
 {
    let fixture = Fixture::new().await;
    let app = fixture.app();
    fixture.fake.lock().unwrap().uncertain_insert = true;
    let id = app.submit(submission()).await.unwrap().submission.id;
    app.tick().await.unwrap();
    app.tick().await.unwrap();
    assert!(fixture.fake.lock().unwrap().instance.is_some());
    fixture.fake.lock().unwrap().lookup_status = Some(StatusCode::FORBIDDEN);
    retry_due(&app, id).await;
    app.tick().await.unwrap();
    assert!(matches!(
        app.changed.borrow().jobs[&id].phase,
        Phase::Blocked { .. }
    ));
    fixture.fake.lock().unwrap().lookup_status = None;
    app.finish(id, None, Outcome::Cancelled).await.unwrap();
    app.tick().await.unwrap();
    app.tick().await.unwrap();
    assert!(!app.changed.borrow().jobs[&id].active());
    assert_eq!(fixture.fake.lock().unwrap().deleted, vec!["123456"]);
    assert_eq!(fixture.fake.lock().unwrap().inserted.len(), 1);
}

#[tokio::test]
async fn denied_cleanup_blocks_without_forgetting_vm_and_operator_retry_finishes_deletion() {
    let fixture = Fixture::new().await;
    let app = fixture.app();
    let id = running(&app).await;
    fixture.fake.lock().unwrap().delete_status = Some(StatusCode::FORBIDDEN);
    app.finish(id, None, Outcome::Completed).await.unwrap();
    app.tick().await.unwrap();
    let job = app.changed.borrow().jobs[&id].clone();
    assert!(matches!(job.phase, Phase::Blocked { .. }));
    assert!(job.finished_at.is_none());
    assert_eq!(job.instance_id.as_deref(), Some("123456"));
    assert!(matches!(
        job.phase.work(),
        Phase::Cleaning {
            unregister: false,
            delete: true,
            ..
        }
    ));
    for _ in 0..5 {
        app.tick().await.unwrap();
    }
    assert_eq!(fixture.fake.lock().unwrap().deleted.len(), 1);
    fixture.fake.lock().unwrap().delete_status = None;
    app.retry(id).await.unwrap();
    app.tick().await.unwrap();
    assert!(!app.changed.borrow().jobs[&id].active());
    assert_eq!(
        fixture.fake.lock().unwrap().deleted,
        vec!["123456", "123456"]
    );
    assert_eq!(fixture.fake.lock().unwrap().removed.len(), 1);
}

#[tokio::test]
async fn blocked_running_worker_times_out_into_cleanup_and_keeps_its_identity() {
    let fixture = Fixture::new().await;
    let app = fixture.app();
    let id = running(&app).await;
    fixture.fake.lock().unwrap().lookup_status = Some(StatusCode::FORBIDDEN);
    app.tick().await.unwrap();
    {
        let mut store = app.store.lock().await;
        let mut next = store.ledger.clone();
        next.jobs.get_mut(&id).unwrap().created_at = 0;
        store.commit(next).unwrap();
        app.publish(&store);
    }
    app.tick().await.unwrap();
    assert_eq!(
        app.changed.borrow().jobs[&id].outcome,
        Some(Outcome::TimedOut)
    );
    assert!(!app.changed.borrow().jobs[&id].active());
    assert_eq!(fixture.fake.lock().unwrap().deleted, vec!["123456"]);
}

#[tokio::test]
async fn version_two_ledger_migration_preserves_jobs_and_rejects_nested_suspended_phases() {
    let fixture = Fixture::new().await;
    let app = fixture.app();
    let id = running(&app).await;
    let mut value = serde_json::to_value(&**app.changed.borrow()).unwrap();
    value["version"] = json!(2);
    value["jobs"][id.to_string()]
        .as_object_mut()
        .unwrap()
        .remove("failure_error");
    drop(app);
    let path = fixture.config.data_dir.join("jobs.json");
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let app = fixture.app();
    assert_eq!(app.changed.borrow().version, 3);
    assert!(app.changed.borrow().jobs[&id].running());
    drop(app);
    value["jobs"][id.to_string()]["phase"] = json!({"kind":"blocked","failures":1,"phase":{"kind":"blocked","failures":1,"phase":{"kind":"running"}}});
    let bytes = serde_json::to_vec(&value).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    assert!(control_tower::storage::Store::open(&fixture.config.data_dir).is_err());
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[tokio::test]
async fn operation_error_is_not_a_successful_provision() {
    let fixture = Fixture::new().await;
    fixture.fake.lock().unwrap().insert_error = true;
    let app = fixture.app();
    let id = app.submit(submission()).await.unwrap().submission.id;
    app.tick().await.unwrap();
    app.tick().await.unwrap();
    app.tick().await.unwrap();
    app.tick().await.unwrap();
    assert_eq!(
        app.changed.borrow().jobs[&id].outcome,
        Some(Outcome::Failed)
    );
    assert!(!app.changed.borrow().jobs[&id].active());
    assert!(
        app.changed.borrow().jobs[&id]
            .failure_error
            .as_deref()
            .unwrap()
            .contains("QUOTA_EXCEEDED")
    );
}

#[tokio::test]
async fn worker_router_is_ip_bound_and_never_exposes_operator_routes() {
    let fixture = Fixture::new().await;
    let app = fixture.app();
    let id = running(&app).await;
    let router = control_tower::http::worker(app.clone());
    let response = router
        .clone()
        .oneshot(request("/api/jobs", "GET", "127.0.0.1:1234", Body::empty()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let mut wrong = request(
        &format!("/jobs/{id}/complete"),
        "POST",
        "127.0.0.2:1234",
        Body::from(r#"{"outcome":"completed"}"#),
    );
    wrong
        .headers_mut()
        .insert("x-forwarded-for", "127.0.0.1".parse().unwrap());
    assert_eq!(
        router.clone().oneshot(wrong).await.unwrap().status(),
        StatusCode::CONFLICT
    );
    assert!(app.changed.borrow().jobs[&id].running());
    let response = router
        .oneshot(request(
            &format!("/jobs/{id}/complete"),
            "POST",
            "127.0.0.1:1234",
            Body::from(r#"{"outcome":"completed"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn operator_router_rejects_browser_cross_origin_and_rebinding() {
    let fixture = Fixture::new().await;
    let router = control_tower::http::operator(fixture.app());
    for (header, value) in [
        ("origin", "https://evil.test"),
        ("host", "evil.test"),
        ("sec-fetch-site", "cross-site"),
    ] {
        let mut req = request("/api/jobs", "GET", "127.0.0.1:1234", Body::empty());
        req.headers_mut().insert(header, value.parse().unwrap());
        assert_eq!(
            router.clone().oneshot(req).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        router
            .oneshot(request("/api/jobs", "GET", "127.0.0.1:1234", Body::empty()))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn uploads_are_bounded_atomic_idempotent_and_expire_oldest() {
    let fixture = Fixture::new().await;
    let app = fixture.app();
    let id = running(&app).await;
    let router = control_tower::http::worker(app.clone());
    let path = format!("/jobs/{id}/artifact");
    assert_eq!(
        router
            .clone()
            .oneshot(request(
                &path,
                "POST",
                "127.0.0.1:1234",
                Body::from(vec![0; 33])
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    let bytes = vec![0x28, 0xb5, 0x2f, 0xfd, 1, 2, 3];
    assert_eq!(
        router
            .clone()
            .oneshot(request(
                &path,
                "POST",
                "127.0.0.1:1234",
                Body::from(bytes.clone())
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        router
            .oneshot(request(
                &path,
                "POST",
                "127.0.0.1:1234",
                Body::from(bytes.clone())
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    let file = app.store.lock().await.artifact_path(id);
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
    app.finish(id, None, Outcome::Cancelled).await.unwrap();
    app.tick().await.unwrap();
    let mut next = (**app.config.read().await).clone();
    next.limits.artifact_budget_bytes = 4;
    *app.config.write().await = Arc::new(next);
    app.retain().await.unwrap();
    assert!(!file.exists());
    assert!(app.changed.borrow().jobs[&id].artifact.is_none());
}

#[tokio::test]
async fn config_snapshots_and_data_directory_lock_survive_reload_and_restart() {
    let fixture = Fixture::new().await;
    let app = fixture.app();
    let id = app.submit(submission()).await.unwrap().submission.id;
    assert!(
        App::open(
            fixture.config.clone(),
            google_cloud_auth::credentials::anonymous::Builder::new().build()
        )
        .is_err()
    );
    let mut next = fixture.config.clone();
    next.worker_profiles
        .get_mut("windows")
        .unwrap()
        .observatory_port = 9999;
    *app.config.write().await = Arc::new(next.clone());
    let second = app.submit(submission()).await.unwrap().submission.id;
    assert_eq!(
        app.changed.borrow().jobs[&id]
            .snapshot
            .worker_profile
            .observatory_port,
        5678
    );
    assert_eq!(
        app.changed.borrow().jobs[&second]
            .snapshot
            .worker_profile
            .observatory_port,
        9999
    );
    drop(app);
    let restored = App::open(
        next,
        google_cloud_auth::credentials::anonymous::Builder::new().build(),
    )
    .unwrap();
    assert_eq!(restored.changed.borrow().jobs.len(), 2);
}

#[test]
fn config_rejects_exposed_operator_wildcard_worker_and_unknown_fields() {
    let source = include_str!("../config.example.yaml");
    let mut config: Config = serde_yaml::from_str(source).unwrap();
    config.operator_addr = "0.0.0.0:8090".parse().unwrap();
    assert!(config.validate().is_err());
    config.operator_addr = "127.0.0.1:8090".parse().unwrap();
    config.worker_addr = "0.0.0.0:8091".parse().unwrap();
    assert!(config.validate().is_err());
    assert!(serde_yaml::from_str::<Config>(&format!("{source}\nunknown: true")).is_err());
    assert!(
        serde_yaml::from_str::<Config>(&format!("{source}\nworker_url: http://127.0.0.1:8091"))
            .is_err()
    );
    assert!(
        serde_yaml::from_str::<Config>(&source.replace("worker_profiles:", "images:")).is_err()
    );
}

#[tokio::test]
async fn callback_url_is_derived_from_the_listener_for_ipv4_and_ipv6() {
    for address in ["127.0.0.1:8091", "[::1]:8091"] {
        let mut fixture = Fixture::new().await;
        fixture.config.worker_addr = address.parse().unwrap();
        let app = fixture.app();
        let job = app.submit(submission()).await.unwrap();
        assert_eq!(
            job.metadata()["control-tower-worker-url"],
            format!("http://{address}")
        );
    }
}

#[tokio::test]
async fn sdk_unknown_or_missing_instance_status_does_not_mark_a_running_worker_lost() {
    for status in [Some("FUTURE_STATUS"), None] {
        let fixture = Fixture::new().await;
        let app = fixture.app();
        let id = running(&app).await;
        {
            let mut fake = fixture.fake.lock().unwrap();
            let instance = fake.instance.as_mut().unwrap().as_object_mut().unwrap();
            if let Some(status) = status {
                instance.insert("status".into(), json!(status));
            } else {
                instance.remove("status");
            }
        }
        app.tick().await.unwrap();
        let job = app.changed.borrow().jobs[&id].clone();
        assert!(job.running());
        assert!(job.last_error.as_deref().unwrap().contains("status"));
        assert!(fixture.fake.lock().unwrap().deleted.is_empty());
    }
}

#[tokio::test]
async fn sdk_request_timeout_returns_control_without_losing_the_worker() {
    let mut fixture = Fixture::new().await;
    fixture.config.limits.request_timeout_secs = 1;
    let app = fixture.app();
    let id = running(&app).await;
    fixture.fake.lock().unwrap().lookup_delay = true;
    tokio::time::timeout(std::time::Duration::from_millis(1500), app.tick())
        .await
        .unwrap()
        .unwrap();
    let job = app.changed.borrow().jobs[&id].clone();
    assert!(job.running());
    assert!(job.last_error.is_some());
    assert!(fixture.fake.lock().unwrap().deleted.is_empty());
}

#[tokio::test]
async fn sdk_recovery_retains_pending_and_future_operation_statuses() {
    let fixture = Fixture::new().await;
    fixture.fake.lock().unwrap().pending_insert = true;
    let app = fixture.app();
    let id = app.submit(submission()).await.unwrap().submission.id;
    app.tick().await.unwrap();
    app.tick().await.unwrap();
    drop(app);
    let app = fixture.app();
    app.tick().await.unwrap();
    assert!(matches!(
        app.changed.borrow().jobs[&id].phase.work(),
        Phase::Provisioning { operation: Some(_) }
    ));
    assert!(fixture.fake.lock().unwrap().registrations.is_empty());
    fixture.fake.lock().unwrap().operation_override =
        Some(json!({"name":"insert-1","status":"FUTURE_STATUS"}));
    app.tick().await.unwrap();
    assert!(matches!(
        app.changed.borrow().jobs[&id].phase.work(),
        Phase::Provisioning { operation: Some(_) }
    ));
    assert!(
        app.changed.borrow().jobs[&id]
            .last_error
            .as_deref()
            .unwrap()
            .contains("unsupported GCE operation status")
    );
    assert!(app.changed.borrow().jobs[&id].outcome.is_none());
    let calls = fixture.fake.lock().unwrap().operation_count;
    app.tick().await.unwrap();
    assert_eq!(fixture.fake.lock().unwrap().operation_count, calls);
    {
        let mut fake = fixture.fake.lock().unwrap();
        fake.operation_override = None;
        fake.pending_insert = false;
    }
    app.retry(id).await.unwrap();
    app.tick().await.unwrap();
    app.tick().await.unwrap();
    assert!(app.changed.borrow().jobs[&id].running());
    assert_eq!(fixture.fake.lock().unwrap().inserted.len(), 1);
}

#[tokio::test]
async fn sdk_missing_or_http_error_operation_never_reports_success() {
    for operation in [
        json!({"name":"insert-1"}),
        json!({"name":"insert-1","status":"DONE","httpErrorStatusCode":500,"httpErrorMessage":"backend failure"}),
    ] {
        let fixture = Fixture::new().await;
        let app = fixture.app();
        let id = app.submit(submission()).await.unwrap().submission.id;
        app.tick().await.unwrap();
        app.tick().await.unwrap();
        fixture.fake.lock().unwrap().operation_override = Some(operation);
        app.tick().await.unwrap();
        let job = app.changed.borrow().jobs[&id].clone();
        assert!(!job.running());
        assert!(job.last_error.is_some());
        assert!(fixture.fake.lock().unwrap().registrations.is_empty());
    }
}

#[tokio::test]
async fn sdk_unknown_operation_during_deletion_stays_pending_until_resolved() {
    let fixture = Fixture::new().await;
    let app = fixture.app();
    let id = running(&app).await;
    fixture.fake.lock().unwrap().pending_delete = true;
    app.finish(id, None, Outcome::Cancelled).await.unwrap();
    app.tick().await.unwrap();
    fixture.fake.lock().unwrap().operation_override =
        Some(json!({"name":"delete-1","status":"FUTURE_STATUS"}));
    app.tick().await.unwrap();
    assert!(app.changed.borrow().jobs[&id].active());
    assert_eq!(
        app.changed.borrow().jobs[&id].outcome,
        Some(Outcome::Cancelled)
    );
    assert!(
        matches!(app.changed.borrow().jobs[&id].phase.work(),Phase::Cleaning{operation:Some(name),..} if name=="delete-1")
    );
    let calls = fixture.fake.lock().unwrap().operation_count;
    app.tick().await.unwrap();
    assert_eq!(fixture.fake.lock().unwrap().operation_count, calls);
    {
        let mut fake = fixture.fake.lock().unwrap();
        fake.operation_override = None;
        fake.pending_delete = false;
        fake.instance = None;
    }
    app.retry(id).await.unwrap();
    app.tick().await.unwrap();
    assert!(!app.changed.borrow().jobs[&id].active());
}

#[test]
fn version_one_ledger_migrates_without_losing_resource_or_callback_identity() {
    for (phase, outcome) in [
        (json!({"kind":"running"}), Value::Null),
        (
            json!({"kind":"provisioning","operation":"insert-1"}),
            Value::Null,
        ),
        (
            json!({"kind":"cleaning","unregister":true,"delete":true,"operation":"delete-1"}),
            json!("cancelled"),
        ),
    ] {
        let temp = Temp::new();
        std::fs::create_dir_all(&temp.0).unwrap();
        let mut legacy: Value =
            serde_json::from_str(include_str!("fixtures/ledger-v1.json")).unwrap();
        let id = "11111111-1111-4111-8111-111111111111";
        legacy["jobs"][id]["phase"] = phase.clone();
        legacy["jobs"][id]["outcome"] = outcome;
        let path = temp.0.join("jobs.json");
        std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        let store = control_tower::storage::Store::open(&temp.0).unwrap();
        let job = &store.ledger.jobs[&id.parse().unwrap()];
        assert_eq!(store.ledger.version, 3);
        assert_eq!(job.submission.worker_profile, "windows");
        assert_eq!(job.snapshot.worker_profile.metadata["custom"], "retained");
        assert_eq!(
            job.snapshot.cloud.endpoint,
            "https://compute.googleapis.com"
        );
        assert_eq!(
            job.snapshot.worker_url,
            "http://legacy-controller.internal:8091"
        );
        assert_eq!(job.instance_id.as_deref(), Some("123456"));
        assert_eq!(serde_json::to_value(&job.phase).unwrap(), phase);
        assert_eq!(job.artifact.as_ref().unwrap().bytes, 5);
        assert!(
            store
                .ledger
                .retired_ids
                .contains(&"22222222-2222-4222-8222-222222222222".parse().unwrap())
        );
        drop(store);
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["version"], 3);
        assert!(saved["jobs"][id]["submission"].get("image").is_none());
        let reopened = control_tower::storage::Store::open(&temp.0).unwrap();
        assert_eq!(reopened.ledger.jobs.len(), 1);
    }
}

#[test]
fn invalid_legacy_ledger_is_not_rewritten() {
    let temp = Temp::new();
    std::fs::create_dir_all(&temp.0).unwrap();
    let mut legacy: Value = serde_json::from_str(include_str!("fixtures/ledger-v1.json")).unwrap();
    legacy["jobs"]["11111111-1111-4111-8111-111111111111"]["instance_id"] = json!("invalid");
    let path = temp.0.join("jobs.json");
    let bytes = serde_json::to_vec(&legacy).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    assert!(control_tower::storage::Store::open(&temp.0).is_err());
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[test]
fn malformed_legacy_job_fails_without_panicking_or_rewriting() {
    let temp = Temp::new();
    std::fs::create_dir_all(&temp.0).unwrap();
    let path = temp.0.join("jobs.json");
    let bytes = br#"{"version":1,"jobs":{"bad":7},"retired_ids":[]}"#;
    std::fs::write(&path, bytes).unwrap();
    assert!(control_tower::storage::Store::open(&temp.0).is_err());
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[tokio::test]
async fn failed_delete_operation_blocks_and_explicit_retry_reissues_instead_of_polling_forever() {
    let fixture = Fixture::new().await;
    let app = fixture.app();
    let id = running(&app).await;
    fixture.fake.lock().unwrap().pending_delete = true;
    app.finish(id, None, Outcome::Cancelled).await.unwrap();
    app.tick().await.unwrap();
    {
        let mut fake = fixture.fake.lock().unwrap();
        fake.pending_delete = false;
        fake.delete_error = true;
    }
    app.tick().await.unwrap();
    assert!(matches!(
        app.changed.borrow().jobs[&id].phase.work(),
        Phase::Cleaning {
            operation: None,
            ..
        }
    ));
    fixture.fake.lock().unwrap().delete_error = false;
    assert!(matches!(
        app.changed.borrow().jobs[&id].phase,
        Phase::Blocked { .. }
    ));
    app.retry(id).await.unwrap();
    app.tick().await.unwrap();
    assert!(!app.changed.borrow().jobs[&id].active());
    assert_eq!(fixture.fake.lock().unwrap().deleted.len(), 2);
}

#[tokio::test]
async fn disappearing_registering_worker_never_grants_its_ip_credentials() {
    let fixture = Fixture::new().await;
    let app = fixture.app();
    let id = app.submit(submission()).await.unwrap().submission.id;
    app.tick().await.unwrap();
    app.tick().await.unwrap();
    app.tick().await.unwrap();
    fixture.fake.lock().unwrap().instance = None;
    app.tick().await.unwrap();
    assert_eq!(app.changed.borrow().jobs[&id].outcome, Some(Outcome::Lost));
    assert!(fixture.fake.lock().unwrap().registrations.is_empty());
}

#[tokio::test]
async fn ip_reuse_retires_the_old_job_before_accepting_new_worker_traffic() {
    let fixture = Fixture::new().await;
    let app = fixture.app();
    let old = running(&app).await;
    fixture.fake.lock().unwrap().lookup_status = Some(StatusCode::FORBIDDEN);
    app.tick().await.unwrap();
    assert!(matches!(
        app.changed.borrow().jobs[&old].phase,
        Phase::Blocked { .. }
    ));
    fixture.fake.lock().unwrap().lookup_status = None;
    // The old GCE incarnation is gone; the replacement receives its address.
    fixture.fake.lock().unwrap().instance = None;
    let new = app.submit(submission()).await.unwrap().submission.id;
    for _ in 0..4 {
        app.tick().await.unwrap();
    }
    assert!(!app.changed.borrow().jobs[&old].running());
    assert!(app.changed.borrow().jobs[&new].running());
    assert!(
        app.finish(old, Some("127.0.0.1".parse().unwrap()), Outcome::Completed)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cancelled_upload_discards_partial_bytes_and_never_commits_a_packet() {
    let fixture = Fixture::new().await;
    let app = fixture.app();
    let id = running(&app).await;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(2);
    let body = Body::from_stream(futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx)));
    let router = control_tower::http::worker(app.clone());
    let upload = tokio::spawn(router.oneshot(request(
        &format!("/jobs/{id}/artifact"),
        "POST",
        "127.0.0.1:1234",
        body,
    )));
    tx.send(Ok(vec![0x28, 0xb5, 0x2f, 0xfd].into()))
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if app.uploads.lock().unwrap().contains_key(&id) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        app.finish(id, Some("127.0.0.1".parse().unwrap()), Outcome::Completed)
            .await
            .is_err()
    );
    app.finish(id, None, Outcome::Cancelled).await.unwrap();
    drop(tx);
    assert_eq!(
        upload.await.unwrap().unwrap().status(),
        StatusCode::CONFLICT
    );
    assert!(app.uploads.lock().unwrap().is_empty());
    assert!(!app.store.lock().await.upload_path(id).exists());
    assert!(app.changed.borrow().jobs[&id].artifact.is_none());
}

#[tokio::test]
async fn history_expiration_does_not_allow_submission_identity_reuse() {
    let fixture = Fixture::new().await;
    let app = fixture.app();
    let input = submission();
    let id = input.id;
    app.submit(input.clone()).await.unwrap();
    for _ in 0..4 {
        app.tick().await.unwrap();
    }
    app.finish(id, None, Outcome::Cancelled).await.unwrap();
    app.tick().await.unwrap();
    let next = running(&app).await;
    app.finish(next, None, Outcome::Cancelled).await.unwrap();
    app.tick().await.unwrap();
    {
        let mut config = (**app.config.read().await).clone();
        config.limits.max_finished_jobs = 1;
        *app.config.write().await = Arc::new(config);
    }
    app.retain().await.unwrap();
    let expired = app
        .changed
        .borrow()
        .retired_ids
        .iter()
        .next()
        .copied()
        .unwrap();
    let mut input = input;
    input.id = expired;
    assert!(app.submit(input).await.is_err());
}

#[tokio::test]
async fn cancelling_a_queued_job_does_not_provision_a_vm() {
    let fixture = Fixture::new().await;
    let app = fixture.app();
    let id = app.submit(submission()).await.unwrap().submission.id;
    app.finish(id, None, Outcome::Cancelled).await.unwrap();
    app.tick().await.unwrap();
    assert!(!app.changed.borrow().jobs[&id].active());
    assert!(fixture.fake.lock().unwrap().inserted.is_empty());
}

#[tokio::test]
async fn vanished_provisioned_worker_is_not_recreated_for_the_same_job() {
    let fixture = Fixture::new().await;
    let app = fixture.app();
    let id = app.submit(submission()).await.unwrap().submission.id;
    app.tick().await.unwrap();
    app.tick().await.unwrap();
    fixture.fake.lock().unwrap().instance = None;
    app.tick().await.unwrap();
    assert_eq!(app.changed.borrow().jobs[&id].outcome, Some(Outcome::Lost));
    assert_eq!(fixture.fake.lock().unwrap().inserted.len(), 1);
}

#[tokio::test]
async fn pending_upload_times_out_and_releases_its_file_and_reservation() {
    let mut fixture = Fixture::new().await;
    fixture.config.limits.upload_timeout_secs = 1;
    let app = fixture.app();
    let id = running(&app).await;
    let body = Body::from_stream(futures_util::stream::pending::<
        Result<axum::body::Bytes, std::io::Error>,
    >());
    let response = control_tower::http::worker(app.clone())
        .oneshot(request(
            &format!("/jobs/{id}/artifact"),
            "POST",
            "127.0.0.1:1234",
            body,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(app.uploads.lock().unwrap().is_empty());
    assert!(!app.store.lock().await.upload_path(id).exists());
}

#[tokio::test]
async fn upload_admission_evicts_old_finished_packets_to_make_room() {
    let mut fixture = Fixture::new().await;
    fixture.config.limits.artifact_budget_bytes = 32;
    let app = fixture.app();
    let old = running(&app).await;
    let router = control_tower::http::worker(app.clone());
    let bytes = vec![0x28, 0xb5, 0x2f, 0xfd, 1];
    assert_eq!(
        router
            .clone()
            .oneshot(request(
                &format!("/jobs/{old}/artifact"),
                "POST",
                "127.0.0.1:1234",
                Body::from(bytes.clone())
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::CREATED
    );
    app.finish(old, None, Outcome::Cancelled).await.unwrap();
    app.tick().await.unwrap();
    let new = running(&app).await;
    assert_eq!(
        router
            .oneshot(request(
                &format!("/jobs/{new}/artifact"),
                "POST",
                "127.0.0.1:1234",
                Body::from(bytes)
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::CREATED
    );
    assert!(app.changed.borrow().jobs[&old].artifact.is_none());
    assert!(app.changed.borrow().jobs[&new].artifact.is_some());
}

#[tokio::test]
async fn yaml_replacement_publishes_valid_snapshots_and_preserves_rejected_reload_error() {
    let fixture = Fixture::new().await;
    let app = fixture.app();
    let path = fixture._temp.0.join("config.yaml");
    // Exercise replacement through the service's deployment parser and reload loop.
    let source = include_str!("../config.example.yaml");
    let mut document: serde_yaml::Value = serde_yaml::from_str(source).unwrap();
    document["data_dir"] =
        serde_yaml::Value::String(fixture.config.data_dir.to_string_lossy().into());
    document["ui_dir"] = serde_yaml::Value::String(fixture.config.ui_dir.to_string_lossy().into());
    std::fs::write(&path, serde_yaml::to_string(&document).unwrap()).unwrap();
    let cancel = tokio_util::sync::CancellationToken::new();
    let task = tokio::spawn(app.clone().run(path.clone(), cancel.clone()));
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    std::fs::write(&path, "invalid: [").unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    assert!(app.reload_error.read().await.is_some());
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert!(app.reload_error.read().await.is_some());
    document["limits"]["max_jobs"] = serde_yaml::Value::Number(3.into());
    let temp = path.with_extension("replacement");
    std::fs::write(&temp, serde_yaml::to_string(&document).unwrap()).unwrap();
    std::fs::rename(temp, path).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    assert_eq!(app.config.read().await.limits.max_jobs, 3);
    assert!(app.reload_error.read().await.is_none());
    cancel.cancel();
    task.await.unwrap().unwrap();
}
