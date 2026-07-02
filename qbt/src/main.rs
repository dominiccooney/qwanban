use clap::{CommandFactory, Parser, Subcommand};
use image::ImageError;

#[cfg(target_os = "windows")]
#[path = "pal/windows.rs"]
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
}

fn main() {
    let args = Cli::parse();
    match &args.command {
        Some(CliCommand::Screenshot) => match pal::screenshot() {
            Ok(screenshot) => screenshot
                .save("screenshot.png")
                .map_err(|err: ImageError| -> anyhow::Result<()> {
                    eprintln!("could not save screenshot: {:?}", err);
                    std::process::exit(1);
                })
                .unwrap(),
            Err(err) => {
                eprintln!("{:?}", err);
                std::process::exit(1)
            }
        },
        None => {
            let mut cmd = Cli::command();
            cmd.print_help().unwrap();
            std::process::exit(1)
        }
    }
}
