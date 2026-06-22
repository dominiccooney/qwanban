//! `host-harness` - the host-side bootstrap driver. Connects to a real guest VM
//! via TCP over the private vSwitch and drives the full bootstrap handshake:
//! HELLO -> AUTH -> PUSH_AGENT -> WriteFile -> LAUNCH -> STREAM -> Exit.
//!
//! Usage:
//!   host-harness --addr <GUEST-IP:PORT> --secret <SECRET> [--stub-version 1]
//!                [--agent-path ./fake-agent] [--work-dir ./qwan-harness-work]

use qwanban_stub::protocol::*;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn hex_sha256(b: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b);
    let out = h.finalize();
    out.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut addr = String::new();
    let mut secret = String::new();
    let mut stub_version: u32 = 1;
    let mut agent_path = std::path::PathBuf::from("./fake-agent");
    let mut work_dir = std::path::PathBuf::from(".");

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--addr" => addr = args.next().expect("--addr needs a value"),
            "--secret" => secret = args.next().expect("--secret needs a value"),
            "--stub-version" => stub_version = args.next().expect("value").parse()?,
            "--agent-path" => agent_path = std::path::PathBuf::from(args.next().expect("value")),
            "--work-dir" => work_dir = std::path::PathBuf::from(args.next().expect("value")),
            other => anyhow::bail!("unknown arg: {other}"),
        }
    }

    if addr.is_empty() || secret.is_empty() {
        anyhow::bail!("--addr and --secret are required");
    }

    println!("[harness] connecting to {addr} ...");
    let mut stream = TcpStream::connect(&addr).await?;
    println!("[harness] connected!");

    // HELLO
    write_frame(&mut stream, &Frame::Hello(Hello {
        stub_version, os: qwanban_proto::broker::GuestOs::Windows, arch: "x86_64".into(),
    })).await?;
    // AUTH
    write_frame(&mut stream, &Frame::Auth { case_bootstrap_secret: secret }).await?;
    let auth_ack = read_frame(&mut stream).await?;
    if !is_ok(&auth_ack) {
        anyhow::bail!("auth rejected: {auth_ack:?}");
    }
    println!("[harness] HELLO + AUTH OK");

    // PUSH_AGENT
    let agent_bytes = std::fs::read(&agent_path).unwrap_or_else(|_| {
        b"#!/bin/sh\necho qwan-guest up\n".to_vec()
    });
    let hash = hex_sha256(&agent_bytes);
    write_frame(&mut stream, &Frame::PushAgent(PushAgent { sha256: hash, len: agent_bytes.len() as u64 })).await?;
    write_payload(&mut stream, &agent_bytes).await?;
    let push_ack = read_frame(&mut stream).await?;
    if !is_ok(&push_ack) { anyhow::bail!("push_agent failed: {push_ack:?}"); }
    println!("[harness] PUSH_AGENT OK ({} bytes)", agent_bytes.len());

    // WriteFile
    let manifest = br#"{"schema":"qwan.manifest/v1"}"#;
    write_frame(&mut stream, &Frame::WriteFile(WriteFile {
        path: "manifest.json".into(), mode: "0644".into(), len: manifest.len() as u64,
    })).await?;
    write_payload(&mut stream, manifest).await?;
    let wf_ack = read_frame(&mut stream).await?;
    if !is_ok(&wf_ack) { anyhow::bail!("write_file failed: {wf_ack:?}"); }
    println!("[harness] WriteFile OK");

    // LAUNCH
    #[cfg(windows)]
    let (shell, command) = ("cmd", "echo qwan-launched-on-guest");
    #[cfg(not(windows))]
    let (shell, command) = ("sh", "echo qwan-launched-on-guest");
    write_frame(&mut stream, &Frame::Launch(Launch {
        command: command.into(), shell: shell.into(),
        cwd: work_dir.to_string_lossy().to_string(),
        env: Default::default(),
    })).await?;
    let launch_ack = read_frame(&mut stream).await?;
    if !is_ok(&launch_ack) { anyhow::bail!("launch failed: {launch_ack:?}"); }
    println!("[harness] LAUNCH OK, collecting STREAM + Exit...");

    // Collect STREAM + Exit
    let mut stdout = String::new();
    let mut exit_code: Option<i32> = None;
    for _ in 0..32 {
        match read_frame(&mut stream).await {
            Ok(Frame::Stream { fd: 1, bytes }) => stdout.push_str(&String::from_utf8_lossy(&bytes)),
            Ok(Frame::Stream { fd: 2, bytes }) => eprintln!("[harness] stderr: {}", String::from_utf8_lossy(&bytes)),
            Ok(Frame::Exit { code }) => { exit_code = Some(code); break; }
            Ok(other) => println!("[harness] frame: {other:?}"),
            Err(e) => { eprintln!("[harness] stream ended: {e}"); break; }
        }
    }

    println!("[harness] === RESULT ===");
    println!("[harness] stdout: {stdout:?}");
    println!("[harness] exit_code: {exit_code:?}");

    let _ = stream.shutdown().await;
    Ok(())
}
