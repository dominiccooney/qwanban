//! The computer-use executor `cuxec` (§input-injection). Maps Anthropic
//! `computer_20250124` actions → `InputEvent`s, scales coords API↔screen,
//! injects via the per-OS `ComputerBackend`. The agent-loop adapter calls this.

use async_trait::async_trait;
use qwanban_proto::input::{ComputerAction, InputAck, InputEvent, ToolResult};
use qwanban_proto::QwanResult;

/// Per-OS injection backend (SendInput on Windows; uinput/XTEST on Linux).
/// Host-only; mocked in tests.
#[async_trait]
pub trait ComputerBackend: Send + Sync {
    async fn inject(&self, ev: InputEvent) -> QwanResult<InputAck>;
    async fn capture_screenshot(&self) -> QwanResult<Vec<u8>>; // png/jpeg bytes
    fn screen_geometry(&self) -> (u32, u32);
}

/// Executes one Anthropic computer-use action end-to-end.
#[async_trait]
pub trait ComputerUseExecutor: Send + Sync {
    async fn execute(&self, action: ComputerAction) -> QwanResult<ToolResult>;
    fn advertised_resolution(&self) -> qwanban_proto::input::Resolution;
}
