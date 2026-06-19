//! The per-case manifest (§agent-lifecycle). Written by `qwanban-core`, pushed
//! into the guest by `qwan-stub` over hvsocket. Contains only **dummy** keys +
//! the case_token (worthless off-host); real keys never appear (§S7).

use crate::id::{CaseId, JobId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: String,
    pub job_id: JobId,
    pub case_id: CaseId,
    pub kind: JobKind,
    pub task: TaskPayload,
    pub repo: RepoSpec,
    pub broker: BrokerEndpoint,
    pub auth: AuthSpec,
    pub inference: InferenceSpec,
    pub proxy: ProxySpec,
    pub agent: AgentSpec,
    pub capture: CaptureSpec,
    pub limits: LimitsSpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    ScriptedQa,
    BugFix,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPayload {
    /// Human-readable QA script (markdown) for ScriptedQa; bug report for BugFix.
    pub script_text: Option<String>,
    pub report_text: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoSpec {
    pub url: String,
    pub ref_: String,
    pub checkout_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokerEndpoint {
    pub endpoint: String,
    pub cert_spki_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthSpec {
    pub case_token_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceSpec {
    pub base_url: String,
    pub dummy_key: String,
    pub allowed_models: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxySpec {
    pub https_proxy: String,
    pub ca_fpr_sha256: String,
}

/// The agent = "files + a command" (Q2 decided). qwanban is form-factor agnostic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSpec {
    pub files: Vec<AgentFile>,
    pub launch: AgentLaunch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentFile {
    pub src: String,
    pub dest: String,
    #[serde(default = "default_mode")]
    pub mode: String,
}
fn default_mode() -> String {
    "0755".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentLaunch {
    pub shell: String,
    pub command: String,
    pub cwd: String,
    pub env: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureSpec {
    #[serde(default = "default_fps")]
    pub fps: u32,
    #[serde(default = "default_segment_seconds")]
    pub segment_seconds: u32,
    #[serde(default = "default_encode_where")]
    pub encode_where: EncodeWhere,
}
fn default_fps() -> u32 {
    5
}
fn default_segment_seconds() -> u32 {
    4
}
fn default_encode_where() -> EncodeWhere {
    EncodeWhere::Guest
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EncodeWhere {
    Guest,
    Host,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitsSpec {
    pub max_runtime_s: u64,
}
