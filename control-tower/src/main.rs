use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use control_tower::{cloud, config::Config, controller::App, model::Submission};
use std::future::IntoFuture;
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Parser)]
#[command(about = "Provision, observe and clean up computer-use workers")]
struct Cli {
    #[arg(long)]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Serve,
    Submit {
        #[arg(long)]
        worker_profile: String,
        #[arg(long)]
        title: String,
        #[arg(long, default_value = "cli")]
        source: String,
        #[arg(long)]
        prompt: Option<PathBuf>,
        #[arg(long)]
        setup_script: Option<PathBuf>,
        #[arg(long)]
        id: Option<Uuid>,
    },
    List,
    Cancel {
        id: Uuid,
    },
    Retry {
        id: Uuid,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let path = std::fs::canonicalize(cli.config)?;
    let config = Config::load(&path)?;
    if matches!(cli.command, Command::Serve) {
        return serve(config, path).await;
    }
    let client = cloud::client(config.limits.request_timeout_secs)?;
    let base = format!("http://{}", config.operator_addr);
    let response = match cli.command {
        Command::Submit {
            worker_profile,
            title,
            source,
            prompt,
            setup_script,
            id,
        } => {
            ensure!(
                prompt.is_some() || setup_script.is_some(),
                "provide --prompt and/or --setup-script"
            );
            let submission = Submission {
                id: id.unwrap_or_else(Uuid::new_v4),
                worker_profile,
                title,
                source,
                prompt: prompt
                    .map(std::fs::read_to_string)
                    .transpose()?
                    .unwrap_or_default(),
                setup_script: setup_script
                    .map(std::fs::read_to_string)
                    .transpose()?
                    .unwrap_or_default(),
            };
            eprintln!("submission ID: {} (reuse --id if retrying)", submission.id);
            client
                .post(format!("{base}/api/jobs"))
                .json(&submission)
                .send()
                .await?
        }
        Command::List => client.get(format!("{base}/api/jobs")).send().await?,
        Command::Cancel { id } => {
            client
                .post(format!("{base}/api/jobs/{id}/cancel"))
                .json(&serde_json::json!({}))
                .send()
                .await?
        }
        Command::Retry { id } => {
            client
                .post(format!("{base}/api/jobs/{id}/retry"))
                .json(&serde_json::json!({}))
                .send()
                .await?
        }
        Command::Serve => unreachable!(),
    };
    let status = response.status();
    let bytes = cloud::response_bytes(response, 16 * 1024 * 1024).await?;
    ensure!(
        status.is_success(),
        "Control Tower: {status}: {}",
        String::from_utf8_lossy(&bytes)
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::from_slice::<serde_json::Value>(&bytes)?)?
    );
    Ok(())
}

async fn serve(config: Config, path: PathBuf) -> Result<()> {
    let operator = tokio::net::TcpListener::bind(config.operator_addr).await?;
    let worker = tokio::net::TcpListener::bind(config.worker_addr).await?;
    let credentials = google_cloud_auth::credentials::Builder::default()
        .build()
        .context("load Application Default Credentials")?;
    let app = App::open(config, credentials)?;
    app.retain().await?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let controller = tokio::spawn(app.clone().run(path, cancel.clone()));
    let operator_cancel = cancel.clone();
    let worker_cancel = cancel.clone();
    let mut operator_task = tokio::spawn(
        axum::serve(
            operator,
            control_tower::http::operator(app.clone())
                .into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(async move { operator_cancel.cancelled().await })
        .into_future(),
    );
    let mut worker_task = tokio::spawn(
        axum::serve(
            worker,
            control_tower::http::worker(app.clone())
                .into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(async move { worker_cancel.cancelled().await })
        .into_future(),
    );
    let mut controller = controller;
    eprintln!(
        "Control Tower operator: {}; worker: {}",
        app.config.read().await.operator_addr,
        app.config.read().await.worker_addr
    );
    let result = tokio::select! {
        result=tokio::signal::ctrl_c()=>result.context("wait for shutdown"),
        result=&mut controller=>match result {Ok(result)=>result,Err(e)=>Err(e.into())},
        result=&mut operator_task=>match result{Ok(Ok(()))=>Err(anyhow::anyhow!("operator listener stopped")),Ok(Err(e))=>Err(e.into()),Err(e)=>Err(e.into())},
        result=&mut worker_task=>match result{Ok(Ok(()))=>Err(anyhow::anyhow!("worker listener stopped")),Ok(Err(e))=>Err(e.into()),Err(e)=>Err(e.into())},
    };
    cancel.cancel();
    // Dropping tasks cannot cancel already-issued cloud requests. The ledger
    // retains their intent and recovery settles them on the next startup.
    operator_task.abort();
    worker_task.abort();
    controller.abort();
    result
}
