//! The WebSocket server the observatory connects to.
//!
//! Observers are pure subscribers of the journal: on connect they receive a
//! snapshot of recent events, then live events as they are appended, all in
//! journal order. Screenshots are fetched by id (`{"fetchScreenshot": id}`)
//! and returned as binary frames prefixed with the id line, so the client
//! can correlate them. Any number of observers may connect; none of them
//! keeps any other component alive.

use std::sync::Arc;
use futures_util::{SinkExt, StreamExt};
use image::ImageFormat;
use serde::Deserialize;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use crate::journal::Journal;
use crate::pal::ScreenSampler;

fn take_screenshot_png() -> anyhow::Result<Vec<u8>> {
    let screenshot = ScreenSampler::new()?.screenshot()?;
    let mut png = Vec::new();
    screenshot.write_to(&mut std::io::Cursor::new(&mut png), ImageFormat::Png)?;
    Ok(png)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum ObservatoryRequest {
    FetchScreenshot(String),
    /// Take a screenshot now, journaled as an `observer.screenshot` event.
    /// The requesting observer receives it through its own live stream like
    /// everyone else, then fetches the image by id.
    TakeScreenshot,
}

pub(crate) async fn serve_observatory(
    listener: TcpListener,
    journal: Arc<Journal>,
    shutdown: CancellationToken,
) {
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, peer)) => {
                        eprintln!("observer connected: {}", peer);
                        let journal = journal.clone();
                        let cancel = shutdown.child_token();
                        tokio::spawn(async move {
                            if let Err(err) = serve_observer(stream, journal, cancel).await {
                                eprintln!("observer connection ended: {}", err);
                            }
                        });
                    }
                    Err(err) => {
                        eprintln!("error accepting observer socket: {}", err);
                    }
                }
            }
            _ = shutdown.cancelled() => break,
        }
    }
}

async fn serve_observer(
    stream: TcpStream,
    journal: Arc<Journal>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let mut ws = accept_async(stream).await?;
    // Subscribe before sending the snapshot so no event can fall in a gap;
    // events which race into both are deduplicated by seq below.
    let (mut live, snapshot) = journal.subscribe_with_snapshot();
    let mut last_sent_seq = 0u64;
    for event in snapshot {
        ws.send(Message::Text(serde_json::to_string(&*event)?.into())).await?;
        last_sent_seq = event.seq;
    }
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            event = live.recv() => {
                match event {
                    Ok(event) => {
                        if event.seq <= last_sent_seq {
                            continue;
                        }
                        last_sent_seq = event.seq;
                        ws.send(Message::Text(serde_json::to_string(&*event)?.into())).await?;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                        // A slow observer missed events. It can see them in
                        // the seq gap; keep streaming from here.
                        eprintln!("observer lagged; {} events dropped", missed);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            incoming = ws.next() => {
                let Some(Ok(msg)) = incoming else { break };
                let Ok(text) = msg.to_text() else { continue };
                match serde_json::from_str::<ObservatoryRequest>(text) {
                    Ok(ObservatoryRequest::TakeScreenshot) => {
                        match take_screenshot_png() {
                            Ok(png) => {
                                journal.append("observer.screenshot", serde_json::json!({}), Some(png));
                            }
                            Err(err) => {
                                eprintln!("observer screenshot failed: {}", err);
                            }
                        }
                    }
                    Ok(ObservatoryRequest::FetchScreenshot(id)) => {
                        match journal.screenshot(&id) {
                            Some(png) => {
                                // Prefix the binary frame with the id so the
                                // client can correlate the reply.
                                let mut framed = id.clone().into_bytes();
                                framed.push(b'\n');
                                framed.extend_from_slice(&png);
                                ws.send(Message::Binary(framed.into())).await?;
                            }
                            None => {
                                ws.send(Message::Text(
                                    serde_json::json!({ "missingScreenshot": id }).to_string().into(),
                                )).await?;
                            }
                        }
                    }
                    Err(err) => {
                        eprintln!("unparseable observer request: {}", err);
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::connect_async;

    async fn start_test_server(journal: Arc<Journal>) -> (std::net::SocketAddr, CancellationToken) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let shutdown = CancellationToken::new();
        tokio::spawn(serve_observatory(listener, journal, shutdown.clone()));
        (addr, shutdown)
    }

    #[tokio::test]
    async fn snapshot_then_live_events_in_order() {
        let journal = Journal::new();
        journal.append("test.before", serde_json::json!({"n": 1}), None);
        let (addr, _shutdown) = start_test_server(journal.clone()).await;

        let (mut ws, _) = connect_async(format!("ws://{}", addr)).await.unwrap();
        let snapshot_msg = ws.next().await.unwrap().unwrap();
        let snapshot: serde_json::Value =
            serde_json::from_str(snapshot_msg.to_text().unwrap()).unwrap();
        assert_eq!(snapshot["kind"], "test.before");

        journal.append("test.after", serde_json::json!({"n": 2}), None);
        let live_msg = ws.next().await.unwrap().unwrap();
        let live: serde_json::Value =
            serde_json::from_str(live_msg.to_text().unwrap()).unwrap();
        assert_eq!(live["kind"], "test.after");
        assert!(live["seq"].as_u64().unwrap() > snapshot["seq"].as_u64().unwrap());
    }

    #[tokio::test]
    async fn fetches_screenshots_by_id() {
        let journal = Journal::new();
        let event = journal.append("test.shot", serde_json::json!({}), Some(vec![9, 8, 7]));
        let id = event.screenshot_id.clone().unwrap();
        let (addr, _shutdown) = start_test_server(journal.clone()).await;

        let (mut ws, _) = connect_async(format!("ws://{}", addr)).await.unwrap();
        // Skip the snapshot event.
        ws.next().await.unwrap().unwrap();

        ws.send(Message::Text(
            serde_json::json!({ "fetchScreenshot": id }).to_string().into(),
        ))
        .await
        .unwrap();
        let reply = ws.next().await.unwrap().unwrap();
        let Message::Binary(bytes) = reply else {
            panic!("expected a binary frame");
        };
        let newline = bytes.iter().position(|byte| *byte == b'\n').unwrap();
        assert_eq!(&bytes[..newline], id.as_bytes());
        assert_eq!(&bytes[newline + 1..], &[9, 8, 7]);
    }
}
