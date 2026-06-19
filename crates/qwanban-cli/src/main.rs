//! `qwanban` CLI — submit jobs and inspect reports. Thin layer over
//! `qwanban-core`'s `Orchestrator` (design §11).

use clap::{Parser, Subcommand};
use qwanban_core::job::{JobKind, JobSpec};

#[derive(Parser, Debug)]
#[command(name = "qwanban", version, about = "Run QA / bug-fix jobs in Hyper-V VMs")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run a scripted QA pass.
    RunQa {
        #[arg(long)]
        image: String,
        #[arg(long)]
        git_ref: String,
        /// Path to the markdown QA script.
        #[arg(long)]
        script: String,
        #[arg(long)]
        note: Option<String>,
    },
    /// Work on a bug report (reproduce + fix).
    FixBug {
        #[arg(long)]
        image: String,
        #[arg(long)]
        git_ref: String,
        /// Path to the bug report text.
        #[arg(long)]
        report: String,
        #[arg(long)]
        note: Option<String>,
    },
}

fn read_text(path: &str) -> anyhow::Result<String> {
    Ok(std::fs::read_to_string(path)?)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let cli = Cli::parse();
    let spec = match cli.cmd {
        Cmd::RunQa { image, git_ref, script, note } => JobSpec {
            kind: JobKind::ScriptedQa,
            base_image: image,
            git_ref,
            task_text: read_text(&script)?,
            note,
            caps: None,
        },
        Cmd::FixBug { image, git_ref, report, note } => JobSpec {
            kind: JobKind::BugFix,
            base_image: image,
            git_ref,
            task_text: read_text(&report)?,
            note,
            caps: None,
        },
    };
    // TODO: construct a real Orchestrator (Hyper-V driver + broker + vault) and
    // submit. For now, print the parsed spec so the CLI is wired end-to-end.
    let json = serde_json::to_string_pretty(&spec)?;
    println!("{json}");
    Ok(())
}
