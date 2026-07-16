use anyhow::Context;
/// The websocket server which interacts with the observatory.

use tokio::net::TcpListener;
use tokio_tungstenite::{accept_async, WebSocketStream};
use futures_util::{StreamExt, SinkExt};
use image::ImageFormat;
use serde::Deserialize;
use tokio_tungstenite::tungstenite::Message;
use crate::pal;

pub(crate) enum ObservatoryToServerMessage {
    TakeScreenshot(tokio::sync::oneshot::Sender<anyhow::Result<pal::ScreenshotImage>>),
}

#[derive(Deserialize)]
pub(crate) enum ObservatoryToObservedMessage {
    TakeScreenshot,
}

pub(crate) enum ServerToObservatoryMessage {
    Screenshot(pal::ScreenshotImage),
}

pub(crate) struct Observed {
    to_server: tokio::sync::mpsc::Sender<ObservatoryToServerMessage>,
    from_server: tokio::sync::mpsc::Receiver<ServerToObservatoryMessage>,
}

impl Observed {
    pub(crate) fn new(to_server: tokio::sync::mpsc::Sender<ObservatoryToServerMessage>, from_server: tokio::sync::mpsc::Receiver<ServerToObservatoryMessage>) -> Self {
        Self {
            to_server,
            from_server,
        }
    }

    pub(crate) async fn serve_ws(&mut self, port: u16) -> anyhow::Result<()> {
        let listener = TcpListener::bind(("0.0.0.0", port)).await?;
        println!("WebSocket server listening on port: {}", port);
        loop {
            tokio::select! {
                Ok((stream, _)) = listener.accept() => {
                    let to_server_clone = self.to_server.clone();
                    tokio::spawn(async move {
                        let mut ws_stream = accept_async(stream).await.unwrap();
                        println!("New WebSocket connection: {:?}", ws_stream);
                        if let Err(err) = service_websocket_connection(&mut ws_stream, to_server_clone).await {
                            eprintln!("WebSocket connection error: {:?}", err);
                        }
                        println!("WebSocket connection closed: {:?}", ws_stream);
                    });
                }
                None = self.from_server.recv() => {
                    eprintln!("Shutting down websocket server");
                    break;
                }
            }
        }
        Ok(())
    }
}

async fn service_websocket_connection(ws_stream: &mut WebSocketStream<tokio::net::TcpStream>, to_server: tokio::sync::mpsc::Sender<ObservatoryToServerMessage>) -> anyhow::Result<()> {
    while let Some(Ok(msg)) = ws_stream.next().await {
        match serde_json::from_str(msg.to_text()?) {
            Err(err) => {
                eprintln!("Websocket error: {}", err);
            },
            Ok(msg) => {
                match msg {
                    ObservatoryToObservedMessage::TakeScreenshot => {
                        let (reply, recv) = tokio::sync::oneshot::channel();
                        to_server.send(ObservatoryToServerMessage::TakeScreenshot(reply)).await.context("failed to take screenshot")?;
                        // TODO: This is cheesy, the shutdown may happen during the screenshot.
                        let image = recv.await??;
                        let mut png_bytes = Vec::new();
                        image.write_to(&mut std::io::Cursor::new(&mut png_bytes), ImageFormat::Png)?;
                        ws_stream.send(Message::Binary(png_bytes.into())).await?;
                    }
                }
            }
        }
    }
    Ok(())
}