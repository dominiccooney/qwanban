//! `JobSpec` / `JobOutcome` / `JobHandle` (§11).

use qwanban_proto::broker::CaseOutcome;
use qwanban_proto::id::{CaseId, JobId};
use serde::{Deserialize, Serialize};

/// A submitted job (CLI / Rust API surface).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSpec {
    pub kind: JobKind,
    /// Image registry name (resolved to a file path on the host).
    pub base_image: String,
    pub git_ref: String,
    /// For ScriptedQa: the markdown QA script. For BugFix: the bug report text.
    pub task_text: String,
    pub note: Option<String>,
    pub caps: Option<qwanban_proto::config::ResourceCaps>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    ScriptedQa,
    BugFix,
}

/// A handle returned by `submit`.
#[derive(Debug, Clone)]
pub struct JobHandle {
    pub job_id: JobId,
    pub case_id: CaseId,
}

/// The outcome of a finished job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobOutcome {
    pub job_id: JobId,
    pub case_id: CaseId,
    pub result: CaseOutcome,
    pub summary: String,
    pub report_url: String,
    pub pr_url: Option<String>,
}
