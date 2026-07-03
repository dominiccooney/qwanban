use clap::{CommandFactory, Parser, Subcommand};
use image::ImageError;

#[cfg(target_os = "windows")]
#[path = "pal/windows.rs"]
mod pal;
mod video;

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
}

#[tokio::main]
async fn main() {
    let args = Cli::parse();
    match &args.command {
        Some(CliCommand::Screenshot) => {
            let sampler = pal::ScreenSampler::new().unwrap();
            match sampler.screenshot() {
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
            }
        },
        Some(CliCommand::Video) => {
            video::encode_video_demo().await.unwrap();
        }
        None => {
            let mut cmd = Cli::command();
            cmd.print_help().unwrap();
            std::process::exit(1)
        }
    }
}