use crate::{
    controller::App,
    model::{Artifact, Outcome, Submission, now},
};
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    body::Body,
    extract::{
        ConnectInfo, Path, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use std::{net::SocketAddr, sync::Arc, sync::atomic::Ordering, time::Duration};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;
use uuid::Uuid;

type ApiResult<T> = std::result::Result<T, ApiError>;
fn require(condition: bool, message: &str) -> Result<()> {
    ensure!(condition, "{message}");
    Ok(())
}
pub struct ApiError(anyhow::Error);
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self(e)
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            StatusCode::CONFLICT,
            Json(json!({"error":format!("{:#}",self.0)})),
        )
            .into_response()
    }
}

pub fn operator(app: Arc<App>) -> Router {
    Router::new()
        .route("/api/jobs", get(list).post(submit))
        .route("/api/jobs/{id}/cancel", post(cancel))
        .route("/api/jobs/{id}/delete", post(delete))
        .route("/api/jobs/{id}/retry", post(retry))
        .route("/api/jobs/{id}/artifact", get(download))
        .route("/api/jobs/{id}/observatory", get(relay))
        .fallback(get(asset))
        .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024))
        .layer(axum::middleware::from_fn(operator_boundary))
        .with_state(app)
}

pub fn worker(app: Arc<App>) -> Router {
    Router::new()
        .route("/jobs/{id}/artifact", post(upload))
        .route("/jobs/{id}/complete", post(complete))
        .layer(axum::extract::DefaultBodyLimit::max(4096))
        .with_state(app)
}

async fn operator_boundary(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let allowed = (|| -> Result<()> {
        let host = request
            .headers()
            .get(header::HOST)
            .context("Host required")?
            .to_str()?;
        let authority = host.parse::<axum::http::uri::Authority>()?;
        let host_name = authority.host().trim_matches(['[', ']']);
        ensure!(
            host_name == "localhost"
                || host_name
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback()),
            "operator Host must be loopback"
        );
        if let Some(origin) = request.headers().get(header::ORIGIN) {
            let origin = reqwest::Url::parse(origin.to_str()?)?;
            let expected = reqwest::Url::parse(&format!("http://{host}"))?;
            ensure!(
                origin.origin() == expected.origin(),
                "cross-origin operator request denied"
            );
        }
        if request
            .headers()
            .get("sec-fetch-site")
            .is_some_and(|v| v == "cross-site")
        {
            anyhow::bail!("cross-site operator request denied");
        }
        if !matches!(
            *request.method(),
            axum::http::Method::GET | axum::http::Method::HEAD
        ) {
            ensure!(
                request
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .is_some_and(|v| v == "application/json"),
                "operator mutations require application/json"
            );
        }
        Ok(())
    })();
    if let Err(e) = allowed {
        return (StatusCode::FORBIDDEN, e.to_string()).into_response();
    }
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    response
        .headers_mut()
        .insert(header::X_FRAME_OPTIONS, "DENY".parse().unwrap());
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        "frame-ancestors 'none'".parse().unwrap(),
    );
    response
}

