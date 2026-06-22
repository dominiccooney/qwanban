//! `MockHyperVDriver` - an in-memory `HyperVDriver` for dev-VM testing. Tracks
//! VMs by id with their state; `open_stream` returns a tokio duplex so the
//! stub codec can be exercised against it. No real Hyper-V involved.

use crate::{GuestStream, HyperVDriver, VmHandle, VmSpec, VmState};
use async_trait::async_trait;
use parking_lot::Mutex;
use qwanban_proto::id::{CheckpointId, VmId};
use qwanban_proto::QwanResult;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

struct MockVm {
    #[allow(dead_code)]
    case_id: qwanban_proto::id::CaseId,
    state: VmState,
    /// The stub's end of the stream duplex, stashed when `open_stream`
    /// creates the pair. A test retrieves it via `take_stub_stream` and runs
    /// `serve()` on it (simulating the in-guest stub). `None` once taken or if
    /// no stream has been opened yet.
    stub_side: Option<tokio::io::DuplexStream>,
}

/// An in-memory Hyper-V driver for tests.
pub struct MockHyperVDriver {
    vms: Mutex<HashMap<VmId, MockVm>>,
    next_id: AtomicU64,
}

impl MockHyperVDriver {
    pub fn new() -> Self {
        Self {
            vms: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
        }
    }

    /// Retrieve the stub's end of the stream for a VM (the end
    /// `serve()` should run on). Returns `None` if `open_stream` hasn't been
    /// called yet, or if it was already taken. This is the seam integration
    /// tests use to drive the in-guest stub in-process.
    pub fn take_stub_stream(&self, vm_id: &VmId) -> Option<tokio::io::DuplexStream> {
        self.vms.lock().get_mut(vm_id).and_then(|v| v.stub_side.take())
    }
}

impl Default for MockHyperVDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HyperVDriver for MockHyperVDriver {
    async fn ensure_switch(&self, _name: &str) -> QwanResult<()> {
        Ok(())
    }

    async fn create_case_vm(&self, spec: VmSpec) -> QwanResult<VmHandle> {
        let n = self.next_id.fetch_add(1, Ordering::Relaxed);
        let vm_id = VmId(format!("mock-vm-{n:04x}"));
        let handle = VmHandle { vm_id: vm_id.clone(), case_id: spec.case_id.clone() };
        self.vms.lock().insert(
            vm_id,
            MockVm {
                case_id: spec.case_id,
                state: VmState::Off,
                stub_side: None,
            },
        );
        Ok(handle)
    }

    async fn start_vm(&self, vm: &VmHandle) -> QwanResult<()> {
        let mut g = self.vms.lock();
        let entry = g.get_mut(&vm.vm_id).ok_or_else(|| qwanban_proto::not_found("vm not found"))?;
        entry.state = VmState::Running;
        Ok(())
    }

    async fn await_state(&self, vm: &VmHandle, state: VmState, _timeout: std::time::Duration) -> QwanResult<()> {
        let g = self.vms.lock();
        let entry = g.get(&vm.vm_id).ok_or_else(|| qwanban_proto::not_found("vm not found"))?;
        if entry.state != state {
            return Err(qwanban_proto::internal(format!(
                "vm {} is {:?}, not {:?}",
                vm.vm_id, entry.state, state
            )));
        }
        Ok(())
    }

    async fn open_stream(&self, vm: &VmHandle, _port: u32) -> QwanResult<Box<dyn GuestStream>> {
        let mut g = self.vms.lock();
        let entry = g.get_mut(&vm.vm_id).ok_or_else(|| qwanban_proto::not_found("vm not found"))?;
        // Create a duplex pair: the host (orchestrator) gets one end, the stub
        // gets the other. We stash the stub's end so a test can retrieve it via
        // `take_stub_stream` and run `serve()` on it.
        let (host, stub) = tokio::io::duplex(8192);
        entry.stub_side = Some(stub);
        Ok(Box::new(host))
    }

    async fn checkpoint(&self, _vm: &VmHandle, name: &str) -> QwanResult<CheckpointId> {
        Ok(CheckpointId(format!("ckpt-{}", name)))
    }

    async fn stop_vm(&self, vm: &VmHandle) -> QwanResult<()> {
        let mut g = self.vms.lock();
        if let Some(e) = g.get_mut(&vm.vm_id) {
            e.state = VmState::Off;
        }
        Ok(())
    }

    async fn destroy_case_vm(&self, vm: &VmHandle) -> QwanResult<()> {
        self.vms.lock().remove(&vm.vm_id);
        Ok(())
    }

    async fn live_vm_count(&self) -> QwanResult<u32> {
        Ok(self.vms.lock().values().filter(|v| v.state != VmState::Off).count() as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qwanban_proto::config::ResourceCaps;
    use qwanban_proto::id::CaseId;

    fn spec() -> VmSpec {
        VmSpec {
            case_id: CaseId::from_str_inner("case_1"),
            base_vhd_path: "/tmp/base.vhdx".into(),
            caps: ResourceCaps::default(),
            vswitch: "qwan-internal".into(),
        }
    }

    #[tokio::test]
    async fn create_start_destroy_lifecycle() {
        let d = MockHyperVDriver::new();
        let vm = d.create_case_vm(spec()).await.unwrap();
        d.start_vm(&vm).await.unwrap();
        d.await_state(&vm, VmState::Running, std::time::Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(d.live_vm_count().await.unwrap(), 1);
        d.stop_vm(&vm).await.unwrap();
        assert_eq!(d.live_vm_count().await.unwrap(), 0);
        d.destroy_case_vm(&vm).await.unwrap();
    }

    #[tokio::test]
    async fn open_stream_returns_duplex_stream() {
        let d = MockHyperVDriver::new();
        let vm = d.create_case_vm(spec()).await.unwrap();
        let mut stream = d.open_stream(&vm, 9999).await.unwrap();
        use tokio::io::AsyncWriteExt;
        stream.write_all(b"hello stub").await.unwrap();
    }

    #[tokio::test]
    async fn destroy_removes_vm() {
        let d = MockHyperVDriver::new();
        let vm = d.create_case_vm(spec()).await.unwrap();
        d.destroy_case_vm(&vm).await.unwrap();
        // now gone — stop returns Ok but does nothing; await_state errors
        let err = d
            .await_state(&vm, VmState::Off, std::time::Duration::from_secs(1))
            .await
            .unwrap_err();
        assert_eq!(err.code(), qwanban_proto::QwanCode::NotFound);
    }
}
