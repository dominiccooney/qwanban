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
}
type FakeState = Arc<Mutex<Fake>>;

async fn insert(
    State(state): State<FakeState>,
    Query(query): Query<std::collections::HashMap<String, String>>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let mut state = state.lock().unwrap();
    state
        .request_ids
        .push(query.get("requestId").unwrap().clone());
    state.inserted.push(body.clone());
    if !state.insert_error {
        state.instance = Some(
            json!({"id":(123455+state.inserted.len()).to_string(),"name":body["name"],"status":"RUNNING","labels":body["labels"],"networkInterfaces":[{"networkIP":"127.0.0.1"}]}),
        );
    }
    Json(
        json!({"name":"insert-1","status":if state.pending_insert{"RUNNING"}else{"DONE"},"error":if state.insert_error{json!({"errors":[{"code":"QUOTA_EXCEEDED","message":"quota"}]})}else{Value::Null}}),
    )
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
    let state = state.lock().unwrap();
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
        app.changed.borrow().jobs[&id].phase,
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
        app.changed.borrow().jobs[&id].phase,
        Phase::Registering
    ));
    fixture.fake.lock().unwrap().registration_error = false;
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
    app.tick().await.unwrap();
    assert_eq!(app.changed.borrow().jobs[&id].outcome, Some(Outcome::Lost));
    app.tick().await.unwrap();
    assert!(!app.changed.borrow().jobs[&id].active());
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
        app.changed.borrow().jobs[&id].phase,
        Phase::Provisioning { operation: Some(_) }
    ));
    assert!(fixture.fake.lock().unwrap().registrations.is_empty());
    fixture.fake.lock().unwrap().operation_override =
        Some(json!({"name":"insert-1","status":"FUTURE_STATUS"}));
    app.tick().await.unwrap();
    assert!(matches!(
        app.changed.borrow().jobs[&id].phase,
        Phase::Provisioning { operation: Some(_) }
    ));
    assert!(
        app.changed.borrow().jobs[&id]
            .last_error
            .as_deref()
            .unwrap()
            .contains("unsupported GCE operation status")
    );
    {
        let mut fake = fixture.fake.lock().unwrap();
        fake.operation_override = None;
        fake.pending_insert = false;
    }
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
        assert_eq!(store.ledger.version, 2);
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
        assert_eq!(saved["version"], 2);
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
async fn failed_delete_operation_is_retried_instead_of_polled_forever() {
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
        app.changed.borrow().jobs[&id].phase,
        Phase::Cleaning {
            operation: None,
            ..
        }
    ));
    fixture.fake.lock().unwrap().delete_error = false;
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
