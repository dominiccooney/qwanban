// See https://github.com/anthropics/claude-quickstarts/blob/main/computer-use-demo/computer_use_demo/tools/computer.py
// See https://github.com/anthropics/anthropic-sdk-typescript/blob/4f2eb8071993780d79610b9eda26db96f7653843/src/resources/beta/messages/messages.ts#L3283

use std::sync::Arc;
use std::time::Duration;
use base64::Engine;
use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::{Framed, LinesCodec};
use tokio_util::sync::CancellationToken;
use futures::{SinkExt, StreamExt};
use image::{GenericImageView, ImageFormat};
use crate::journal::Journal;
use crate::{input, pal};
use crate::pal::MouseButton;

#[derive(Deserialize)]
pub(crate) struct MouseClickParams {
    id: usize,
    key: Option<String>,
    coordinate: Option<(usize, usize)>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub(crate) enum ComputerUseRequest {
    Key { id: usize, text: String, },
    Type { id: usize, text: String,},
    MouseMove { id: usize, coordinate: (usize, usize) },
    LeftClick(MouseClickParams),
    #[serde(rename_all="camelCase")]
    LeftClickDrag { id: usize, start_coordinate: (usize, usize), coordinate: (usize, usize) },
    RightClick(MouseClickParams),
    MiddleClick(MouseClickParams),
    DoubleClick(MouseClickParams),
    Screenshot { id: usize, },
    // *Gets* the cursor position
    CursorPosition { id: usize, },
    LeftMouseDown { id: usize, coordinate: (usize, usize), },
    LeftMouseUp { id: usize, coordinate: (usize, usize), },

    #[serde(rename_all="camelCase")]
    Scroll { id: usize, scroll_direction: ScrollDirection, scroll_amount: f64, coordinate: Option<(usize, usize)> },

    #[serde(rename_all="camelCase")]
    HoldKey { id: usize, duration_seconds: f64, text: String, },

    // Waits -> screenshot
    #[serde(rename_all="camelCase")]
    Wait { id: usize, duration_seconds: f64, },
    TripleClick(MouseClickParams),

    // Cropped screenshot, x0,y0,x1,y1
    Zoom { id: usize, region: (usize, usize, usize, usize) },

    // Not Claude events
    GetDisplayInfo { id: usize, },

    /// The agent publishes one of its own events (transcript message,
    /// coordinator status change, ...) into the journal for the observatory.
    /// `kind` namespaces the event (e.g. "transcript.message"); `payload` is
    /// passed through to observers untouched.
    PublishEvent { id: usize, kind: String, payload: serde_json::Value },
}

impl ComputerUseRequest {
    fn id(&self) -> usize {
        match self {
            ComputerUseRequest::Key { id, .. } => *id,
            ComputerUseRequest::Type { id, .. } => *id,
            ComputerUseRequest::MouseMove { id, .. } => *id,
            ComputerUseRequest::LeftClick(params) |
            ComputerUseRequest::RightClick(params) |
            ComputerUseRequest::MiddleClick(params) |
            ComputerUseRequest::DoubleClick(params) |
            ComputerUseRequest::TripleClick(params) => params.id,
            ComputerUseRequest::LeftClickDrag { id, .. } => *id,
            ComputerUseRequest::Screenshot { id, .. } => *id,
            ComputerUseRequest::CursorPosition { id, .. } => *id,
            ComputerUseRequest::LeftMouseDown { id, .. } => *id,
            ComputerUseRequest::LeftMouseUp { id, .. } => *id,
            ComputerUseRequest::Scroll { id, .. } => *id,
            ComputerUseRequest::HoldKey { id, .. } => *id,
            ComputerUseRequest::Wait { id, .. } => *id,
            ComputerUseRequest::Zoom { id, .. } => *id,
            ComputerUseRequest::GetDisplayInfo { id, .. } => *id,
            ComputerUseRequest::PublishEvent { id, .. } => *id,
        }
    }

