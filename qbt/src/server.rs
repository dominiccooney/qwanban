// See https://github.com/anthropics/claude-quickstarts/blob/main/computer-use-demo/computer_use_demo/tools/computer.py
// See https://github.com/anthropics/anthropic-sdk-typescript/blob/4f2eb8071993780d79610b9eda26db96f7653843/src/resources/beta/messages/messages.ts#L3283

use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::{Framed, LinesCodec};
use futures::{SinkExt, StreamExt};
use crate::pal;

#[derive(Deserialize)]
pub(crate) struct MouseClickParams {
    id: usize,
    key: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub(crate) enum ComputerUseRequest {
    Key { id: usize, text: String, },
    Type { id: usize, text: String,},
    MouseMove { id: usize, coordinate: (usize, usize) },
    LeftClick(MouseClickParams),
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

    // TODO: durationSeconds? Ditto for wait below.
    HoldKey { id: usize, duration_seconds: f64, text: String, },

    // Waits -> screenshot
    Wait { id: usize, duration_seconds: f64, },
    TripleClick(MouseClickParams),

    // Cropped screenshot, x0,y0,x1,y1
    Zoom { id: usize, region: (usize, usize, usize, usize) },

    // Not Claude events
    GetDisplayInfo { id: usize, },
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
#[serde(rename_all = "camelCase")]
pub(crate) struct ComputerUseResponse {
    id: usize,
    ok: bool,
    text: Option<String>,
    image: Option<ComputerUseImage>,
    error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RequestResponse {
    // Echoes the request id this response answers.
    id: usize,
    ok: bool,
    // Present when ok is false.
    error: Option<String>,
    // Human-readable result text (e.g. cursor position, ack message).
    text: Option<String>,
    // Screenshots, post-action screenshots.
    image: Option<ComputerUseImage>,
    display: Option<ComputerUseDisplayInfo>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ComputerUseDisplayInfo {
    width_px: usize,
    height_px: usize,
}

pub(crate) async fn start_jsonl_socket_server(port: u16) -> anyhow::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    println!("Listening on {}", port);
    loop {
        let (socket, _) = listener.accept().await?;
        tokio::spawn(async move {
            if let Err(e) = handle_client(socket).await {
                eprintln!("Error handling client: {}", e);
            }
        });
    }
}

async fn handle_client(socket: TcpStream) -> anyhow::Result<()> {
    let mut framed = Framed::new(socket, LinesCodec::new());
    while let Some(result) = framed.next().await {
        let line = result?;
        if line.trim().is_empty() {
            continue;
        }
        eprintln!("request: {}", line);
        match serde_json::from_str::<ComputerUseRequest>(&line) {
            Ok(request) => {
                handle_request(request, &mut framed).await?;
            }
            Err(e) => {
                eprintln!("invalid request: {:?}", e)
                // TODO: send some error
            }
        }
    }
    Ok(())
}

async fn handle_request(request: ComputerUseRequest, framed: &mut Framed<TcpStream, LinesCodec>) -> anyhow::Result<()> {
    match request {
        ComputerUseRequest::GetDisplayInfo { id } => {
            let (width, height) = pal::ScreenSampler::new()?.size_px();
            framed.send(serde_json::to_string(&RequestResponse {
                id,
                ok: true,
                error: None,
                text: None,
                display: Some(ComputerUseDisplayInfo {
                    width_px: width,
                    height_px: height,
                }),
                image: None,
            })?).await?;
            Ok(())
        }
        _ => {
            anyhow::bail!("NYI request type")
        }
    }
}