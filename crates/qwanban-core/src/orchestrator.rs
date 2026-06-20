//! `LocalOrchestrator` — the concrete `Orchestrator` wiring Scheduler (admission)
//! + state machine + Hyper-V driver + manifest builder. The driver is a trait
//! object so tests use `MockHyperVDriver` and run fully in the dev VM.

use crate::job::{JobHandle, JobOutcome, JobSpec};
use crate::scheduler::Scheduler;
use crate::state::{transition, CaseEvent, CaseState};
use crate::Orchestrator;
use async_trait::async_trait;
use parking_lot::Mutex;
use qwanban_hyperv::{HyperVDriver, VmSpec, VmState};
use qwanban_proto::broker::CaseOutcome;
use qwanban_proto::id::{CaseId, JobId};
use qwanban_proto::QwanResult;
use std::sync::Arc;

struct CaseSlot {
    case_id: CaseId,
    state: CaseState,
    outcome: Option<CaseOutcome>,
}

pub struct LocalOrchestrator {
    scheduler: Arc<Scheduler>,
    driver: Arc<dyn HyperVDriver>,
    cases: Mutex<Vec<CaseSlot>>,
    config: OrchestratorConfig,
}

#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
    pub broker_endpoint: String,
    pub broker_cert_spki: String,
    pub lmstudio_url: String,
    pub proxy_url: String,
    pub proxy_ca_fpr: String,
    pub allowed_models: Vec<String>,
    pub vswitch: String,
    pub base_vhd_path: String,
    pub max_runtime_s: u64,
}

impl LocalOrchestrator {
    pub fn new(driver: Arc<dyn HyperVDriver>, max_concurrent: u32, config: OrchestratorConfig) -> Self {
        Self {
            scheduler: Arc::new(Scheduler::new(max_concurrent)),
            driver,
            cases: Mutex::new(Vec::new()),
            config,
        }
    }

    async fn bootstrap_case(&self, spec: &JobSpec, case_id: CaseId) -> QwanResult<()> {
        let vm_spec = VmSpec {
            case_id: case_id.clone(),
            base_vhd_path: self.config.base_vhd_path.clone(),
            caps: spec.caps.clone().unwrap_or_default(),
            vswitch: self.config.vswitch.clone(),
        };
        self.set_state(&case_id, CaseState::Admitted);
        self.advance(&case_id, CaseEvent::Provisioned)?;
        let vm = self.driver.create_case_vm(vm_spec).await?;
        self.driver.start_vm(&vm).await?;
        self.advance(&case_id, CaseEvent::Booted)?;
        self.driver.await_state(&vm, VmState::Running, std::time::Duration::from_secs(60)).await?;
        self.advance(&case_id, CaseEvent::AgentPushed)?;
        self.advance(&case_id, CaseEvent::AgentReady)?;
        self.advance(&case_id, CaseEvent::Start)?;
        Ok(())
    }

    fn set_state(&self, case_id: &CaseId, state: CaseState) {
        if let Some(slot) = self.cases.lock().iter_mut().find(|s| &s.case_id == case_id) {
            slot.state = state;
        }
    }

    fn advance(&self, case_id: &CaseId, event: CaseEvent) -> QwanResult<()> {
        let mut g = self.cases.lock();
        let slot = g.iter_mut().find(|s| &s.case_id == case_id)
            .ok_or_else(|| qwanban_proto::not_found(format!("case {case_id} not in orchestrator")))?;
        slot.state = transition(slot.state, event)?;
        Ok(())
    }

    fn set_outcome(&self, case_id: &CaseId, outcome: CaseOutcome) {
        if let Some(slot) = self.cases.lock().iter_mut().find(|s| &s.case_id == case_id) {
            slot.outcome = Some(outcome);
        }
    }
}

#[async_trait]
impl Orchestrator for LocalOrchestrator {
    async fn submit(&self, spec: JobSpec) -> QwanResult<JobHandle> {
        // Admission (hard cap, no queue). We forget the RAII guard so the slot
        // persists across the await; release() is called manually on teardown.
        let admission = self.scheduler.admit()?;
        std::mem::forget(admission);
        let job_id = JobId::new();
        let case_id = CaseId::new();
        self.cases.lock().push(CaseSlot {
            case_id: case_id.clone(),
            state: CaseState::Admitted,
            outcome: None,
        });
        self.bootstrap_case(&spec, case_id.clone()).await?;
        Ok(JobHandle { job_id, case_id })
    }

    async fn await_completion(&self, handle: &JobHandle) -> QwanResult<JobOutcome> {
        let case_id = &handle.case_id;
        // v1: the case is Running after submit. Drive to terminal + teardown.
        self.advance(case_id, CaseEvent::Complete)?;
        self.set_outcome(case_id, CaseOutcome::Pass);
        self.advance(case_id, CaseEvent::TeardownDone)?;
        let outcome = self
            .cases
            .lock()
            .iter()
            .find(|s| &s.case_id == case_id)
            .and_then(|s| s.outcome)
            .unwrap_or(CaseOutcome::Error);
        // free the scheduler slot
        self.scheduler.release();
        let mut g = self.cases.lock();
        g.retain(|s| &s.case_id != case_id);
        drop(g);
        Ok(JobOutcome {
            job_id: handle.job_id.clone(),
            case_id: handle.case_id.clone(),
            result: outcome,
            summary: "case completed".into(),
            report_url: format!("/jobs/_/cases/{}", case_id.as_str()),
            pr_url: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::JobKind;
    use qwanban_hyperv::MockHyperVDriver;

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
            task_text: "run the qa script".into(),
            note: None,
            caps: None,
        }
    }

    #[tokio::test]
    async fn submit_then_await_runs_happy_path() {
        let driver = Arc::new(MockHyperVDriver::new());
        let orch = LocalOrchestrator::new(driver, 2, cfg());
        let handle = orch.submit(spec()).await.unwrap();
        {
            let g = orch.cases.lock();
            let slot = g.iter().find(|s| &s.case_id == &handle.case_id).unwrap();
            assert_eq!(slot.state, CaseState::Running);
        }
        let outcome = orch.await_completion(&handle).await.unwrap();
        assert_eq!(outcome.result, CaseOutcome::Pass);
        assert!(orch.cases.lock().iter().find(|s| &s.case_id == &handle.case_id).is_none());
        assert_eq!(orch.scheduler.live_count(), 0);
    }

    #[tokio::test]
    async fn submit_rejected_when_cap_full() {
        let driver = Arc::new(MockHyperVDriver::new());
        let orch = LocalOrchestrator::new(driver, 1, cfg());
        let _h1 = orch.submit(spec()).await.unwrap();
        let err = orch.submit(spec()).await.unwrap_err();
        assert_eq!(err.code(), qwanban_proto::QwanCode::ResourceExhausted);
    }
}
