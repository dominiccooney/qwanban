use clap::{CommandFactory, Parser, Subcommand};

mod pal;
mod video;
mod input;
mod computer_use;
mod observed;
mod server;

#[derive(Parser)]
#[command(about = "Qwanban native support tools", name = "qbt")]
struct Cli {
    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Subcommand)]
enum CliCommand {
    Screenshot,
    Video,
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
        },
        Some(CliCommand::Video) => {
            video::offline_encode_video_demo().await
        }
        Some(CliCommand::Input) => {
            input::send_input_demo().await
        }
        Some(CliCommand::Serve { port, ws_port }) => {
            let server = server::Server::new(*port, *ws_port);
            eprintln!("ctrl-c to quit.");
            tokio::signal::ctrl_c().await?;
            eprintln!("Server shutting down");
            server.shutdown().await?;
            Ok(())
        }
        None => {
            let mut cmd = Cli::command();
            cmd.print_help()?;
            std::process::exit(1)
        }
    }
}