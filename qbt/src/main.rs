use clap::{CommandFactory, Parser, Subcommand};
use tokio_util::sync::CancellationToken;

mod computer_use;
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
    Serve { port: u16, ws_port: Option<u16> },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Cli::parse();
    match &args.command {
        Some(CliCommand::Screenshot) => {
            let sampler = pal::ScreenSampler::new()?;
            sampler.screenshot()?.save("screenshot.png")?;
            Ok(())
        }
        Some(CliCommand::Input) => input::send_input_demo().await,
        Some(CliCommand::Serve { port, ws_port }) => {
            // One journal is the sole source of truth: the agent server
            // writes computer actions and published events into it; the
            // observatory server only reads. Shutdown is one cancellation
            // token shared by both servers and all their connections.
            let journal = journal::Journal::new();
            let shutdown = CancellationToken::new();

            let agent_listener = tokio::net::TcpListener::bind(("127.0.0.1", *port)).await?;
            eprintln!("agent server listening on {}", port);
            let agent_server = tokio::spawn(computer_use::serve_agent(
                agent_listener,
                journal.clone(),
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

            eprintln!("ctrl-c to quit.");
            tokio::signal::ctrl_c().await?;
            eprintln!("Server shutting down");
            shutdown.cancel();
            agent_server.await?;
            if let Some(observatory_server) = observatory_server {
                observatory_server.await?;
            }
            Ok(())
        }
        None => {
            let mut cmd = Cli::command();
            cmd.print_help()?;
            std::process::exit(1)
        }
    }
}
