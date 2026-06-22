//! `qwan-bootstrapd` - the TCP stub-loader daemon. Binds a TCP listener on a
//! port inside the guest VM, accepts host connections over the private vSwitch,
//! and runs the bootstrap `serve()` loop on each.
//!
//! Usage:
//!   qwan-bootstrapd --bind 0.0.0.0:7474 --work-dir <DIR> --secret <SECRET>
//!
//! Run this inside the guest VM (the one being driven by the host harness).
//! It stays up and persistent, accepting one connection at a time.

use qwanban_stub::{serve, ServeConfig};
use std::path::PathBuf;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("qwanban=info".parse()?),
        )
        .init();

    let mut bind_addr = String::new();
    let mut work_dir = PathBuf::new();
    let mut secret = String::new();
    let mut stub_version: u32 = 1;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--bind" => bind_addr = args.next().expect("--bind needs a value"),
            "--work-dir" => work_dir = PathBuf::from(args.next().expect("--work-dir needs a value")),
            "--secret" => secret = args.next().expect("--secret needs a value"),
            "--stub-version" => stub_version = args.next().expect("value").parse()?,
            _ => eprintln!("unknown arg: {arg}"),
        }
    }

    if bind_addr.is_empty() || secret.is_empty() {
        anyhow::bail!("--bind and --secret are required");
    }

    std::fs::create_dir_all(&work_dir)?;
    tracing::info!(%bind_addr, ?work_dir, "qwan-bootstrapd binding TCP listener");

    let listener = TcpListener::bind(&bind_addr).await?;
    tracing::info!("listening; waiting for host connection...");

    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                tracing::info!(%peer, "host connected");
                let cfg = ServeConfig {
                    work_dir: work_dir.clone(),
                    case_bootstrap_secret: secret.clone(),
                    expected_stub_version: stub_version,
                };
                let outcome = serve(stream, &cfg).await;
                tracing::info!(?outcome, "connection finished");
            }
            Err(e) => {
                tracing::error!(%e, "accept failed");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
    }
}
