//! `qwanban-hyperv` — Hyper-V driver (§hyperv-driver). Owns VM lifecycle, disks,
//! networking, hvsocket transport, checkpoints. **Host-only; gated tests.**
//!
//! The `HyperVDriver` trait is the seam: a mock impl lets `qwanban-core` and the
//! broker integration harness run in the dev VM with no Hyper-V.

pub mod mock;

use async_trait::async_trait;
use qwanban_proto::{QwanResult, config::ResourceCaps, id::{CaseId, CheckpointId, VmId}};

/// A handle to a running case VM.
#[derive(Debug, Clone)]
pub struct VmHandle {
    pub vm_id: VmId,
    pub case_id: CaseId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmState {
    Off,
    Booting,
    Running,
    Held,
}

/// Spec for creating a case VM.
#[derive(Debug, Clone)]
pub struct VmSpec {
    pub case_id: CaseId,
    pub base_vhd_path: String,
    pub caps: ResourceCaps,
    pub vswitch: String,
}

/// The Hyper-V driver trait. Mock in tests; real impl shells out to PowerShell/WMI.
#[async_trait]
pub trait HyperVDriver: Send + Sync {
    async fn ensure_switch(&self, name: &str) -> QwanResult<()>;
    async fn create_case_vm(&self, spec: VmSpec) -> QwanResult<VmHandle>;
    async fn start_vm(&self, vm: &VmHandle) -> QwanResult<()>;
    async fn await_state(&self, vm: &VmHandle, state: VmState, timeout: std::time::Duration) -> QwanResult<()>;
    /// Open an hvsocket byte stream to the guest's `qwan-stub` (§stub-loader).
    async fn open_hvsocket(&self, vm: &VmHandle, port: u32) -> QwanResult<Box<dyn HvStream>>;
    async fn checkpoint(&self, vm: &VmHandle, name: &str) -> QwanResult<CheckpointId>;
    async fn stop_vm(&self, vm: &VmHandle) -> QwanResult<()>;
    async fn destroy_case_vm(&self, vm: &VmHandle) -> QwanResult<()>;
    /// Current live VM count (for the scheduler to count free slots).
    async fn live_vm_count(&self) -> QwanResult<u32>;
}

/// A byte-stream over hvsocket to the guest stub.
pub trait HvStream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin {}

impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + ?Sized> HvStream for T {}

pub use mock::MockHyperVDriver;
