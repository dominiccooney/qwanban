//! `MockHyperVDriver` — an in-memory `HyperVDriver` for dev-VM testing. Tracks
//! VMs by id with their state; `open_hvsocket` returns a tokio duplex so the
//! stub codec can be exercised against it. No real Hyper-V involved.

use crate::{HyperVDriver, HvStream, VmHandle, VmSpec, VmState};
use async_trait::async_trait;
use parking_lot::Mutex;
use qwanban_proto::id::{CheckpointId, VmId};
use qwanban_proto::QwanResult;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

struct MockVm {
    case_id: qwanban_proto::id::CaseId,
    state: VmState,
    /// The host side of a duplex pair; the guest side is handed out on open_hvsocket.
    host_side: Option<tokio::io::DuplexStream>,
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
        let (host, _guest) = tokio::io::duplex(8192);
        let handle = VmHandle { vm_id: vm_id.clone(), case_id: spec.case_id.clone() };
        self.vms.lock().insert(
            vm_id,
            MockVm {
                case_id: spec.case_id,
                state: VmState::Off,
                host_side: Some(host),
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

    async fn open_hvsocket(&self, vm: &VmHandle, _port: u32) -> QwanResult<Box<dyn HvStream>> {
        let mut g = self.vms.lock();
        let entry = g.get_mut(&vm.vm_id).ok_or_else(|| qwanban_proto::not_found("vm not found"))?;
        // Create a fresh duplex and store the host side on the VM entry (keeps it
        // alive so the returned guest half doesn't get a broken pipe).
        let (host, guest) = tokio::io::duplex(8192);
        entry.host_side = Some(host);
        Ok(Box::new(guest))
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
    async fn open_hvsocket_returns_duplex_stream() {
        let d = MockHyperVDriver::new();
        let vm = d.create_case_vm(spec()).await.unwrap();
        let mut stream = d.open_hvsocket(&vm, 9999).await.unwrap();
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
