//! Integration test: orchestrator → driver → stub.
//!
//! The LocalOrchestrator creates + starts a VM via MockHyperVDriver. Then we
//! manually open the hvsocket (the seam the orchestrator's bootstrap step will
//! use once wired) and run the HELLO/AUTH handshake against the in-guest stub,
//! proving the orchestrator's VM lifecycle composes with the stub transport.
//!
//! This is the closest we get to the real 7.1→7.2 sequence without a VM: the
//! orchestrator admits + provisions + boots, then the bootstrap handshake runs
//! over the driver's hvsocket to the stub's serve().

use qwanban_core::job::{JobKind, JobSpec};
use qwanban_core::orchestrator::OrchestratorConfig;
use qwanban_core::Orchestrator;
use qwanban_core::LocalOrchestrator;
use qwanban_hyperv::{HyperVDriver, MockHyperVDriver, VmState};
use qwanban_stub::protocol::*;
use qwanban_stub::{serve, ServeConfig};
use std::sync::Arc;
use std::time::Duration;

fn cfg() -> OrchestratorConfig {
    OrchestratorConfig {
        broker_endpoint: "https://10.0.75.1:7443".into(),
        broker_cert_spki: "abc".into(),
        lmstudio_url: "http://10.0.75.1:1234/v1".into(),
        proxy_url: "http://10.0.75.1:8080".into(),
        proxy_ca_fpr: "def".into(),
        allowed_models: vec!["qwen2.5-coder-32b".into()],
        vswitch: "qwan-internal".into(),
        base_vhd_path: "/tmp/base.vhdx".into(),
        max_runtime_s: 3600,
    }
}

fn spec() -> JobSpec {
    JobSpec {
        kind: JobKind::ScriptedQa,
        base_image: "linux-test".into(),
        git_ref: "main".into(),
        task_text: "run qa".into(),
        note: None,
        caps: None,
    }
}

#[tokio::test]
async fn orchestrator_provisions_vm_then_bootstrap_handshake_works() {
    let work_dir = tempfile::tempdir().unwrap();
    // We need the driver both in the orchestrator (as Arc<dyn HyperVDriver>)
    // and to call take_stub_stream. Wrap it in Arc and clone before erasing.
    let driver = Arc::new(MockHyperVDriver::new());
    let driver_for_orch: Arc<dyn qwanban_hyperv::HyperVDriver> = driver.clone();
    let orch = LocalOrchestrator::new(driver_for_orch, 2, cfg());

    // 1. Submit the job — the orchestrator admits, creates + starts the VM,
    //    and drives the state machine to Running.
    let handle = orch.submit(spec()).await.unwrap();
    eprintln!("[orch_stub] admitted case {} (job {})", handle.case_id, handle.job_id);

    // 2. The orchestrator created a VM internally; we don't have its VmHandle,
    //    so for this integration test we create a *second* VM via the same
    //    driver to exercise the hvsocket→stub seam. (The real orchestrator
    //    will open hvsocket on the VM it created; here we prove the seam works
    //    with the same driver instance the orchestrator uses.)
    use qwanban_hyperv::VmSpec;
    use qwanban_proto::config::ResourceCaps;
    use qwanban_proto::id::CaseId;
    let vm = driver
        .create_case_vm(VmSpec {
            case_id: handle.case_id.clone(),
            base_vhd_path: "/tmp/base.vhdx".into(),
            caps: ResourceCaps::default(),
            vswitch: "qwan-internal".into(),
        })
        .await
        .unwrap();
    driver.start_vm(&vm).await.unwrap();
    driver.await_state(&vm, VmState::Running, Duration::from_secs(5)).await.unwrap();

    // 3. Open hvsocket + run the stub handshake.
    let host_stream = driver.open_hvsocket(&vm, 9999).await.unwrap();
    let stub_stream = driver.take_stub_stream(&vm.vm_id).unwrap();
    let serve_cfg = ServeConfig {
        work_dir: work_dir.path().to_path_buf(),
        case_bootstrap_secret: "bootstrap-secret".into(),
        expected_stub_version: 1,
    };
    let serve_task = tokio::spawn(async move { serve(stub_stream, &serve_cfg).await });

    let (mut host_r, mut host_w) = tokio::io::split(host_stream);
    write_frame(&mut host_w, &Frame::Hello(Hello {
        stub_version: 1, os: qwanban_proto::broker::GuestOs::Linux, arch: "x86_64".into(),
    })).await.unwrap();
    write_frame(&mut host_w, &Frame::Auth { case_bootstrap_secret: "bootstrap-secret".into() }).await.unwrap();
    let ack = read_frame(&mut host_r).await.unwrap();
    assert!(is_ok(&ack), "bootstrap handshake over orchestrator's driver should succeed: {ack:?}");
    eprintln!("[orch_stub] bootstrap handshake OK over the orchestrator's driver");

    // 4. Complete the job through the orchestrator.
    let outcome = orch.await_completion(&handle).await.unwrap();
    assert_eq!(outcome.result, qwanban_proto::broker::CaseOutcome::Pass);
    eprintln!("[orch_stub] job outcome: {:?}", outcome.result);

    // 5. Shut down the stub: close the write half so serve() sees EOF and exits.
    use tokio::io::AsyncWriteExt;
    let _ = host_w.shutdown().await;
    drop(host_r);
    let serve_outcome = serve_task.await.unwrap().unwrap();
    assert_eq!(serve_outcome, qwanban_stub::ServeOutcome::HostClosed);
}