    fn mouse_clickiness(&self) -> Option<(MouseButton, usize)> {
        match self {
            ComputerUseRequest::LeftClick(_) => Some((MouseButton::Left, 1)),
            ComputerUseRequest::RightClick(_) => Some((MouseButton::Right, 1)),
            ComputerUseRequest::MiddleClick(_) => Some((MouseButton::Middle, 1)),
            ComputerUseRequest::DoubleClick(_) => Some((MouseButton::Left, 2)),
            ComputerUseRequest::TripleClick(_) => Some((MouseButton::Left, 3)),
            _ => None
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
    Error { id: usize, ok: bool, error: String },
    Empty { id: usize, ok: bool },
    DisplayInfo { id: usize, ok: bool, display: ComputerUseDisplayInfo },
    Text { id: usize, ok: bool, text: String },
    Image { id: usize, ok: bool, image: ComputerUseImage },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ComputerUseDisplayInfo {
    width_px: usize,
    height_px: usize,
}

/// Serves the agent's JSONL socket. One agent at a time, most-recent-wins:
/// accepting a new connection cancels the previous client's task and awaits
/// it before the new client is served, so a restarted CLI can always
/// reconnect and two agents can never interleave input events. Runs until
/// `shutdown` cancels.
pub(crate) async fn serve_agent(
    listener: TcpListener,
    journal: Arc<Journal>,
    shutdown: CancellationToken,
) {
    let mut active_client: Option<(CancellationToken, tokio::task::JoinHandle<()>)> = None;
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                match accepted {
                    Ok((socket, peer)) => {
                        eprintln!("agent connected: {}", peer);
                        if let Some((cancel, handle)) = active_client.take() {
                            cancel.cancel();
                            // Cancellation is observed between requests, so an
                            // in-flight action (even a long `wait`) completes
                            // before the new agent is served. Slow, but it is
                            // what keeps two agents from interleaving input.
                            let _ = handle.await;
                        }
                        let cancel = shutdown.child_token();
                        let handle = tokio::spawn(handle_agent_client(socket, journal.clone(), cancel.clone()));
                        active_client = Some((cancel, handle));
                    }
                    Err(err) => {
                        eprintln!("error accepting agent socket: {}", err);
                    }
                }
            }
            _ = shutdown.cancelled() => break,
        }
    }
    if let Some((cancel, handle)) = active_client.take() {
        cancel.cancel();
        let _ = handle.await;
    }
}

async fn handle_agent_client(socket: TcpStream, journal: Arc<Journal>, cancel: CancellationToken) {
    let mut framed = Framed::new(socket, LinesCodec::new());
    loop {
        let line = tokio::select! {
            _ = cancel.cancelled() => break,
            next = framed.next() => match next {
                None => break,
                Some(Err(err)) => {
                    eprintln!("agent connection error: {}", err);
                    break;
                }
                Some(Ok(line)) => line,
            }
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
        let response = respond_and_journal(value, &journal).await;
        let Ok(text) = serde_json::to_string(&response) else {
            eprintln!("failed to serialize response");
            continue;
        };
        if let Err(err) = framed.send(text).await {
            eprintln!("failed to send response: {}", err);
            break;
        }
    }
}

/// Executes one request and journals it. Every request produces exactly one
/// journal entry and one response: computer actions are journaled here (qbt
/// is the only component that sees them all, in execution order) while
/// `publish_event` journals the agent's own event under its published kind.
async fn respond_and_journal(value: serde_json::Value, journal: &Journal) -> ComputerUseResponse {
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

    let id = request.id();
    let (response, journal_screenshot) = match handle_request(&request).await {
        Ok(outcome) => outcome,
        Err(error) => {
            eprintln!("error handling request: {}", error);
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
    let ok = !matches!(response, ComputerUseResponse::Error { .. });
    // The full request — including typed text — goes into the journal so
    // observers can see exactly what happened between screenshots. The
    // journal is in-memory, capped, and served only to the observatory;
    // streams that leave the machine must redact text themselves.
    journal.append(
        "computer.action",
        serde_json::json!({ "request": value, "ok": ok }),
        journal_screenshot,
    );
    response
}

    /// Executes the action and returns its response plus, for screen-capturing
    /// actions, the full-screen PNG for the journal.
    async fn handle_request(request: &ComputerUseRequest) -> anyhow::Result<(ComputerUseResponse, Option<Vec<u8>>)> {
        match request {
            ComputerUseRequest::PublishEvent { .. } => {
                // Handled before execution reaches here; see respond_and_journal.
                unreachable!("publish_event is not a computer action")
            }
            ComputerUseRequest::GetDisplayInfo { id } => {
                let (width, height) = pal::ScreenSampler::new()?.size_px();
                Ok((ComputerUseResponse::DisplayInfo {
                    id: *id,
                    ok: true,
                    display: ComputerUseDisplayInfo {
                        width_px: width,
                        height_px: height,
                    }
                }, None))
            }
            ComputerUseRequest::CursorPosition { id } => {
                let (x, y) = pal::cursor_position()?;
                Ok((ComputerUseResponse::Text {
                    id: *id,
                    ok: true,
                    text: format!("X={},Y={}", x, y)
                }, None))
            }
            ComputerUseRequest::Zoom { id, region } => {
                let (x0, y0, x1, y1) = *region;
                let (x0, x1) = (std::cmp::min(x0, x1), std::cmp::max(x0, x1));
                let (y0, y1) = (std::cmp::min(y0, y1), std::cmp::max(y0, y1));
                let (width, height) = (x1 - x0, y1 - y0);
                reply_screenshot(*id, Some((x0, y0, width, height))).await
            },
            ComputerUseRequest::Wait { id, duration_seconds, } => {
                tokio::time::sleep(Duration::from_secs_f64(*duration_seconds)).await;
                reply_screenshot(*id, None).await
            }
            ComputerUseRequest::Screenshot { id } => reply_screenshot(*id, None).await,
            ComputerUseRequest::MouseMove { id, coordinate: (x, y) } => {
                pal::mouse_move_to((*x as i32, *y as i32)).await?;
                Ok((ComputerUseResponse::Empty {
                    id: *id,
                    ok: true
                }, None))
            }
            ComputerUseRequest::LeftClick(params) |
            ComputerUseRequest::RightClick(params) |
            ComputerUseRequest::MiddleClick(params) |
            ComputerUseRequest::DoubleClick(params) |
            ComputerUseRequest::TripleClick(params) => {
                let MouseClickParams { id, key, coordinate } = params;
                if let Some(key) = key {
                    input::press_keys(key).await?;
                }
                if let Some((x, y)) = coordinate {
                    pal::mouse_move_to((*x as i32, *y as i32)).await?;
                }
                let (button, click_count) = request.mouse_clickiness().unwrap();
                for _ in 0..click_count {
                    pal::mouse_down(button).await?;
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    pal::mouse_up(button).await?;
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
                if let Some(key) = key {
                    input::release_keys(key).await?;
                }
                Ok((ComputerUseResponse::Empty {
                    id: *id,
                    ok: true,
                }, None))
            }
            ComputerUseRequest::LeftMouseDown { id, coordinate: (x, y) } => {
                pal::mouse_move_to((*x as i32, *y as i32)).await?;
                pal::mouse_down(MouseButton::Left).await?;
                Ok((ComputerUseResponse::Empty {
                    id: *id,
                    ok: true,
                }, None))
            }
            ComputerUseRequest::LeftMouseUp { id, coordinate: (x, y) } => {
                pal::mouse_move_to((*x as i32, *y as i32)).await?;
                pal::mouse_up(MouseButton::Left).await?;
                Ok((ComputerUseResponse::Empty {
                    id: *id,
                    ok: true,
                }, None))
            }
            ComputerUseRequest::LeftClickDrag { id, coordinate, start_coordinate } => {
                pal::mouse_move_to(((*start_coordinate).0 as i32, (*start_coordinate).1 as i32)).await?;
                pal::mouse_down(MouseButton::Left).await?;
                pal::mouse_move_to(((*coordinate).0 as i32, (*coordinate).1 as i32)).await?;
                pal::mouse_up(MouseButton::Left).await?;
                Ok((ComputerUseResponse::Empty {
                    id: *id,
                    ok: true,
                }, None))
            }
            ComputerUseRequest::Type { id, text } => {
                input::type_text(text).await?;
                Ok((ComputerUseResponse::Empty {
                    id: *id,
                    ok: true,
                }, None))
            }
            ComputerUseRequest::Key { id, text } => {
                input::press_release_keys(text).await?;
                Ok((ComputerUseResponse::Empty {
                    id: *id,
                    ok: true,
                }, None))
            }
            ComputerUseRequest::HoldKey { id, duration_seconds, text } => {
                input::hold_keys(text, Duration::from_secs_f64(*duration_seconds)).await?;
                Ok((ComputerUseResponse::Text {
                    id: *id,
                    ok: true,
                    text: "The specified delay will complete asynchronously.".into(),
                }, None))
            }
            ComputerUseRequest::Scroll { id, scroll_amount, scroll_direction, coordinate } => {
                if let Some((x, y)) = coordinate {
                    pal::mouse_move_to((*x as i32, *y as i32)).await?;
                }
                pal::mouse_scroll(scroll_amount, scroll_direction).await?;
                Ok((ComputerUseResponse::Empty {
                    id: *id,
                    ok: true,
                }, None))
            }
        }
    }

/// Takes a screenshot and builds the agent's response plus the full-screen
/// PNG for the journal. `bounds` is x0,y0,x1,y1, *not* width and height; the
/// agent gets the cropped view but the journal always gets the full screen,
/// so the observatory shows consistently sized screenshots.
async fn reply_screenshot(id: usize, bounds: Option<(usize, usize, usize, usize)>) -> anyhow::Result<(ComputerUseResponse, Option<Vec<u8>>)> {
    let screenshot = pal::screenshot()?;
    let cropped = {
        let (x, y, mut width, mut height) = bounds.unwrap_or((0, 0, screenshot.width() as usize, screenshot.height() as usize));
        width = std::cmp::min(width, screenshot.width() as usize - x);
        height = std::cmp::min(height, screenshot.height() as usize - y);
        screenshot.view(x as u32, y as u32, width as u32, height as u32).to_image()
    };

    let mut full_png_bytes = Vec::new();
    screenshot.write_to(&mut std::io::Cursor::new(&mut full_png_bytes), ImageFormat::Png)?;

    let mut png_bytes = Vec::new();
    cropped.write_to(&mut std::io::Cursor::new(&mut png_bytes), ImageFormat::Png)?;
    let base64_png_bytes = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
    Ok((ComputerUseResponse::Image {
        id,
        ok: true,
        image: ComputerUseImage {
            data: base64_png_bytes,
            media_type: "image/png".into(),
        }
    }, Some(full_png_bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    async fn start_test_server() -> (std::net::SocketAddr, Arc<Journal>, CancellationToken) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let journal = Journal::new();
        let shutdown = CancellationToken::new();
        tokio::spawn(serve_agent(listener, journal.clone(), shutdown.clone()));
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
}
