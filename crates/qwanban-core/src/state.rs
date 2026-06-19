//! Case state machine (design §12). Admission is synchronous accept/reject
//! (hard cap, no queue). States flow:
//!
//! ```text
//! submit → Rejected(ResourceExhausted)   # if no free slot
//! submit → Admitted → Provisioning → Booting → QwanAgentPushed →
//!          ClineAgentReady → Running
//!   Running → (Completed | Failed | Error) → Teardown → Archived
//!   Running → InterventionRequested → Held
//!   Running → OsMigration → Provisioning(new case) → Running
//!   Held → (Resumed → Running | Discarded → Teardown)
//! ```

use qwanban_proto::QwanCode;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseState {
    Admitted,
    Provisioning,
    Booting,
    QwanAgentPushed,
    ClineAgentReady,
    Running,
    InterventionRequested,
    Held,
    OsMigration,
    Completed,
    Failed,
    Error,
    Teardown,
    Archived,
}

impl fmt::Display for CaseState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl CaseState {
    /// Is this a terminal-ish state (no further guest work)?
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            CaseState::Completed | CaseState::Failed | CaseState::Error | CaseState::Archived
        )
    }
    /// Does the case hold a live VM (so auto-teardown must be suppressed)?
    pub fn holds_vm(self) -> bool {
        matches!(self, CaseState::Held | CaseState::InterventionRequested)
    }
}

/// Events that drive the state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseEvent {
    Admit,
    Provisioned,
    Booted,
    AgentPushed,
    AgentReady,
    Start,
    Complete,
    Fail,
    Errored,
    RequestIntervention,
    Resume,
    Discard,
    RequestOsMigration,
    Migrated,
    TeardownDone,
    /// An event that is illegal in the current state.
    Illegal,
}

/// Pure transition function. Returns the new state or an error code.
pub fn transition(from: CaseState, event: CaseEvent) -> Result<CaseState, qwanban_proto::QwanError> {
    use CaseEvent::*;
    use CaseState::*;
    let next = match (from, event) {
        (Admitted, Provisioned) => Provisioning,
        (Provisioning, Booted) => Booting,
        (Booting, AgentPushed) => QwanAgentPushed,
        (QwanAgentPushed, AgentReady) => ClineAgentReady,
        (ClineAgentReady, Start) => Running,
        (Running, Complete) => Completed,
        (Running, Fail) => Failed,
        (Running, Errored) => Error,
        (Running, RequestIntervention) => InterventionRequested,
        (InterventionRequested, _) if event == CaseEvent::Illegal => Held, // placeholder
        (InterventionRequested, Resume) => Held, // resolved-to-held then resume; simplified
        (Held, Resume) => Running,
        (Held, Discard) => Teardown,
        (Running, RequestOsMigration) => OsMigration,
        (OsMigration, Migrated) => Running,
        (Completed | Failed | Error, TeardownDone) => Archived,
        _ => {
            return Err(qwanban_proto::QwanError::new(
                QwanCode::FailedPrecondition,
                format!("illegal transition: {from:?} + {event:?}"),
            ))
        }
    };
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_transitions() {
        let s = CaseState::Admitted;
        let s = transition(s, CaseEvent::Provisioned).unwrap();
        let s = transition(s, CaseEvent::Booted).unwrap();
        let s = transition(s, CaseEvent::AgentPushed).unwrap();
        let s = transition(s, CaseEvent::AgentReady).unwrap();
        let s = transition(s, CaseEvent::Start).unwrap();
        assert_eq!(s, CaseState::Running);
        let s = transition(s, CaseEvent::Complete).unwrap();
        assert_eq!(s, CaseState::Completed);
        let s = transition(s, CaseEvent::TeardownDone).unwrap();
        assert_eq!(s, CaseState::Archived);
    }

    #[test]
    fn illegal_transition_is_error() {
        let err = transition(CaseState::Admitted, CaseEvent::Start).unwrap_err();
        assert_eq!(err.code(), QwanCode::FailedPrecondition);
    }

    #[test]
    fn intervention_holds_vm() {
        let s = transition(CaseState::Running, CaseEvent::RequestIntervention).unwrap();
        assert_eq!(s, CaseState::InterventionRequested);
        assert!(s.holds_vm());
    }
}