async fn list(State(app): State<Arc<App>>) -> Json<serde_json::Value> {
    let config = app.config.read().await.clone();
    let ledger = app.changed.borrow().clone();
    Json(
        json!({"jobs":ledger.jobs.values().collect::<Vec<_>>(),"worker_profiles":config.worker_profiles.keys().collect::<Vec<_>>(),"reload_error":*app.reload_error.read().await}),
    )
}
async fn submit(
    State(app): State<Arc<App>>,
    Json(submission): Json<Submission>,
) -> ApiResult<impl IntoResponse> {
    Ok((StatusCode::ACCEPTED, Json(app.submit(submission).await?)))
}
async fn cancel(
    State(app): State<Arc<App>>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<crate::model::Job>> {
    Ok(Json(app.finish(id, None, Outcome::Cancelled).await?))
}
async fn delete(
    State(app): State<Arc<App>>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    app.delete(id).await?;
    Ok(Json(json!({"id": id})))
}
async fn retry(
    State(app): State<Arc<App>>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<crate::model::Job>> {
    Ok(Json(app.retry(id).await?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Completion {
    outcome: CompletionOutcome,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum CompletionOutcome {
    Completed,
    Failed,
}
async fn complete(
    State(app): State<Arc<App>>,
    Path(id): Path<Uuid>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<Completion>,
) -> ApiResult<impl IntoResponse> {
    let outcome = match body.outcome {
        CompletionOutcome::Completed => Outcome::Completed,
        CompletionOutcome::Failed => Outcome::Failed,
    };
    app.finish(id, Some(peer.ip()), outcome).await?;
    Ok(StatusCode::ACCEPTED)
}

struct UploadGuard {
    app: Arc<App>,
    id: Uuid,
    path: std::path::PathBuf,
}
impl Drop for UploadGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        self.app.uploads.lock().unwrap().remove(&self.id);
    }
}

async fn upload(
    State(app): State<Arc<App>>,
    Path(id): Path<Uuid>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: axum::extract::Request,
) -> ApiResult<impl IntoResponse> {
    let config = app.config.read().await.clone();
    let path = {
        let mut store = app.store.lock().await;
        let job = store.ledger.jobs.get(&id).context("unknown job")?;
        require(
            job.running() && job.ip == Some(peer.ip()),
            "source IP does not own a running job",
        )?;
        if job.artifact.is_some() {
            return Ok(StatusCode::NO_CONTENT);
        }
        let mut uploads = app.uploads.lock().unwrap();
        require(!uploads.contains_key(&id), "upload already in progress")?;
        let reserved = uploads
            .values()
            .try_fold(config.limits.max_artifact_bytes, |sum, bytes| {
                sum.checked_add(*bytes)
            })
            .context("upload reservations overflow")?;
        require(
            reserved <= config.limits.artifact_budget_bytes,
            "artifact budget exhausted by active uploads",
        )?;
        let mut next = store.ledger.clone();
        let available = config.limits.artifact_budget_bytes - reserved;
        let retained = store.expire_artifacts(&mut next, available)?;
        store.commit(next)?;
        app.publish(&store);
        require(
            retained <= available,
            "artifact budget exhausted; retry after retention",
        )?;
        uploads.insert(id, config.limits.max_artifact_bytes);
        store.upload_path(id)
    };
    let guard = UploadGuard {
        app: app.clone(),
        id,
        path: path.clone(),
    };
    let result = tokio::time::timeout(
        Duration::from_secs(config.limits.upload_timeout_secs),
        async {
            let mut options = tokio::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                options.mode(0o600);
            }
            let mut file = options.open(&path).await?;
            let mut stream = request.into_body().into_data_stream();
            let mut bytes = 0u64;
            let mut prefix: Vec<u8> = Vec::new();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                bytes = bytes
                    .checked_add(chunk.len() as u64)
                    .context("upload size overflow")?;
                ensure!(
                    bytes <= config.limits.max_artifact_bytes,
                    "artifact exceeds upload limit"
                );
                if prefix.len() < 4 {
                    prefix.extend(chunk.iter().take(4 - prefix.len()));
                }
                file.write_all(&chunk).await?;
            }
            ensure!(prefix == [0x28, 0xb5, 0x2f, 0xfd], "expected a zstd frame");
            file.sync_all().await?;
            drop(file);
            let mut store = app.store.lock().await;
            let job = store
                .ledger
                .jobs
                .get(&id)
                .context("job removed during upload")?;
            ensure!(
                job.running() && job.ip == Some(peer.ip()) && job.artifact.is_none(),
                "job changed during upload"
            );
            let destination = store.artifact_path(id);
            tokio::fs::rename(&path, &destination).await?;
            #[cfg(unix)]
            std::fs::File::open(destination.parent().unwrap())?.sync_all()?;
            let mut next = store.ledger.clone();
            next.jobs.get_mut(&id).unwrap().artifact = Some(Artifact { bytes, at: now() });
            store.commit(next)?;
            app.publish(&store);
            Ok::<(), anyhow::Error>(())
        },
    )
    .await
    .context("artifact upload timed out")?;
    drop(guard);
    result?;
    Ok(StatusCode::CREATED)
}

async fn download(State(app): State<Arc<App>>, Path(id): Path<Uuid>) -> ApiResult<Response> {
    let store = app.store.lock().await;
    require(
        store
            .ledger
            .jobs
            .get(&id)
            .is_some_and(|j| j.artifact.is_some()),
        "artifact unavailable or expired",
    )?;
    let file = tokio::fs::File::open(store.artifact_path(id))
        .await
        .map_err(anyhow::Error::from)?;
    let bytes = file.metadata().await.map_err(anyhow::Error::from)?.len();
    drop(store);
    Ok((
        [
            (header::CONTENT_TYPE, "application/zstd".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{id}.zst\""),
            ),
            (header::CONTENT_LENGTH, bytes.to_string()),
        ],
        Body::from_stream(ReaderStream::new(file)),
    )
        .into_response())
}

struct RelayGuard(Arc<App>);
impl Drop for RelayGuard {
    fn drop(&mut self) {
        self.0.relay_count.fetch_sub(1, Ordering::Relaxed);
    }
}

async fn relay(
    State(app): State<Arc<App>>,
    Path(id): Path<Uuid>,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    let limits = app.config.read().await.limits.clone();
    let job = app
        .changed
        .borrow()
        .jobs
        .get(&id)
        .context("unknown job")?
        .clone();
    require(job.running(), "job is not running")?;
    let count = app.relay_count.fetch_add(1, Ordering::Relaxed);
    let guard = RelayGuard(app.clone());
    require(count < limits.max_relays, "relay limit reached")?;
    let address = SocketAddr::new(
        job.ip.context("worker has no IP")?,
        job.snapshot.worker_profile.observatory_port,
    );
    let duration = Duration::from_secs(job.snapshot.request_timeout_secs);
    let mut websocket_config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default();
    websocket_config.max_message_size = Some(limits.max_relay_message_bytes);
    websocket_config.max_frame_size = Some(limits.max_relay_message_bytes);
    let (upstream, _) = tokio::time::timeout(
        duration,
        tokio_tungstenite::connect_async_with_config(
            format!("ws://{address}"),
            Some(websocket_config),
            false,
        ),
    )
    .await
    .context("observatory connection timed out")?
    .map_err(anyhow::Error::from)?;
    Ok(ws
        .max_message_size(limits.max_relay_message_bytes)
        .max_frame_size(limits.max_relay_message_bytes)
        .on_upgrade(move |socket| async move {
            if let Err(e) = relay_stream(socket, upstream, app, id, duration).await {
                eprintln!("relay {id}: {e:#}");
            }
            drop(guard);
        }))
}

async fn relay_stream(
    mut browser: WebSocket,
    mut worker: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    app: Arc<App>,
    id: Uuid,
    timeout: Duration,
) -> Result<()> {
    let mut changes = app.changed.subscribe();
    loop {
        ensure!(
            changes.borrow().jobs.get(&id).is_some_and(|j| j.running()),
            "worker no longer running"
        );
        tokio::select! {
            result=changes.changed()=>{result?;},
            incoming=browser.next()=>{
                let Some(message)=incoming else{break};
                match message? {
                    Message::Text(text)=>{
                        let value:serde_json::Value=serde_json::from_str(&text)?;
                        ensure!(value==json!("takeScreenshot") || value.as_object().is_some_and(|v|v.len()==1 && v.get("fetchScreenshot").is_some_and(|id|id.as_str().is_some_and(|id|id.len()<=128))),"unsupported observatory command");
                        tokio::time::timeout(timeout,worker.send(tokio_tungstenite::tungstenite::Message::Text(text.to_string().into()))).await??;
                    }
                    Message::Close(_)=>break,
                    Message::Ping(data)=>tokio::time::timeout(timeout,browser.send(Message::Pong(data))).await??,
                    Message::Pong(_)=>{},
                    Message::Binary(_)=>anyhow::bail!("binary observatory commands denied"),
                }
            }
            incoming=worker.next()=>{
                let Some(message)=incoming else{break};
                let message=match message?{
                    tokio_tungstenite::tungstenite::Message::Text(text)=>Message::Text(text.to_string().into()),
                    tokio_tungstenite::tungstenite::Message::Binary(data)=>Message::Binary(data),
                    tokio_tungstenite::tungstenite::Message::Close(_)=>break,
                    _=>continue,
                };
                tokio::time::timeout(timeout,browser.send(message)).await??;
            }
        }
    }
    Ok(())
}

async fn asset(State(app): State<Arc<App>>, request: axum::extract::Request) -> Response {
    let path = request.uri().path().to_owned();
    let result = async {
        let root = std::fs::canonicalize(&app.config.read().await.ui_dir)?;
        if path == "/" {
            return Ok(axum::response::Redirect::temporary("/tower").into_response());
        }
        let relative = if path == "/tower" || path == "/tower/" {
            "tower.html"
        } else {
            path.trim_start_matches('/')
        };
        ensure!(
            !relative.split('/').any(|s| s == ".." || s.contains('\\')),
            "invalid asset path"
        );
        let file = std::fs::canonicalize(root.join(relative))?;
        ensure!(
            file.starts_with(&root) && file.is_file(),
            "asset outside UI directory"
        );
        let content_type = match file.extension().and_then(|e| e.to_str()) {
            Some("html") => "text/html; charset=utf-8",
            Some("js") => "text/javascript",
            Some("css") => "text/css",
            Some("svg") => "image/svg+xml",
            Some("png") => "image/png",
            _ => "application/octet-stream",
        };
        let file = tokio::fs::File::open(file).await?;
        Ok::<_, anyhow::Error>(
            (
                [(header::CONTENT_TYPE, content_type)],
                Body::from_stream(ReaderStream::new(file)),
            )
                .into_response(),
        )
    }
    .await;
    result.unwrap_or_else(|_| {
        (
            StatusCode::NOT_FOUND,
            "UI asset not found; build the Control Tower UI and check ui_dir",
        )
            .into_response()
    })
}
