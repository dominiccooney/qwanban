//! `qwanban-core` — orchestrator + job scheduler + case state machine (§agent-lifecycle).
//!
//! Owns `JobSpec`/`JobOutcome`, the case state machine, admission (hard cap, no
//! queue — §5.8), and manifest building. Host-touching bits (Hyper-V driver) are
//! behind a trait so this is fully unit-testable in the dev VM.
//!
//! **Status: scaffold.** Implements the state machine + scheduler (pure logic);
//! the Hyper-V/broker wiring is stubbed behind traits for teammates to fill.

pub mod state;
pub mod scheduler;
pub mod manifest_builder;
pub mod job;
pub mod orchestrator;

pub use job::{JobSpec, JobOutcome, JobHandle};
pub use state::{CaseState, CaseEvent, transition};
pub use orchestrator::LocalOrchestrator;

use async_trait::async_trait;
use qwanban_proto::QwanResult;

/// The host-side orchestrator. Implementations wire in a real Hyper-V driver +
/// broker; tests use a mock.
#[async_trait]
pub trait Orchestrator: Send + Sync {
    /// Submit a job. Returns `ResourceExhausted` if no free slot (§5.8, no queue).
    async fn submit(&self, spec: JobSpec) -> QwanResult<JobHandle>;
    /// Await a submitted job's outcome.
    async fn await_completion(&self, handle: &JobHandle) -> QwanResult<JobOutcome>;
}

pub mod prelude {
    pub use crate::{job::*, state::*, scheduler::*, Orchestrator, LocalOrchestrator};
    pub use qwanban_proto as proto;
}
