use std::path::PathBuf;

use clap::{CommandFactory, Parser, Subcommand};
use image::ImageFormat;
use tokio_util::sync::CancellationToken;

mod artifacts;
mod computer_use;
mod flipbook;
mod input;
mod journal;
mod observed;
mod pal;

#[derive(Parser)]
#[command(about = "Qwanban native support tools", name = "qbt")]
struct Cli {
    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Subcommand)]
enum CliCommand {
    Screenshot,
    Input,
    Serve {
        port: u16,
        ws_port: Option<u16>,
        #[arg(long, default_value = ".")]
        artifact_root: PathBuf,
        #[arg(long, default_value_t = journal::DEFAULT_MAX_SCREENSHOTS)]
        max_screenshots: usize,
    },
    Flipbook {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 2.0)]
        fps: f64,
        #[arg(long, default_value = "ws://127.0.0.1:5678")]
        observatory: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Cli::parse();
    match &args.command {
        Some(CliCommand::Screenshot) => {
            pal::screenshot()?.image.save("screenshot.png")?;
            Ok(())
        }
        Some(CliCommand::Input) => input::send_input_demo().await,
        Some(CliCommand::Serve {
            port,
            ws_port,
            artifact_root,
            max_screenshots,
        }) => {
            anyhow::ensure!(
                *max_screenshots > 0,
                "--max-screenshots must be greater than zero"
            );
            // One journal is the sole source of truth: the agent server
            // writes computer actions and published events into it; the
            // observatory server only reads. Shutdown is one cancellation
            // token shared by both servers and all their connections.
            let journal = journal::Journal::new(*max_screenshots);
            let artifacts = std::sync::Arc::new(artifacts::ArtifactStore::new(artifact_root)?);
            let shutdown = CancellationToken::new();

            eprintln!("artifact root: {}", artifacts.root().display());

            let agent_listener = tokio::net::TcpListener::bind(("127.0.0.1", *port)).await?;
            eprintln!("agent server listening on {}", port);
            let agent_server = tokio::spawn(computer_use::serve_agent(
                agent_listener,
                journal.clone(),
                artifacts,
                shutdown.clone(),
            ));

            let observatory_server = if let Some(ws_port) = ws_port {
                let listener = tokio::net::TcpListener::bind(("0.0.0.0", *ws_port)).await?;
                eprintln!("observatory server listening on {}", ws_port);
                Some(tokio::spawn(observed::serve_observatory(
                    listener,
                    journal.clone(),
                    shutdown.clone(),
                )))
            } else {
                None
            };

            let estimate_capacity = *max_screenshots;
            std::thread::Builder::new()
                .name("qbt-memory-estimate".into())
                .spawn(move || print_screenshot_memory_estimate(estimate_capacity))?;

            eprintln!("ctrl-c to quit.");
            tokio::select! {
                result = tokio::signal::ctrl_c() => result?,
                _ = shutdown.cancelled() => {}
            }
            eprintln!("Server shutting down");
            shutdown.cancel();
            let agent_result = agent_server.await;
            let observatory_result = match observatory_server {
                Some(observatory_server) => Some(observatory_server.await),
                None => None,
            };
            let clipboard_result = pal::shutdown_clipboard();
            agent_result?;
            if let Some(observatory_result) = observatory_result {
                observatory_result?;
            }
            clipboard_result?;
            Ok(())
        }
        Some(CliCommand::Flipbook {
            from,
            to,
            out,
            fps,
            observatory,
        }) => flipbook::create(observatory, from, to, out, *fps).await,
        None => {
            let mut cmd = Cli::command();
            cmd.print_help()?;
            std::process::exit(1)
        }
    }
}

fn print_screenshot_memory_estimate(max_screenshots: usize) {
    let estimate = (|| -> anyhow::Result<usize> {
        let screenshot = pal::screenshot()?;
        let mut png = Vec::new();
        screenshot
            .image
            .write_to(&mut std::io::Cursor::new(&mut png), ImageFormat::Png)?;
        Ok(png.len())
    })();
    match estimate {
        Ok(last_png_size) => {
            let estimated_bytes = max_screenshots as u128 * last_png_size as u128;
            let estimated_mib = estimated_bytes as f64 / (1024.0 * 1024.0);
            eprintln!(
                "screenshot buffer capacity: {max_screenshots}; estimated memory: {estimated_bytes} bytes ({max_screenshots} × {last_png_size}-byte PNG, {estimated_mib:.1} MiB)"
            );
        }
        Err(error) => eprintln!(
            "screenshot buffer capacity: {max_screenshots}; estimated memory unavailable (startup PNG sample failed: {error})"
        ),
    }
}
