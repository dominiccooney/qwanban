// See https://github.com/anthropics/claude-quickstarts/blob/main/computer-use-demo/computer_use_demo/tools/computer.py
// See https://github.com/anthropics/anthropic-sdk-typescript/blob/4f2eb8071993780d79610b9eda26db96f7653843/src/resources/beta/messages/messages.ts#L3283

use crate::artifacts::ArtifactStore;
use crate::journal::Journal;
use crate::pal::MouseButton;
use crate::pal::{CapturedScreenshot, ForegroundWindow, ScreenshotImage};
use crate::{input, pal};
use base64::Engine;
use futures::stream::FuturesUnordered;
use futures::{SinkExt, StreamExt};
use image::{GenericImageView, ImageFormat};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::{Framed, LinesCodec};
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MouseClickParams {
    id: usize,
    key: Option<String>,
    coordinate: Option<(usize, usize)>,
    /// Click guard: (x, y, width, height) of a region the caller expects to
    /// look unchanged since the last full screenshot they were shown. The
    /// region is compared before the click; if it changed, the click is
    /// aborted and a fresh screenshot is returned instead.
    expect_unchanged: Option<(usize, usize, usize, usize)>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub(crate) enum ComputerUseRequest {
    Key {
        id: usize,
        text: String,
    },
    Type {
        id: usize,
        text: String,
    },
    MouseMove {
        id: usize,
        coordinate: (usize, usize),
    },
    LeftClick(MouseClickParams),
    #[serde(rename_all = "camelCase")]
    LeftClickDrag {
        id: usize,
        start_coordinate: (usize, usize),
        coordinate: (usize, usize),
    },
    RightClick(MouseClickParams),
    MiddleClick(MouseClickParams),
    DoubleClick(MouseClickParams),
    Screenshot {
        id: usize,
    },
    // *Gets* the cursor position
    CursorPosition {
        id: usize,
    },
    LeftMouseDown {
        id: usize,
        coordinate: (usize, usize),
    },
    LeftMouseUp {
        id: usize,
        coordinate: (usize, usize),
    },

    #[serde(rename_all = "camelCase")]
    Scroll {
        id: usize,
        scroll_direction: ScrollDirection,
        scroll_amount: f64,
        coordinate: Option<(usize, usize)>,
    },

    #[serde(rename_all = "camelCase")]
    HoldKey {
        id: usize,
        duration_seconds: f64,
        text: String,
    },

    // Fixed 300 ms pause for transient UI to become ready, then screenshot.
    BriefPause {
        id: usize,
    },

    // Waits -> screenshot
    #[serde(rename_all = "camelCase")]
    Wait {
        id: usize,
        duration_seconds: f64,
    },
    TripleClick(MouseClickParams),

    // Cropped screenshot, x0,y0,x1,y1
    Zoom {
        id: usize,
        region: (usize, usize, usize, usize),
    },

    /// Executes a queue of actions back-to-back and returns one screenshot of
    /// the final state, so a multi-step interaction costs the caller one
    /// round trip instead of one per action. Aborts on the first failing
    /// step; each step is journaled individually.
    #[serde(rename_all = "camelCase")]
    RunSequence {
        id: usize,
        actions: Vec<serde_json::Value>,
    },

    // Not Claude events
    GetDisplayInfo {
        id: usize,
    },

    SaveScreenshot {
        id: usize,
        screenshot_id: String,
        path: PathBuf,
    },
    GetClipboard {
        id: usize,
    },
    SetClipboard {
        id: usize,
        text: String,
    },

    /// The agent publishes one of its own events (transcript message,
    /// coordinator status change, ...) into the journal for the observatory.
    /// `kind` namespaces the event (e.g. "transcript.message"); `payload` is
    /// passed through to observers untouched.
    PublishEvent {
        id: usize,
        kind: String,
        payload: serde_json::Value,
    },
    /// A trusted loopback client asks this qbt process to exit cleanly.
    ShutdownBackend {
        id: usize,
    },
}

impl ComputerUseRequest {
    fn id(&self) -> usize {
        match self {
            ComputerUseRequest::Key { id, .. } => *id,
            ComputerUseRequest::Type { id, .. } => *id,
            ComputerUseRequest::MouseMove { id, .. } => *id,
            ComputerUseRequest::LeftClick(params)
            | ComputerUseRequest::RightClick(params)
            | ComputerUseRequest::MiddleClick(params)
            | ComputerUseRequest::DoubleClick(params)
            | ComputerUseRequest::TripleClick(params) => params.id,
            ComputerUseRequest::LeftClickDrag { id, .. } => *id,
            ComputerUseRequest::Screenshot { id, .. } => *id,
            ComputerUseRequest::CursorPosition { id, .. } => *id,
            ComputerUseRequest::LeftMouseDown { id, .. } => *id,
            ComputerUseRequest::LeftMouseUp { id, .. } => *id,
            ComputerUseRequest::Scroll { id, .. } => *id,
            ComputerUseRequest::HoldKey { id, .. } => *id,
            ComputerUseRequest::BriefPause { id, .. } => *id,
            ComputerUseRequest::Wait { id, .. } => *id,
            ComputerUseRequest::Zoom { id, .. } => *id,
            ComputerUseRequest::RunSequence { id, .. } => *id,
            ComputerUseRequest::GetDisplayInfo { id, .. } => *id,
            ComputerUseRequest::SaveScreenshot { id, .. } => *id,
            ComputerUseRequest::GetClipboard { id } => *id,
            ComputerUseRequest::SetClipboard { id, .. } => *id,
            ComputerUseRequest::PublishEvent { id, .. } => *id,
            ComputerUseRequest::ShutdownBackend { id } => *id,
        }
    }

    fn mouse_clickiness(&self) -> Option<(MouseButton, usize)> {
        match self {
            ComputerUseRequest::LeftClick(_) => Some((MouseButton::Left, 1)),
            ComputerUseRequest::RightClick(_) => Some((MouseButton::Right, 1)),
            ComputerUseRequest::MiddleClick(_) => Some((MouseButton::Middle, 1)),
            ComputerUseRequest::DoubleClick(_) => Some((MouseButton::Left, 2)),
            ComputerUseRequest::TripleClick(_) => Some((MouseButton::Left, 3)),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ComputerUseImage {
    data: String,
    // MIME type, e.g. "image/png"
    media_type: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", untagged)]
pub(crate) enum ComputerUseResponse {
    Error {
        id: usize,
        ok: bool,
        error: String,
    },
    Empty {
        id: usize,
        ok: bool,
    },
    DisplayInfo {
        id: usize,
        ok: bool,
        display: ComputerUseDisplayInfo,
    },
    Text {
        id: usize,
        ok: bool,
        text: String,
    },
    Clipboard {
        id: usize,
        ok: bool,
        text: Option<String>,
    },
    Saved {
        id: usize,
        ok: bool,
        saved: PathBuf,
    },
    Image {
        id: usize,
        ok: bool,
        /// A guard refused the action and any remaining sequence steps.
        aborted: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        image: ComputerUseImage,
        #[serde(rename = "screenshot_id", skip_serializing_if = "Option::is_none")]
        screenshot_id: Option<String>,
        #[serde(rename = "foregroundWindow")]
        foreground_window: Option<ForegroundWindow>,
        /// Only a full-screen image can become the next click-guard reference.
        #[serde(skip)]
        reference_screenshot: Option<ScreenshotImage>,
    },
}

impl ComputerUseResponse {
    fn completed(&self) -> bool {
        match self {
            Self::Error { .. } => false,
            Self::Image { ok, aborted, .. } => *ok && !aborted,
            Self::Empty { ok, .. }
            | Self::DisplayInfo { ok, .. }
            | Self::Text { ok, .. }
            | Self::Clipboard { ok, .. }
            | Self::Saved { ok, .. } => *ok,
        }
    }

    fn set_screenshot_id(&mut self, screenshot_id: Option<String>) {
        if let Self::Image {
            screenshot_id: response_id,
            ..
        } = self
        {
            *response_id = screenshot_id;
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ComputerUseDisplayInfo {
    width_px: usize,
    height_px: usize,
}

/// Serves the agent's JSONL socket. The first frame on each connection is
/// classified before it joins the single-agent action path. A valid shutdown
/// frame is acknowledged out of band, so it cannot wait behind a long action
/// and execute after its caller has timed out. Any other first frame makes the
/// connection the new input owner and promptly cancels the previous owner.
pub(crate) async fn serve_agent(
    listener: TcpListener,
    journal: Arc<Journal>,
    artifacts: Arc<ArtifactStore>,
    shutdown: CancellationToken,
) {
    let mut active_client: Option<(CancellationToken, tokio::task::JoinHandle<()>)> = None;
    let mut incoming = FuturesUnordered::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                match accepted {
                    Ok((socket, peer)) => {
                        eprintln!("agent connected: {}", peer);
                        incoming.push(receive_first_request(socket));
                    }
                    Err(err) => {
                        eprintln!("error accepting agent socket: {}", err);
                    }
                }
            }
            candidate = incoming.next(), if !incoming.is_empty() => {
                let Some(candidate) = candidate else { continue };
                let (mut framed, first_line) = match candidate {
                    Ok(Some(candidate)) => candidate,
                    Ok(None) => continue,
                    Err(error) => {
                        eprintln!("agent connection error: {error}");
                        continue;
                    }
                };
                if let Some(id) = shutdown_request_id(&first_line) {
                    let response = ComputerUseResponse::Empty { id, ok: true };
                    match serde_json::to_string(&response) {
                        Ok(text) => match framed.send(text).await {
                            Ok(()) => shutdown.cancel(),
                            Err(error) => eprintln!("failed to acknowledge shutdown: {error}"),
                        },
                        Err(error) => eprintln!("failed to serialize shutdown response: {error}"),
                    }
                    continue;
                }
                if let Some((cancel, handle)) = active_client.take() {
                    cancel.cancel();
                    let _ = handle.await;
                }
                let cancel = shutdown.child_token();
                let handle = tokio::spawn(handle_agent_client(
                    framed,
                    Some(first_line),
                    journal.clone(),
                    artifacts.clone(),
                    cancel.clone(),
                ));
                active_client = Some((cancel, handle));
            }
            _ = shutdown.cancelled() => break,
        }
    }
    if let Some((cancel, handle)) = active_client.take() {
        cancel.cancel();
        let _ = handle.await;
    }
}

async fn receive_first_request(
    socket: TcpStream,
) -> anyhow::Result<Option<(Framed<TcpStream, LinesCodec>, String)>> {
    let mut framed = Framed::new(socket, LinesCodec::new());
    while let Some(line) = framed.next().await {
        let line = line?;
        if !line.trim().is_empty() {
            return Ok(Some((framed, line)));
        }
    }
    Ok(None)
}

fn shutdown_request_id(line: &str) -> Option<usize> {
    let value = serde_json::from_str(line).ok()?;
    match serde_json::from_value::<ComputerUseRequest>(value).ok()? {
        ComputerUseRequest::ShutdownBackend { id } => Some(id),
        _ => None,
    }
}

async fn handle_agent_client(
    mut framed: Framed<TcpStream, LinesCodec>,
    mut first_line: Option<String>,
    journal: Arc<Journal>,
    artifacts: Arc<ArtifactStore>,
    cancel: CancellationToken,
) {
    let input = Arc::new(input::SyntheticInput::default());
    let mut state = ClientState {
        last_screenshot: None,
        input: input.clone(),
    };
    loop {
        let line = match first_line.take() {
            Some(line) => line,
            None => tokio::select! {
                _ = cancel.cancelled() => break,
                next = framed.next() => match next {
                    None => break,
                    Some(Err(err)) => {
                        eprintln!("agent connection error: {}", err);
                        break;
                    }
                    Some(Ok(line)) => line,
                }
            },
        };
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(err) => {
                eprintln!("unparseable request: {}", err);
                continue;
            }
        };
        let response = tokio::select! {
            _ = cancel.cancelled() => break,
            response = respond_and_journal(value, &journal, &artifacts, &state) => response,
        };
        let Ok(text) = serde_json::to_string(&response) else {
            eprintln!("failed to serialize response");
            continue;
        };
        if let Err(err) = framed.send(text).await {
            eprintln!("failed to send response: {}", err);
            break;
        }
        // The reference changes only after the final response is sent, before
        // the next request runs. Unsent sequence screenshots never replace it.
        state.record_response(response);
    }
    if let Err(error) = input.release_all().await {
        eprintln!("failed to release synthetic input: {error}");
    }
}

/// Per-connection view state. A crop invalidates the full-screen reference;
/// pixels outside the returned crop must not be treated as seen by the agent.
struct ClientState {
    last_screenshot: Option<ScreenshotImage>,
    input: Arc<input::SyntheticInput>,
}

impl ClientState {
    fn record_response(&mut self, response: ComputerUseResponse) {
        if let ComputerUseResponse::Image {
            reference_screenshot,
            ..
        } = response
        {
            self.last_screenshot = reference_screenshot;
        }
    }
}

/// Executes one request and journals it. Every request produces exactly one
/// journal entry and one response: computer actions are journaled here (qbt
/// is the only component that sees them all, in execution order) while
/// `publish_event` journals the agent's own event under its published kind.
/// A `run_sequence` executes its steps through this same path, one journal
/// entry per step, then answers with a screenshot of the final state.
async fn respond_and_journal(
    value: serde_json::Value,
    journal: &Journal,
    artifacts: &ArtifactStore,
    state: &ClientState,
) -> ComputerUseResponse {
    let request: ComputerUseRequest = match serde_json::from_value(value.clone()) {
        Ok(request) => request,
        Err(err) => {
            let id = value.get("id").and_then(|id| id.as_u64()).unwrap_or(0) as usize;
            journal.append(
                "computer.invalid_request",
                serde_json::json!({ "error": err.to_string() }),
                None,
            );
            return ComputerUseResponse::Error {
                id,
                ok: false,
                error: format!("invalid request: {}", err),
            };
        }
    };

    if let ComputerUseRequest::PublishEvent { id, kind, payload } = request {
        journal.append(kind, payload, None);
        return ComputerUseResponse::Empty { id, ok: true };
    }

    if let ComputerUseRequest::ShutdownBackend { id } = request {
        return ComputerUseResponse::Empty { id, ok: true };
    }

    if let ComputerUseRequest::RunSequence { id, actions } = &request {
        return run_sequence(*id, actions.clone(), journal, artifacts, state).await;
    }

    execute_action(value, request, journal, artifacts, state).await
}

/// Runs one already-parsed action, journals it (with any screenshot it
/// captured), and returns its response.
async fn execute_action(
    value: serde_json::Value,
    request: ComputerUseRequest,
    journal: &Journal,
    artifacts: &ArtifactStore,
    state: &ClientState,
) -> ComputerUseResponse {
    let id = request.id();
    let (mut response, journal_screenshot) =
        match handle_request(&request, journal, artifacts, state).await {
            Ok(outcome) => outcome,
            Err(error) => {
                eprintln!("error handling request: {}", error);
                if let Err(release_error) = state.input.release_all().await {
                    eprintln!(
                        "failed to release synthetic input after action failure: {release_error}"
                    );
                }
                (
                    ComputerUseResponse::Error {
                        id,
                        ok: false,
                        error: error.to_string(),
                    },
                    None,
                )
            }
        };
    // The full request — including typed text — goes into the journal so
    // observers can see exactly what happened between screenshots. The
    // journal is in-memory, capped, and served only to the observatory;
    // streams that leave the machine must redact text themselves.
    let event = journal.append(
        "computer.action",
        action_payload(value, &response),
        journal_screenshot,
    );
    response.set_screenshot_id(event.screenshot_id.clone());
    response
}

fn action_payload(request: serde_json::Value, response: &ComputerUseResponse) -> serde_json::Value {
    let mut payload = serde_json::json!({ "request": request, "ok": response.completed() });
    if let ComputerUseResponse::Image {
        foreground_window, ..
    } = response
    {
        payload["foregroundWindow"] = serde_json::json!(foreground_window);
    }
    payload
}

const MAX_SEQUENCE_ACTIONS: usize = 20;
const POST_ACTION_SETTLE_MS: u64 = 250;
const BRIEF_PAUSE_MS: u64 = 300;

/// Executes a queued sequence of actions back-to-back, aborts on the first
/// failing step, and answers with one screenshot of the final state — so a
/// multi-step interaction costs the caller one round trip instead of one per
/// action. Steps run through the same execution and journaling path as
/// standalone actions.
async fn run_sequence(
    id: usize,
    actions: Vec<serde_json::Value>,
    journal: &Journal,
    artifacts: &ArtifactStore,
    state: &ClientState,
) -> ComputerUseResponse {
    if actions.is_empty() {
        return ComputerUseResponse::Error {
            id,
            ok: false,
            error: "run_sequence requires at least one action".into(),
        };
    }
    if actions.len() > MAX_SEQUENCE_ACTIONS {
        return ComputerUseResponse::Error {
            id,
            ok: false,
            error: format!("run_sequence accepts at most {MAX_SEQUENCE_ACTIONS} actions"),
        };
    }
    // Parse and validate every step before executing any: a malformed
    // sequence must not half-execute. Runtime failures during execution
    // still abort at the step that failed.
    let mut steps = Vec::with_capacity(actions.len());
    for (index, mut item) in actions.into_iter().enumerate() {
        // Give each step a distinct id so journal entries stay identifiable;
        // the caller matches only the final sequence response by its own id.
        if let Some(object) = item.as_object_mut() {
            object.insert("id".into(), serde_json::json!(index + 1));
        }
        let step_request = match serde_json::from_value::<ComputerUseRequest>(item.clone()) {
            Ok(ComputerUseRequest::RunSequence { .. }) => {
                return ComputerUseResponse::Error {
                    id,
                    ok: false,
                    error: "run_sequence cannot be nested".into(),
                };
            }
            Ok(ComputerUseRequest::PublishEvent { .. }) => {
                return ComputerUseResponse::Error {
                    id,
                    ok: false,
                    error: "publish_event is not allowed inside run_sequence".into(),
                };
            }
            Ok(ComputerUseRequest::ShutdownBackend { .. }) => {
                return ComputerUseResponse::Error {
                    id,
                    ok: false,
                    error: "shutdown_backend is not allowed inside run_sequence".into(),
                };
            }
            Ok(step) => step,
            Err(err) => {
                journal.append(
                    "computer.invalid_request",
                    serde_json::json!({ "error": err.to_string() }),
                    None,
                );
                return ComputerUseResponse::Error {
                    id,
                    ok: false,
                    error: format!(
                        "sequence aborted at step {}: invalid action: {}",
                        index + 1,
                        err
                    ),
                };
            }
        };
        steps.push((item, step_request));
    }
    let mut executed = 0;
    for (item, step_request) in steps {
        let mut response = execute_action(item, step_request, journal, artifacts, state).await;
        if !response.completed() {
            match &mut response {
                ComputerUseResponse::Error {
                    id: response_id,
                    error,
                    ..
                } => {
                    *response_id = id;
                    *error = format!("sequence aborted at step {}: {}", executed + 1, error);
                }
                ComputerUseResponse::Image {
                    id: response_id,
                    text,
                    ..
                } => {
                    *response_id = id;
                    *text = Some(format!(
                        "Sequence aborted at step {}. {}",
                        executed + 1,
                        text.as_deref().unwrap_or("Action aborted."),
                    ));
                }
                _ => unreachable!("only errors and guarded images abort actions"),
            }
            return response;
        }
        executed += 1;
    }
    let (mut response, journal_screenshot) = match reply_screenshot(id, None).await {
        Ok(outcome) => outcome,
        Err(error) => {
            return ComputerUseResponse::Error {
                id,
                ok: false,
                error: format!(
                    "sequence executed {} actions but the final screenshot failed: {}",
                    executed, error
                ),
            };
        }
    };
    let event = journal.append(
        "computer.action",
        action_payload(
            serde_json::json!({ "action": "run_sequence", "steps": executed }),
            &response,
        ),
        journal_screenshot,
    );
    response.set_screenshot_id(event.screenshot_id.clone());
    if let ComputerUseResponse::Image { text, .. } = &mut response {
        *text = Some(format!("Executed {executed} actions."));
    }
    response
}

/// Executes the action and returns its response plus, for screen-capturing
/// actions, the full-screen PNG for the journal. State-changing actions
/// (clicks, drag, type, key, scroll) answer with a screenshot of the
/// resulting state, so the caller never needs a second round trip just to
/// see what happened.
async fn handle_request(
    request: &ComputerUseRequest,
    journal: &Journal,
    artifacts: &ArtifactStore,
    state: &ClientState,
) -> anyhow::Result<(ComputerUseResponse, Option<Vec<u8>>)> {
    match request {
        ComputerUseRequest::PublishEvent { .. } => {
            // Handled before execution reaches here; see respond_and_journal.
            unreachable!("publish_event is not a computer action")
        }
        ComputerUseRequest::ShutdownBackend { .. } => {
            unreachable!("shutdown_backend is handled before action execution")
        }
        ComputerUseRequest::RunSequence { .. } => {
            // Handled before execution reaches here; see respond_and_journal.
            unreachable!("run_sequence is executed by run_sequence()")
        }
        ComputerUseRequest::GetDisplayInfo { id } => {
            let (width, height) = pal::ScreenSampler::new()?.size_px();
            Ok((
                ComputerUseResponse::DisplayInfo {
                    id: *id,
                    ok: true,
                    display: ComputerUseDisplayInfo {
                        width_px: width,
                        height_px: height,
                    },
                },
                None,
            ))
        }
        ComputerUseRequest::SaveScreenshot {
            id,
            screenshot_id,
            path,
        } => {
            let png = journal
                .screenshot(screenshot_id)
                .ok_or_else(|| anyhow::anyhow!("screenshot evicted"))?;
            let saved = artifacts.save(path, &png)?;
            Ok((
                ComputerUseResponse::Saved {
                    id: *id,
                    ok: true,
                    saved,
                },
                None,
            ))
        }
        ComputerUseRequest::GetClipboard { id } => Ok((
            ComputerUseResponse::Clipboard {
                id: *id,
                ok: true,
                text: tokio::task::spawn_blocking(pal::get_clipboard).await??,
            },
            None,
        )),
        ComputerUseRequest::SetClipboard { id, text } => {
            let text = text.clone();
            tokio::task::spawn_blocking(move || pal::set_clipboard(text)).await??;
            Ok((ComputerUseResponse::Empty { id: *id, ok: true }, None))
        }
        ComputerUseRequest::CursorPosition { id } => {
            let (x, y) = pal::cursor_position()?;
            Ok((
                ComputerUseResponse::Text {
                    id: *id,
                    ok: true,
                    text: format!("X={},Y={}", x, y),
                },
                None,
            ))
        }
        ComputerUseRequest::Zoom { id, region } => {
            let (x0, y0, x1, y1) = *region;
            let (x0, x1) = (std::cmp::min(x0, x1), std::cmp::max(x0, x1));
            let (y0, y1) = (std::cmp::min(y0, y1), std::cmp::max(y0, y1));
            let (width, height) = (x1 - x0, y1 - y0);
            reply_screenshot(*id, Some((x0, y0, width, height))).await
        }
        ComputerUseRequest::BriefPause { id } => {
            brief_pause().await;
            reply_screenshot(*id, None).await
        }
        ComputerUseRequest::Wait {
            id,
            duration_seconds,
        } => {
            tokio::time::sleep(Duration::from_secs_f64(*duration_seconds)).await;
            reply_screenshot(*id, None).await
        }
        ComputerUseRequest::Screenshot { id } => reply_screenshot(*id, None).await,
        ComputerUseRequest::MouseMove {
            id,
            coordinate: (x, y),
        } => {
            pal::mouse_move_to((*x as i32, *y as i32)).await?;
            Ok((ComputerUseResponse::Empty { id: *id, ok: true }, None))
        }
        ComputerUseRequest::LeftClick(params)
        | ComputerUseRequest::RightClick(params)
        | ComputerUseRequest::MiddleClick(params)
        | ComputerUseRequest::DoubleClick(params)
        | ComputerUseRequest::TripleClick(params) => {
            let MouseClickParams {
                id,
                key,
                coordinate,
                expect_unchanged,
            } = params;
            if let Some(region) = expect_unchanged {
                if let Some(guarded) = click_guard(*id, *region, state).await? {
                    return Ok(guarded);
                }
            }
            if let Some(key) = key {
                input::press_keys(&state.input, key).await?;
            }
            if let Some((x, y)) = coordinate {
                pal::mouse_move_to((*x as i32, *y as i32)).await?;
            }
            let (button, click_count) = request.mouse_clickiness().unwrap();
            for _ in 0..click_count {
                state.input.mouse_down(button).await?;
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                state.input.mouse_up(button).await?;
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            if let Some(key) = key {
                input::release_keys(&state.input, key).await?;
            }
            settle().await;
            reply_screenshot(*id, None).await
        }
        ComputerUseRequest::LeftMouseDown {
            id,
            coordinate: (x, y),
        } => {
            pal::mouse_move_to((*x as i32, *y as i32)).await?;
            state.input.mouse_down(MouseButton::Left).await?;
            Ok((ComputerUseResponse::Empty { id: *id, ok: true }, None))
        }
        ComputerUseRequest::LeftMouseUp {
            id,
            coordinate: (x, y),
        } => {
            pal::mouse_move_to((*x as i32, *y as i32)).await?;
            state.input.mouse_up(MouseButton::Left).await?;
            Ok((ComputerUseResponse::Empty { id: *id, ok: true }, None))
        }
        ComputerUseRequest::LeftClickDrag {
            id,
            coordinate,
            start_coordinate,
        } => {
            pal::mouse_move_to(((*start_coordinate).0 as i32, (*start_coordinate).1 as i32))
                .await?;
            state.input.mouse_down(MouseButton::Left).await?;
            pal::mouse_move_to(((*coordinate).0 as i32, (*coordinate).1 as i32)).await?;
            state.input.mouse_up(MouseButton::Left).await?;
            settle().await;
            reply_screenshot(*id, None).await
        }
        ComputerUseRequest::Type { id, text } => {
            input::type_text(&state.input, text).await?;
            settle().await;
            reply_screenshot(*id, None).await
        }
        ComputerUseRequest::Key { id, text } => {
            input::press_release_keys(&state.input, text).await?;
            settle().await;
            reply_screenshot(*id, None).await
        }
        ComputerUseRequest::HoldKey {
            id,
            duration_seconds,
            text,
        } => {
            input::hold_keys(
                state.input.clone(),
                text,
                Duration::from_secs_f64(*duration_seconds),
            )
            .await?;
            Ok((
                ComputerUseResponse::Text {
                    id: *id,
                    ok: true,
                    text: "The specified delay will complete asynchronously.".into(),
                },
                None,
            ))
        }
        ComputerUseRequest::Scroll {
            id,
            scroll_amount,
            scroll_direction,
            coordinate,
        } => {
            if let Some((x, y)) = coordinate {
                pal::mouse_move_to((*x as i32, *y as i32)).await?;
            }
            pal::mouse_scroll(scroll_amount, scroll_direction).await?;
            settle().await;
            reply_screenshot(*id, None).await
        }
    }
}

/// Gives the UI a beat to react before the post-action screenshot, so the
/// returned state is the result of the action rather than mid-transition.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(POST_ACTION_SETTLE_MS)).await;
}

async fn brief_pause() {
    tokio::time::sleep(Duration::from_millis(BRIEF_PAUSE_MS)).await;
}

/// The click guard: when the caller marks a region `expect_unchanged`, the
/// region is compared against the last screenshot the caller was shown. If
/// it changed, the click is aborted and `Some(response)` carries a fresh
/// screenshot (plus explanatory text) instead. A missing reference also
/// aborts: the guard cannot prove that the requested region is unchanged.
async fn click_guard(
    id: usize,
    region: (usize, usize, usize, usize),
    state: &ClientState,
) -> anyhow::Result<Option<(ComputerUseResponse, Option<Vec<u8>>)>> {
    let current = pal::screenshot()?;
    let changed = match &state.last_screenshot {
        Some(last) => region_changed(last, &current.image, region),
        None => true,
    };
    if !changed {
        return Ok(None);
    }
    let mut response = image_response(id, &current, None)?;
    if let ComputerUseResponse::Image { aborted, text, .. } = &mut response.0 {
        *aborted = true;
        let reason = if state.last_screenshot.is_none() {
            "no full-screen reference screenshot is available"
        } else {
            "the region you expected unchanged has changed or is outside the screenshot"
        };
        *text = Some(format!(
            "Click aborted: {reason}. A fresh screenshot is attached; re-assess before clicking."
        ));
    }
    Ok(Some(response))
}

const GUARD_DIFF_THRESHOLD: u8 = 16;
const GUARD_CHANGED_FRACTION: f64 = 0.02;

/// True when more than `GUARD_CHANGED_FRACTION` of the region's pixels
/// differ between the two screenshots by more than `GUARD_DIFF_THRESHOLD`
/// per channel. A region outside either image counts as changed: the guard
/// cannot prove it is safe, so it must not click.
fn region_changed(
    last: &ScreenshotImage,
    current: &ScreenshotImage,
    (x, y, w, h): (usize, usize, usize, usize),
) -> bool {
    let Some(right) = x.checked_add(w) else {
        return true;
    };
    let Some(bottom) = y.checked_add(h) else {
        return true;
    };
    let in_bounds =
        |img: &ScreenshotImage| right <= img.width() as usize && bottom <= img.height() as usize;
    if w == 0 || h == 0 || !in_bounds(last) || !in_bounds(current) {
        return true;
    }
    let mut total = 0usize;
    let mut changed = 0usize;
    for dy in 0..h {
        for dx in 0..w {
            let a = last.get_pixel((x + dx) as u32, (y + dy) as u32);
            let b = current.get_pixel((x + dx) as u32, (y + dy) as u32);
            let differs = a.0[0].abs_diff(b.0[0]) > GUARD_DIFF_THRESHOLD
                || a.0[1].abs_diff(b.0[1]) > GUARD_DIFF_THRESHOLD
                || a.0[2].abs_diff(b.0[2]) > GUARD_DIFF_THRESHOLD;
            if differs {
                changed += 1;
            }
            total += 1;
        }
    }
    (changed as f64) / (total as f64) > GUARD_CHANGED_FRACTION
}

/// Takes a screenshot and builds the agent's response plus the full-screen
/// PNG for the journal. `bounds` is (x, y, width, height); the agent gets the
/// cropped view but the journal always gets the full screen, so the
/// observatory shows consistently sized screenshots.
async fn reply_screenshot(
    id: usize,
    bounds: Option<(usize, usize, usize, usize)>,
) -> anyhow::Result<(ComputerUseResponse, Option<Vec<u8>>)> {
    let screenshot = pal::screenshot()?;
    image_response(id, &screenshot, bounds)
}

/// Encodes the response and its next guard reference together, plus the
/// full-screen PNG for the journal. Encoding alone does not update view state.
fn image_response(
    id: usize,
    capture: &CapturedScreenshot,
    bounds: Option<(usize, usize, usize, usize)>,
) -> anyhow::Result<(ComputerUseResponse, Option<Vec<u8>>)> {
    let screenshot = &capture.image;
    let cropped = {
        let (x, y, mut width, mut height) = bounds.unwrap_or((
            0,
            0,
            screenshot.width() as usize,
            screenshot.height() as usize,
        ));
        anyhow::ensure!(
            x < screenshot.width() as usize
                && y < screenshot.height() as usize
                && width > 0
                && height > 0,
            "screenshot region must overlap the screen and have nonzero width and height"
        );
        width = std::cmp::min(width, screenshot.width() as usize - x);
        height = std::cmp::min(height, screenshot.height() as usize - y);
        screenshot
            .view(x as u32, y as u32, width as u32, height as u32)
            .to_image()
    };

    let mut full_png_bytes = Vec::new();
    screenshot.write_to(
        &mut std::io::Cursor::new(&mut full_png_bytes),
        ImageFormat::Png,
    )?;

    let mut png_bytes = Vec::new();
    cropped.write_to(&mut std::io::Cursor::new(&mut png_bytes), ImageFormat::Png)?;
    let base64_png_bytes = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
    Ok((
        ComputerUseResponse::Image {
            id,
            ok: true,
            aborted: false,
            text: None,
            foreground_window: capture.foreground_window.clone(),
            image: ComputerUseImage {
                data: base64_png_bytes,
                media_type: "image/png".into(),
            },
            screenshot_id: None,
            reference_screenshot: if bounds.is_none() {
                Some(screenshot.clone())
            } else {
                None
            },
        },
        Some(full_png_bytes),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::DEFAULT_MAX_SCREENSHOTS;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    fn test_artifacts() -> Arc<ArtifactStore> {
        let root =
            std::env::temp_dir().join(format!("qbt-computer-use-tests-{}", std::process::id(),));
        Arc::new(ArtifactStore::new(root).unwrap())
    }

    fn client_state(last_screenshot: Option<ScreenshotImage>) -> ClientState {
        ClientState {
            last_screenshot,
            input: Arc::new(input::SyntheticInput::default()),
        }
    }

    async fn start_test_server_with_capacity(
        max_screenshots: usize,
    ) -> (
        std::net::SocketAddr,
        Arc<Journal>,
        Arc<ArtifactStore>,
        CancellationToken,
    ) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let journal = Journal::new(max_screenshots);
        let artifacts = test_artifacts();
        let shutdown = CancellationToken::new();
        tokio::spawn(serve_agent(
            listener,
            journal.clone(),
            artifacts.clone(),
            shutdown.clone(),
        ));
        (addr, journal, artifacts, shutdown)
    }

    async fn start_test_server() -> (std::net::SocketAddr, Arc<Journal>, CancellationToken) {
        let (addr, journal, _artifacts, shutdown) =
            start_test_server_with_capacity(DEFAULT_MAX_SCREENSHOTS).await;
        (addr, journal, shutdown)
    }

    async fn publish(
        stream: &mut BufReader<TcpStream>,
        id: usize,
        kind: &str,
    ) -> serde_json::Value {
        let request = serde_json::json!({
            "action": "publish_event",
            "id": id,
            "kind": kind,
            "payload": {},
        });
        stream
            .get_mut()
            .write_all(format!("{}\n", request).as_bytes())
            .await
            .unwrap();
        let mut line = String::new();
        stream.read_line(&mut line).await.unwrap();
        serde_json::from_str(&line).unwrap()
    }

    #[tokio::test]
    async fn shutdown_backend_acknowledges_then_stops_the_server() {
        let (addr, _journal, shutdown) = start_test_server().await;
        let mut client = BufReader::new(TcpStream::connect(addr).await.unwrap());
        let response = send(
            &mut client,
            serde_json::json!({ "id": 1, "action": "shutdown_backend" }),
        )
        .await;

        assert_eq!(response["ok"], true);
        tokio::time::timeout(Duration::from_secs(1), shutdown.cancelled())
            .await
            .unwrap();
        let reconnect = tokio::time::timeout(Duration::from_secs(1), TcpStream::connect(addr))
            .await
            .unwrap();
        assert!(reconnect.is_err());
    }

    #[tokio::test]
    async fn malformed_shutdown_gets_an_error_without_stopping_the_server() {
        let (addr, _journal, shutdown) = start_test_server().await;
        let mut client = BufReader::new(TcpStream::connect(addr).await.unwrap());
        client
            .get_mut()
            .write_all(b"{\"action\":\"shutdown_backend\"}\n")
            .await
            .unwrap();
        let mut line = String::new();
        client.read_line(&mut line).await.unwrap();
        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["ok"], false);
        assert!(!shutdown.is_cancelled());

        let response = publish(&mut client, 2, "test.still_alive").await;
        assert_eq!(response["ok"], true);
        shutdown.cancel();
    }

    #[tokio::test]
    async fn shutdown_bypasses_a_blocked_action() {
        let (addr, _journal, shutdown) = start_test_server().await;
        let mut action_client = BufReader::new(TcpStream::connect(addr).await.unwrap());
        action_client
            .get_mut()
            .write_all(b"{\"id\":1,\"action\":\"wait\",\"durationSeconds\":60}\n")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(25)).await;

        let mut control_client = BufReader::new(TcpStream::connect(addr).await.unwrap());
        let response = tokio::time::timeout(
            Duration::from_millis(500),
            send(
                &mut control_client,
                serde_json::json!({ "id": 2, "action": "shutdown_backend" }),
            ),
        )
        .await
        .expect("shutdown must not wait for the action queue");
        assert_eq!(response["ok"], true);
        tokio::time::timeout(Duration::from_millis(500), shutdown.cancelled())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn brief_pause_waits_300_ms_and_returns_a_screenshot() {
        assert_eq!(BRIEF_PAUSE_MS, 300);
        let (addr, _journal, shutdown) = start_test_server().await;
        let mut client = BufReader::new(TcpStream::connect(addr).await.unwrap());
        let started = tokio::time::Instant::now();
        let response = send(
            &mut client,
            serde_json::json!({ "id": 1, "action": "brief_pause" }),
        )
        .await;

        assert!(started.elapsed() >= Duration::from_millis(BRIEF_PAUSE_MS));
        assert_eq!(response["ok"], true);
        assert_eq!(response["image"]["mediaType"], "image/png");
        shutdown.cancel();
    }

    #[tokio::test]
    async fn a_new_agent_replaces_the_old_one() {
        let (addr, journal, _shutdown) = start_test_server().await;

        let mut first = BufReader::new(TcpStream::connect(addr).await.unwrap());
        let response = publish(&mut first, 1, "test.first").await;
        assert_eq!(response["ok"], true);

        // The second connection must be served even though the first client
        // never disconnected (e.g. a killed CLI whose socket lingers).
        let mut second = BufReader::new(TcpStream::connect(addr).await.unwrap());
        let response = publish(&mut second, 1, "test.second").await;
        assert_eq!(response["ok"], true);

        // The first client is disconnected: its next read returns EOF.
        let mut line = String::new();
        let read = first.read_line(&mut line).await.unwrap();
        assert_eq!(read, 0);

        let (_, events) = journal.subscribe_with_snapshot();
        let kinds: Vec<&str> = events.iter().map(|event| event.kind.as_str()).collect();
        assert_eq!(kinds, vec!["test.first", "test.second"]);
    }

    #[tokio::test]
    async fn shutdown_disconnects_the_agent() {
        let (addr, _journal, shutdown) = start_test_server().await;
        let mut client = BufReader::new(TcpStream::connect(addr).await.unwrap());
        let response = publish(&mut client, 1, "test.event").await;
        assert_eq!(response["ok"], true);

        shutdown.cancel();
        let mut line = String::new();
        let read = client.read_line(&mut line).await.unwrap();
        assert_eq!(read, 0);
    }

    #[tokio::test]
    async fn invalid_requests_get_an_error_response() {
        let (addr, _journal, _shutdown) = start_test_server().await;
        let mut client = BufReader::new(TcpStream::connect(addr).await.unwrap());
        client
            .get_mut()
            .write_all(b"{\"id\": 7, \"action\": \"no_such_action\"}\n")
            .await
            .unwrap();
        let mut line = String::new();
        client.read_line(&mut line).await.unwrap();
        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], 7);
        assert_eq!(response["ok"], false);
    }

    async fn send(
        stream: &mut BufReader<TcpStream>,
        request: serde_json::Value,
    ) -> serde_json::Value {
        stream
            .get_mut()
            .write_all(format!("{}\n", request).as_bytes())
            .await
            .unwrap();
        let mut line = String::new();
        stream.read_line(&mut line).await.unwrap();
        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        if response.get("image").is_some() {
            assert!(
                response["screenshot_id"]
                    .as_str()
                    .unwrap()
                    .starts_with("shot_")
            );
            assert_foreground_contract(&response);
        }
        response
    }

    #[tokio::test]
    async fn screenshot_id_can_save_the_exact_journal_png() {
        let (addr, journal, artifacts, shutdown) =
            start_test_server_with_capacity(DEFAULT_MAX_SCREENSHOTS).await;
        let mut client = BufReader::new(TcpStream::connect(addr).await.unwrap());
        let screenshot = send(
            &mut client,
            serde_json::json!({ "id": 1, "action": "screenshot" }),
        )
        .await;
        let screenshot_id = screenshot["screenshot_id"].as_str().unwrap();
        let (_, events) = journal.subscribe_with_snapshot();
        assert_eq!(events[0].screenshot_id.as_deref(), Some(screenshot_id));

        let saved = send(
            &mut client,
            serde_json::json!({
                "id": 2,
                "action": "save_screenshot",
                "screenshot_id": screenshot_id,
                "path": "runs/x/screenshots/a.png",
            }),
        )
        .await;
        assert_eq!(saved["id"], 2);
        assert_eq!(saved["ok"], true);
        let saved_path = PathBuf::from(saved["saved"].as_str().unwrap());
        assert_eq!(
            saved_path,
            artifacts.root().join("runs/x/screenshots/a.png")
        );
        assert_eq!(
            std::fs::read(&saved_path).unwrap(),
            **journal.screenshot(screenshot_id).unwrap(),
        );
        shutdown.cancel();
        std::fs::remove_dir_all(artifacts.root()).unwrap();
    }

    #[tokio::test]
    async fn save_screenshot_reports_eviction_and_rejects_parent_segments() {
        let (addr, _journal, artifacts, shutdown) = start_test_server_with_capacity(1).await;
        let mut client = BufReader::new(TcpStream::connect(addr).await.unwrap());
        let first = send(
            &mut client,
            serde_json::json!({ "id": 1, "action": "screenshot" }),
        )
        .await;
        send(
            &mut client,
            serde_json::json!({ "id": 2, "action": "screenshot" }),
        )
        .await;
        let evicted = send(
            &mut client,
            serde_json::json!({
                "id": 3,
                "action": "save_screenshot",
                "screenshot_id": first["screenshot_id"],
                "path": "evicted.png",
            }),
        )
        .await;
        assert_eq!(evicted["ok"], false);
        assert_eq!(evicted["error"], "screenshot evicted");

        let rejected = send(
            &mut client,
            serde_json::json!({
                "id": 4,
                "action": "save_screenshot",
                "screenshot_id": "shot_2",
                "path": "../outside.png",
            }),
        )
        .await;
        assert_eq!(rejected["ok"], false);
        assert!(
            rejected["error"]
                .as_str()
                .unwrap()
                .contains("must not contain '..'")
        );
        shutdown.cancel();
        std::fs::remove_dir_all(artifacts.root()).unwrap();
    }

    fn assert_foreground_contract(response: &serde_json::Value) {
        let foreground = response
            .get("foregroundWindow")
            .expect("every screenshot includes foregroundWindow");
        if !foreground.is_null() {
            let fields = foreground
                .as_object()
                .expect("foregroundWindow must be null or an object");
            assert_eq!(fields.len(), 2);
            for name in ["executable", "title"] {
                let value = fields.get(name).expect("both fields must be present");
                assert!(value.is_null() || value.is_string());
            }
        }
    }

    #[test]
    fn image_and_journal_preserve_known_partial_and_unknown_foreground() {
        for foreground in [
            serde_json::json!({ "executable": "C:\\app.exe", "title": "文書\n<untrusted>" }),
            serde_json::json!({ "executable": null, "title": "" }),
            serde_json::json!({ "executable": "C:\\app.exe", "title": null }),
            serde_json::json!({ "executable": null, "title": null }),
            serde_json::Value::Null,
        ] {
            let capture = CapturedScreenshot {
                image: ScreenshotImage::new(2, 2),
                foreground_window: foreground.as_object().map(|fields| ForegroundWindow {
                    executable: fields["executable"].as_str().map(str::to_owned),
                    title: fields["title"].as_str().map(str::to_owned),
                }),
            };
            for bounds in [None, Some((0, 0, 1, 1))] {
                let (response, _) = image_response(1, &capture, bounds).unwrap();
                let wire = serde_json::to_value(&response).unwrap();
                assert_foreground_contract(&wire);
                assert_eq!(wire["foregroundWindow"], foreground);
                assert_eq!(
                    action_payload(serde_json::json!({}), &response)["foregroundWindow"],
                    foreground
                );
                assert!(wire.get("foreground_window").is_none());
            }
        }
        let empty = ComputerUseResponse::Empty { id: 1, ok: true };
        assert!(
            action_payload(serde_json::json!({}), &empty)
                .get("foregroundWindow")
                .is_none()
        );
    }

    #[tokio::test]
    async fn normal_wait_and_post_action_screenshots_share_metadata_with_the_journal() {
        let (addr, journal, shutdown) = start_test_server().await;
        let mut client = BufReader::new(TcpStream::connect(addr).await.unwrap());
        // Empty text takes the real post-action settle/capture path without
        // sending input or changing the user's GUI state.
        for (index, mut request) in [
            serde_json::json!({ "action": "screenshot" }),
            serde_json::json!({ "action": "wait", "durationSeconds": 0 }),
            serde_json::json!({ "action": "type", "text": "" }),
        ]
        .into_iter()
        .enumerate()
        {
            request["id"] = serde_json::json!(index + 1);
            let response = send(&mut client, request).await;
            assert_eq!(response["ok"], true);
            assert_eq!(response["image"]["mediaType"], "image/png");
            let (_, events) = journal.subscribe_with_snapshot();
            let event = events.last().unwrap();
            assert_eq!(
                event.payload.get("foregroundWindow"),
                response.get("foregroundWindow")
            );
            let png = base64::engine::general_purpose::STANDARD
                .decode(response["image"]["data"].as_str().unwrap())
                .unwrap();
            assert_eq!(
                *journal
                    .screenshot(event.screenshot_id.as_deref().unwrap())
                    .unwrap(),
                png
            );
        }
        shutdown.cancel();
    }

    #[test]
    fn region_changed_checks_pixels_and_rejects_invalid_bounds() {
        let last = ScreenshotImage::from_pixel(10, 10, image::Rgba([0, 0, 0, 255]));
        let mut current = last.clone();
        assert!(!region_changed(&last, &current, (0, 0, 10, 10)));
        current.put_pixel(0, 0, image::Rgba([GUARD_DIFF_THRESHOLD, 0, 0, 255]));
        assert!(!region_changed(&last, &current, (0, 0, 1, 1)));
        current.put_pixel(0, 0, image::Rgba([GUARD_DIFF_THRESHOLD + 1, 0, 0, 255]));
        assert!(region_changed(&last, &current, (0, 0, 1, 1)));
        assert!(!region_changed(&last, &current, (0, 0, 10, 10)));
        current.put_pixel(1, 0, image::Rgba([255, 0, 0, 255]));
        current.put_pixel(2, 0, image::Rgba([255, 0, 0, 255]));
        assert!(region_changed(&last, &current, (0, 0, 10, 10)));

        for region in [
            (0, 0, 0, 1),
            (0, 0, 1, 0),
            (10, 0, 1, 1),
            (0, 10, 1, 1),
            (9, 0, 2, 1),
            (0, 9, 1, 2),
            (usize::MAX, 0, 2, 1),
            (0, usize::MAX, 1, 2),
            (1, 0, usize::MAX, 1),
            (0, 1, 1, usize::MAX),
        ] {
            assert!(region_changed(&last, &last, region), "region: {region:?}");
        }
        let smaller = ScreenshotImage::new(1, 1);
        assert!(region_changed(&smaller, &last, (0, 0, 2, 2)));
        assert!(region_changed(&last, &smaller, (0, 0, 2, 2)));
    }

    #[test]
    fn only_returned_full_images_establish_a_guard_reference() {
        let capture = CapturedScreenshot {
            image: ScreenshotImage::from_pixel(4, 4, image::Rgba([80, 90, 100, 255])),
            foreground_window: None,
        };
        let full = capture.image.clone();
        let mut state = client_state(None);
        let (response, _) = image_response(1, &capture, None).unwrap();
        assert!(state.last_screenshot.is_none());
        let wire = serde_json::to_value(&response).unwrap();
        assert_eq!(wire["aborted"], false);
        assert!(wire.get("reference_screenshot").is_none());
        assert!(wire.get("referenceScreenshot").is_none());
        state.record_response(response);
        assert_eq!(state.last_screenshot.as_ref(), Some(&full));

        let (crop_response, journal_png) = image_response(2, &capture, Some((1, 1, 2, 2))).unwrap();
        let wire = serde_json::to_value(&crop_response).unwrap();
        let png = base64::engine::general_purpose::STANDARD
            .decode(wire["image"]["data"].as_str().unwrap())
            .unwrap();
        assert_eq!(image::load_from_memory(&png).unwrap().dimensions(), (2, 2));
        assert_eq!(
            image::load_from_memory(&journal_png.unwrap())
                .unwrap()
                .to_rgba8(),
            full
        );
        // Preparing an unseen crop does not invalidate the caller's reference.
        assert_eq!(state.last_screenshot.as_ref(), Some(&full));
        state.record_response(crop_response);
        assert!(state.last_screenshot.is_none());

        state.record_response(image_response(3, &capture, None).unwrap().0);
        for bounds in [
            (4, 0, 1, 1),
            (0, 4, 1, 1),
            (0, 0, 0, 1),
            (usize::MAX, 0, 1, 1),
        ] {
            assert!(image_response(4, &capture, Some(bounds)).is_err());
        }
        state.record_response(ComputerUseResponse::Error {
            id: 4,
            ok: false,
            error: "failed".into(),
        });
        assert_eq!(state.last_screenshot.as_ref(), Some(&full));
    }

    #[tokio::test]
    async fn missing_guard_reference_aborts_before_input_and_returns_an_image() {
        let (addr, journal, shutdown) = start_test_server().await;
        let mut client = BufReader::new(TcpStream::connect(addr).await.unwrap());
        // If the guard incorrectly permits input, this invalid chord fails
        // before sending any keys or reaching the mouse operations.
        let response = send(
            &mut client,
            serde_json::json!({
                "id": 47, "action": "left_click", "coordinate": [1, 1],
                "key": "invalid_guard_safety_key", "expectUnchanged": [0, 0, 1, 1],
            }),
        )
        .await;
        assert_eq!(response["id"], 47);
        assert_eq!(response["ok"], true);
        assert_eq!(response["aborted"], true);
        assert!(
            response["text"]
                .as_str()
                .unwrap()
                .contains("no full-screen reference")
        );
        assert_eq!(response["image"]["mediaType"], "image/png");
        let (_, events) = journal.subscribe_with_snapshot();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload["ok"], false);
        assert_eq!(
            events[0].payload.get("foregroundWindow"),
            response.get("foregroundWindow")
        );
        let journal_png = journal
            .screenshot(events[0].screenshot_id.as_deref().unwrap())
            .unwrap();
        let response_png = base64::engine::general_purpose::STANDARD
            .decode(response["image"]["data"].as_str().unwrap())
            .unwrap();
        assert_eq!(*journal_png, response_png);
        shutdown.cancel();
    }

    #[tokio::test]
    async fn socket_commits_the_final_sequence_image_but_invalidates_it_on_zoom() {
        let (addr, journal, shutdown) = start_test_server().await;
        let mut client = BufReader::new(TcpStream::connect(addr).await.unwrap());
        let sequence = send(
            &mut client,
            serde_json::json!({
                "id": 51, "action": "run_sequence", "actions": [
                    { "action": "screenshot" }, { "action": "cursor_position" },
                ],
            }),
        )
        .await;
        assert_eq!(sequence["id"], 51);
        assert_eq!(sequence["ok"], true);
        assert_eq!(sequence["aborted"], false);
        assert_eq!(sequence["text"], "Executed 2 actions.");
        let (_, events) = journal.subscribe_with_snapshot();
        assert_eq!(events.len(), 3);
        assert_eq!(events[2].payload["request"]["action"], "run_sequence");
        assert_eq!(events[2].payload["ok"], true);
        assert_eq!(
            events[2].payload.get("foregroundWindow"),
            sequence.get("foregroundWindow")
        );

        let guard = serde_json::json!({
            "id": 52, "action": "left_click", "key": "invalid_guard_safety_key",
            "expectUnchanged": [usize::MAX, 0, 2, 1],
        });
        let overflow = send(&mut client, guard.clone()).await;
        assert_eq!(overflow["aborted"], true);
        assert!(
            overflow["text"]
                .as_str()
                .unwrap()
                .contains("outside the screenshot")
        );

        let crop = send(
            &mut client,
            serde_json::json!({
                "id": 53, "action": "zoom", "region": [0, 0, 1, 1],
            }),
        )
        .await;
        assert_eq!(crop["ok"], true);
        assert_eq!(crop["aborted"], false);
        let after_crop = send(&mut client, guard).await;
        assert_eq!(after_crop["aborted"], true);
        assert!(
            after_crop["text"]
                .as_str()
                .unwrap()
                .contains("no full-screen reference")
        );
        shutdown.cancel();
    }

    #[tokio::test]
    async fn sequence_preserves_the_seen_reference_and_stops_on_guard_abort() {
        let mut changed_reference = pal::screenshot().unwrap().image;
        for y in 0..2 {
            for x in 0..2 {
                let pixel = changed_reference.get_pixel_mut(x, y);
                for channel in &mut pixel.0[..3] {
                    *channel = channel.wrapping_add(128);
                }
            }
        }
        // Absent, undersized, and visibly changed baselines must all abort.
        // An unseen intermediate screenshot must not replace any of them.
        for reference in [
            None,
            Some(ScreenshotImage::new(1, 1)),
            Some(changed_reference),
        ] {
            let mut state = client_state(reference.clone());
            let journal = Journal::new(DEFAULT_MAX_SCREENSHOTS);
            let artifacts = test_artifacts();
            let response = respond_and_journal(
                serde_json::json!({
                    "id": 81, "action": "run_sequence", "actions": [
                        { "action": "screenshot" },
                        { "action": "left_click", "key": "invalid_guard_safety_key",
                          "coordinate": [1, 1], "expectUnchanged": [0, 0, 2, 2] },
                        { "action": "key", "text": "invalid_later_step_sentinel" },
                    ],
                }),
                &journal,
                &artifacts,
                &state,
            )
            .await;
            assert!(!response.completed());
            let wire = serde_json::to_value(&response).unwrap();
            assert_eq!(wire["id"], 81);
            assert_foreground_contract(&wire);
            assert_eq!(wire["ok"], true);
            assert_eq!(wire["aborted"], true);
            assert!(
                wire["text"]
                    .as_str()
                    .unwrap()
                    .contains("Sequence aborted at step 2")
            );
            assert_eq!(state.last_screenshot, reference);
            let (_, events) = journal.subscribe_with_snapshot();
            assert_eq!(
                events.len(),
                2,
                "the later action must not execute or be journaled"
            );
            assert_eq!(events[0].payload["request"]["action"], "screenshot");
            assert_eq!(events[0].payload["ok"], true);
            assert_eq!(events[1].payload["request"]["action"], "left_click");
            assert_eq!(events[1].payload["ok"], false);
            assert_eq!(
                events[1].payload.get("foregroundWindow"),
                wire.get("foregroundWindow")
            );
            let png = base64::engine::general_purpose::STANDARD
                .decode(wire["image"]["data"].as_str().unwrap())
                .unwrap();
            assert_eq!(
                *journal
                    .screenshot(events[1].screenshot_id.as_deref().unwrap())
                    .unwrap(),
                png,
            );
            state.record_response(response);
            assert_eq!(
                state.last_screenshot.unwrap(),
                image::load_from_memory(&png).unwrap().to_rgba8()
            );
        }
    }

    #[tokio::test]
    async fn sequence_error_does_not_commit_intermediate_screenshots() {
        let state = client_state(None);
        let journal = Journal::new(DEFAULT_MAX_SCREENSHOTS);
        let artifacts = test_artifacts();
        let response = respond_and_journal(
            serde_json::json!({
                "id": 91, "action": "run_sequence", "actions": [
                    { "action": "screenshot" },
                    { "action": "key", "text": "invalid_runtime_sentinel" },
                    { "action": "cursor_position" },
                ],
            }),
            &journal,
            &artifacts,
            &state,
        )
        .await;
        let wire = serde_json::to_value(response).unwrap();
        assert_eq!(wire["id"], 91);
        assert_eq!(wire["ok"], false);
        assert!(
            wire["error"]
                .as_str()
                .unwrap()
                .contains("aborted at step 2")
        );
        assert!(state.last_screenshot.is_none());
        let (_, events) = journal.subscribe_with_snapshot();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].payload["ok"], false);
    }

    #[tokio::test]
    async fn run_sequence_rejects_bad_shapes_without_touching_the_screen() {
        let (addr, _journal, _shutdown) = start_test_server().await;
        let mut client = BufReader::new(TcpStream::connect(addr).await.unwrap());

        let empty = send(
            &mut client,
            serde_json::json!({ "id": 1, "action": "run_sequence", "actions": [] }),
        )
        .await;
        assert_eq!(empty["ok"], false);
        assert!(
            empty["error"]
                .as_str()
                .unwrap()
                .contains("at least one action")
        );

        let too_many_actions: Vec<serde_json::Value> = (0..21)
            .map(|i| serde_json::json!({ "action": "mouse_move", "coordinate": [1, 1], "id": i }))
            .collect();
        let too_many = send(
            &mut client,
            serde_json::json!({ "id": 2, "action": "run_sequence", "actions": too_many_actions }),
        )
        .await;
        assert_eq!(too_many["ok"], false);
        assert!(too_many["error"].as_str().unwrap().contains("at most 20"));

        let nested = send(
            &mut client,
            serde_json::json!({ "id": 3, "action": "run_sequence", "actions": [
                { "action": "run_sequence", "actions": [{ "action": "mouse_move", "coordinate": [1, 1] }] },
            ] }),
        )
        .await;
        assert_eq!(nested["ok"], false);
        assert!(
            nested["error"]
                .as_str()
                .unwrap()
                .contains("cannot be nested")
        );

        let bad_step = send(
            &mut client,
            serde_json::json!({ "id": 4, "action": "run_sequence", "actions": [
                { "action": "mouse_move", "coordinate": [1, 1] },
                { "action": "no_such_action" },
            ] }),
        )
        .await;
        assert_eq!(bad_step["ok"], false);
        let error = bad_step["error"].as_str().unwrap();
        assert!(
            error.contains("aborted at step 2"),
            "unexpected error: {error}"
        );
    }
}
