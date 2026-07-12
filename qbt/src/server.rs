// See https://github.com/anthropics/claude-quickstarts/blob/main/computer-use-demo/computer_use_demo/tools/computer.py
// See https://github.com/anthropics/anthropic-sdk-typescript/blob/4f2eb8071993780d79610b9eda26db96f7653843/src/resources/beta/messages/messages.ts#L3283

use std::time::Duration;
use base64::Engine;
use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::{Framed, LinesCodec};
use futures::{SinkExt, StreamExt};
use image::{GenericImageView, ImageFormat};
use crate::{input, pal};
use crate::pal::{MouseButton, ScreenSampler};

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
                handle_request_report_error(request, &mut framed).await?;
            }
            Err(e) => {
                eprintln!("invalid request: {:?}", e)
                // We can't respond to these requests because we didn't parse an ID.
            }
        }
    }
    Ok(())
}

async fn handle_request_report_error(request: ComputerUseRequest, framed: &mut Framed<TcpStream, LinesCodec>) -> anyhow::Result<()> {
    let id = request.id();
    if let Err(error) = handle_request(request, framed).await {
        eprintln!("error handling request: {}", error);
        framed.send(serde_json::to_string(&ComputerUseResponse::Error {
            id,
            ok: false,
            error: format!("{}", error),
        })?).await?;
    }
    Ok(())
}

// Note: bounds is x0,y0,x1,y1, *not* width and height.
async fn reply_screenshot(framed: &mut Framed<TcpStream, LinesCodec>, id: usize, bounds: Option<(usize, usize, usize, usize)>) -> anyhow::Result<()> {
    let screenshot = ScreenSampler::new()?.screenshot()?;
    let cropped = {
        let (x, y, mut width, mut height) = bounds.unwrap_or((0, 0, screenshot.width() as usize, screenshot.height() as usize));
        width = std::cmp::min(width, screenshot.width() as usize - x);
        height = std::cmp::min(height, screenshot.height() as usize - y);
        screenshot.view(x as u32, y as u32, width as u32, height as u32).to_image()
    };
    let mut png_bytes = Vec::new();
    cropped.write_to(&mut std::io::Cursor::new(&mut png_bytes), ImageFormat::Png)?;
    let base64_png_bytes = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
    framed.send(serde_json::to_string(&ComputerUseResponse::Image {
        id,
        ok: true,
        image: ComputerUseImage {
            data: base64_png_bytes,
            media_type: "image/png".into(),
        }
    })?).await?;
    Ok(())
}

async fn handle_request(request: ComputerUseRequest, framed: &mut Framed<TcpStream, LinesCodec>) -> anyhow::Result<()> {
    match &request {
        ComputerUseRequest::GetDisplayInfo { id } => {
            let (width, height) = pal::ScreenSampler::new()?.size_px();
            framed.send(serde_json::to_string(&ComputerUseResponse::DisplayInfo {
                id: *id,
                ok: true,
                display: ComputerUseDisplayInfo {
                    width_px: width,
                    height_px: height,
                }
            })?).await?;
            Ok(())
        }
        ComputerUseRequest::CursorPosition { id } => {
            let (x, y) = pal::cursor_position()?;
            framed.send(serde_json::to_string(&ComputerUseResponse::Text {
                id: *id,
                ok: true,
                text: format!("X={},Y={}", x, y)
            })?).await?;
            Ok(())
        }
        ComputerUseRequest::Zoom { id, region } => {
            let (x0, y0, x1, y1) = *region;
            let (x0, x1) = (std::cmp::min(x0, x1), std::cmp::max(x0, x1));
            let (y0, y1) = (std::cmp::min(y0, y1), std::cmp::max(y0, y1));
            let (width, height) = (x1 - x0, y1 - y0);
            reply_screenshot(framed, *id, Some((x0, y0, width, height))).await
        },
        ComputerUseRequest::Wait { id, duration_seconds, } => {
            tokio::time::sleep(Duration::from_secs_f64(*duration_seconds)).await;
            reply_screenshot(framed, *id, None).await
        }
        ComputerUseRequest::Screenshot { id } => reply_screenshot(framed, *id, None).await,
        ComputerUseRequest::MouseMove { id, coordinate: (x, y) } => {
            pal::mouse_move_to((*x as i32, *y as i32)).await?;
            framed.send(serde_json::to_string(&ComputerUseResponse::Empty {
                id: *id,
                ok: true
            })?).await?;
            Ok(())
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
            framed.send(serde_json::to_string(&ComputerUseResponse::Empty {
                id: *id,
                ok: true,
            })?).await?;
            Ok(())
        }
        ComputerUseRequest::LeftMouseDown { id, coordinate: (x, y) } => {
            pal::mouse_move_to((*x as i32, *y as i32)).await?;
            pal::mouse_down(MouseButton::Left).await?;
            framed.send(serde_json::to_string(&ComputerUseResponse::Empty {
                id: *id,
                ok: true,
            })?).await?;
            Ok(())
        }
        ComputerUseRequest::LeftMouseUp { id, coordinate: (x, y) } => {
            pal::mouse_move_to((*x as i32, *y as i32)).await?;
            pal::mouse_up(MouseButton::Left).await?;
            framed.send(serde_json::to_string(&ComputerUseResponse::Empty {
                id: *id,
                ok: true,
            })?).await?;
            Ok(())
        }
        ComputerUseRequest::LeftClickDrag { id, coordinate, start_coordinate } => {
            pal::mouse_move_to(((*start_coordinate).0 as i32, (*start_coordinate).1 as i32)).await?;
            pal::mouse_down(MouseButton::Left).await?;
            pal::mouse_move_to(((*coordinate).0 as i32, (*coordinate).1 as i32)).await?;
            pal::mouse_up(MouseButton::Left).await?;
            framed.send(serde_json::to_string(&ComputerUseResponse::Empty {
                id: *id,
                ok: true,
            })?).await?;
            Ok(())
        }
        ComputerUseRequest::Type { id, text } => {
            input::type_text(text).await?;
            framed.send(serde_json::to_string(&ComputerUseResponse::Empty {
                id: *id,
                ok: true,
            })?).await?;
            Ok(())
        }
        ComputerUseRequest::Key { id, text } => {
            input::press_release_keys(text).await?;
            framed.send(serde_json::to_string(&ComputerUseResponse::Empty {
                id: *id,
                ok: true,
            })?).await?;
            Ok(())
        }
        ComputerUseRequest::HoldKey { id, duration_seconds, text } => {
            input::hold_keys(text, Duration::from_secs_f64(*duration_seconds)).await?;
            framed.send(serde_json::to_string(&ComputerUseResponse::Text {
                id: *id,
                ok: true,
                text: "The specified delay will complete asynchronously.".into(),
            })?).await?;
            Ok(())
        }
        ComputerUseRequest::Scroll { id, scroll_amount, scroll_direction, coordinate } => {
            if let Some((x, y)) = coordinate {
                pal::mouse_move_to((*x as i32, *y as i32)).await?;
            }
            pal::mouse_scroll(scroll_amount, scroll_direction).await?;
            framed.send(serde_json::to_string(&ComputerUseResponse::Empty {
                id: *id,
                ok: true,
            })?).await?;
            Ok(())
        }
    }
}