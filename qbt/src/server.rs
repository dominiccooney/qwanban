use std::sync::Arc;
use crate::computer_use::{ComputerUse, ComputerUseToServerMessage, ServerToComputerUseMessage};
use crate::observed::{ObservatoryToServerMessage, ServerToObservatoryMessage};

/// Coordinates the WebSocket server that streams status out to the observatory; and the computer
/// use server that responds to requests from the agent.

struct Inner {
    last_screenshot: Option<crate::pal::ScreenshotImage>,
}

pub(crate) struct Server {
    to_computer_use: tokio::sync::mpsc::Sender<ServerToComputerUseMessage>,
    to_observatory: Option<tokio::sync::mpsc::Sender<ServerToObservatoryMessage>>,
    inner: Arc<tokio::sync::Mutex<Inner>>,
}

impl Server {
    pub(crate) fn new(jsonl_port: u16, ws_port: Option<u16>) -> Self {
        let inner = Arc::new(tokio::sync::Mutex::new(Inner {
            last_screenshot: None,
        }));

        let (to_computer_use, from_server) = tokio::sync::mpsc::channel(10);
        let (to_server, mut from_computer_use) = tokio::sync::mpsc::channel(10);

        // Run the computer use server for the agent.
        tokio::task::spawn(async move {
            let computer_use = Arc::new(ComputerUse::new(to_server, from_server));
            computer_use.start_jsonl_socket_server(jsonl_port).await.unwrap();
        });

        // Handle messages from the computer use server.
        let inner_clone = inner.clone();
        tokio::task::spawn(async move {
            while let Some(message) = from_computer_use.recv().await {
                match message {
                    ComputerUseToServerMessage::TookScreenshot(screenshot) => {
                        inner_clone.lock().await.last_screenshot = Some(screenshot);
                    }
                }
            }
        });

        let to_observatory =
            if let Some(ws_port) = ws_port {
                // Run the websocket server for the observatory.
                let (to_observatory, from_server) = tokio::sync::mpsc::channel(10);
                let (to_server, mut from_observed) = tokio::sync::mpsc::channel(10);
                tokio::task::spawn(async move {
                    let mut ws_server = crate::observed::Observed::new(to_server, from_server);
                    // TODO: handle these errors gracefully
                    ws_server.serve_ws(ws_port).await.unwrap();
                });

                // TODO: Shutdown had relied on dropping to_computer_use, but now this copy lives indefinitely and we are stuck.
                let to_computer_use_clone = to_computer_use.clone();
                // Service requests from the observatory.
                tokio::task::spawn(async move {
                    while let Some(message) = from_observed.recv().await {
                        match message {
                            ObservatoryToServerMessage::TakeScreenshot(reply) => {
                                to_computer_use_clone.send(ServerToComputerUseMessage::TakeScreenshot(reply)).await.unwrap();
                            }
                        }
                    }
                });

                Some(to_observatory)
            } else {
                None
            };

        Self {
            to_computer_use,
            to_observatory,
            inner,
        }
    }

    pub(crate) async fn shutdown(self) -> anyhow::Result<()> {
        // TODO: Implement proper shutdown.
        drop(self.to_computer_use);
        Ok(())
    }
}