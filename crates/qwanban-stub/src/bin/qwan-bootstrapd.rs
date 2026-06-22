//! `qwan-bootstrapd` - the real hvsocket stub-loader daemon. Binds an AF_HYPERV
//! listener on a service GUID inside the guest VM, accepts host connections,
//! and runs the bootstrap `serve()` loop on each.
//!
//! Usage:
//!   qwan-bootstrapd --service-guid <GUID> --work-dir <DIR> --secret <SECRET>
//!
//! Run this inside the guest VM (the one being driven by the host harness).
//! It stays up and persistent, accepting one connection at a time.

use qwanban_hyperv::hvsocket::HvSocketListener;
use qwanban_stub::{serve, ServeConfig};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("qwanban=info".parse()?))
        .init();

    let mut service_guid = String::new();
    let mut work_dir = PathBuf::new();
    let mut secret = String::new();
    let mut stub_version: u32 = 1;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--service-guid" => service_guid = args.next().expect("--service-guid needs a value"),
            "--work-dir" => work_dir = PathBuf::from(args.next().expect("--work-dir needs a value")),
            "--secret" => secret = args.next().expect("--secret needs a value"),
            "--stub-version" => stub_version = args.next().expect("--stub-version needs a value").parse()?,
            _ => eprintln!("unknown arg: {arg}"),
        }
    }

    if service_guid.is_empty() || secret.is_empty() {
        anyhow::bail!("--service-guid and --secret are required");
    }

    std::fs::create_dir_all(&work_dir)?;
    tracing::info!(%service_guid, ?work_dir, "qwan-bootstrapd binding hvsocket listener");

    let listener = HvSocketListener::bind(&service_guid).map_err(|e| {
        anyhow::anyhow!("hvsocket bind failed (is vmicguestinterface running + service GUID registered?): {e}")
    })?;
    tracing::info!("listening; waiting for host connection...");

    loop {
        match listener.accept() {
            Ok(stream) => {
                tracing::info!("host connected");
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
