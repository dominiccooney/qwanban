//! Integration test: bootstrap over TCP stream.
//!
//! Proves the stub `serve()` codec works over the *real* transport seam the
//! orchestrator uses (MockHyperVDriver::open_stream), not just a bare duplex.

use qwanban_hyperv::{HyperVDriver, MockHyperVDriver, VmSpec, VmState};
use qwanban_proto::config::ResourceCaps;
use qwanban_proto::id::CaseId;
use qwanban_stub::protocol::*;
use qwanban_stub::{serve, ServeConfig};
use sha2::{Digest, Sha256};
use std::time::Duration;

fn hex_sha256(b: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b);
    let out = h.finalize();
    out.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn spec() -> VmSpec {
    VmSpec {
        case_id: CaseId::from_str_inner("case_boot"),
        base_vhd_path: "/tmp/base.vhdx".into(),
        caps: ResourceCaps::default(),
        vswitch: "qwan-internal".into(),
    }
}

#[tokio::test]
async fn stub_serve_over_stream_full_bootstrap() {
    let work_dir = tempfile::tempdir().unwrap();
    let driver = MockHyperVDriver::new();
    let vm = driver.create_case_vm(spec()).await.unwrap();
    driver.start_vm(&vm).await.unwrap();
    driver.await_state(&vm, VmState::Running, Duration::from_secs(5)).await.unwrap();

    let host_stream = driver.open_stream(&vm, 9999).await.unwrap();
    let stub_stream = driver.take_stub_stream(&vm.vm_id).expect("stub stream after open_stream");

    let cfg = ServeConfig {
        work_dir: work_dir.path().to_path_buf(),
        case_bootstrap_secret: "bootstrap-secret".into(),
        expected_stub_version: 1,
    };
    let serve_task = tokio::spawn(async move { serve(stub_stream, &cfg).await });

    let (mut host_r, mut host_w) = tokio::io::split(host_stream);

    // HELLO + AUTH
    write_frame(&mut host_w, &Frame::Hello(Hello {
        stub_version: 1, os: qwanban_proto::broker::GuestOs::Linux, arch: "x86_64".into(),
    })).await.unwrap();
    write_frame(&mut host_w, &Frame::Auth { case_bootstrap_secret: "bootstrap-secret".into() }).await.unwrap();
    let auth_ack = read_frame(&mut host_r).await.unwrap();
    assert!(is_ok(&auth_ack), "auth should succeed: {auth_ack:?}");

    // PUSH_AGENT
    let agent_bytes = b"#!/bin/sh\necho qwan-guest up\n";
    let hash = hex_sha256(agent_bytes);
    write_frame(&mut host_w, &Frame::PushAgent(PushAgent { sha256: hash, len: agent_bytes.len() as u64 })).await.unwrap();
    write_payload(&mut host_w, agent_bytes).await.unwrap();
    let push_ack = read_frame(&mut host_r).await.unwrap();
    assert!(is_ok(&push_ack), "push_agent ack: {push_ack:?}");

    // WriteFile
    let manifest_bytes = br#"{"schema":"qwan.manifest/v1"}"#;
    write_frame(&mut host_w, &Frame::WriteFile(WriteFile {
        path: "manifest.json".into(), mode: "0644".into(), len: manifest_bytes.len() as u64,
    })).await.unwrap();
    write_payload(&mut host_w, manifest_bytes).await.unwrap();
    let wf_ack = read_frame(&mut host_r).await.unwrap();
    assert!(is_ok(&wf_ack), "write_file ack: {wf_ack:?}");

    // LAUNCH
    #[cfg(windows)]
    let (shell, command) = ("cmd", "echo qwan-launched");
    #[cfg(not(windows))]
    let (shell, command) = ("sh", "echo qwan-launched");
    write_frame(&mut host_w, &Frame::Launch(Launch {
        command: command.into(), shell: shell.into(), cwd: "".into(), env: Default::default(),
    })).await.unwrap();
    let launch_ack = read_frame(&mut host_r).await.unwrap();
    assert!(is_ok(&launch_ack), "launch ack: {launch_ack:?}");

    // Collect STREAM + Exit
    let mut stdout = String::new();
    let mut exit_code: Option<i32> = None;
    for _ in 0..16 {
        match read_frame(&mut host_r).await {
            Ok(Frame::Stream { fd: 1, bytes }) => stdout.push_str(&String::from_utf8_lossy(&bytes)),
            Ok(Frame::Exit { code }) => { exit_code = Some(code); break; }
            Ok(other) => eprintln!("[bootstrap] unexpected: {other:?}"),
            Err(e) => { eprintln!("[bootstrap] stream ended: {e}"); break; }
        }
    }
    assert!(stdout.contains("qwan-launched"), "stdout: {stdout:?}");
    assert_eq!(exit_code, Some(0));

    let outcome = serve_task.await.unwrap().unwrap();
    assert_eq!(outcome, qwanban_stub::ServeOutcome::ChildExited(0));

    // Debugging: real files on disk
    let agent_path = work_dir.path().join("qwan-guest");
    let manifest_path = work_dir.path().join("manifest.json");
    assert!(agent_path.exists());
    assert!(manifest_path.exists());
    assert_eq!(std::fs::read(&agent_path).unwrap(), agent_bytes);
    eprintln!("[bootstrap] agent at: {}", agent_path.display());
    eprintln!("[bootstrap] manifest at: {}", manifest_path.display());
    eprintln!("[bootstrap] stdout: {stdout:?}");
}

#[tokio::test]
async fn stub_rejects_bad_secret_over_stream() {
    let work_dir = tempfile::tempdir().unwrap();
    let driver = MockHyperVDriver::new();
    let vm = driver.create_case_vm(spec()).await.unwrap();
    driver.start_vm(&vm).await.unwrap();

    let host_stream = driver.open_stream(&vm, 9999).await.unwrap();
    let stub_stream = driver.take_stub_stream(&vm.vm_id).unwrap();

    let cfg = ServeConfig {
        work_dir: work_dir.path().to_path_buf(),
        case_bootstrap_secret: "real-secret".into(),
        expected_stub_version: 1,
    };
    let serve_task = tokio::spawn(async move { serve(stub_stream, &cfg).await });

    let (mut host_r, mut host_w) = tokio::io::split(host_stream);
    write_frame(&mut host_w, &Frame::Hello(Hello {
        stub_version: 1, os: qwanban_proto::broker::GuestOs::Linux, arch: "x86_64".into(),
    })).await.unwrap();
    write_frame(&mut host_w, &Frame::Auth { case_bootstrap_secret: "WRONG".into() }).await.unwrap();
    let ack = read_frame(&mut host_r).await.unwrap();
    assert!(!is_ok(&ack), "bad secret should produce a negative ack: {ack:?}");

    let outcome = serve_task.await.unwrap().unwrap();
    assert_eq!(outcome, qwanban_stub::ServeOutcome::AuthRejected);
}
