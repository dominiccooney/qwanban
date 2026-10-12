use crate::config::{Cloud, WorkerProfile};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Submission {
    pub id: Uuid,
    pub worker_profile: String,
    pub title: String,
    pub source: String,
    pub prompt: String,
    pub setup_script: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub cloud: Cloud,
    pub worker_profile: WorkerProfile,
    pub friendzone_url: String,
    pub worker_url: String,
    pub request_timeout_secs: u64,
    pub job_timeout_secs: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Completed,
    Cancelled,
    Failed,
    Lost,
    TimedOut,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Phase {
    Queued,
    Provisioning {
        operation: Option<String>,
    },
    Registering,
    Running,
    Cleaning {
        unregister: bool,
        delete: bool,
        operation: Option<String>,
    },
    Finished,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub bytes: u64,
    pub at: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub submission: Submission,
    pub snapshot: Snapshot,
    pub name: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub finished_at: Option<u64>,
    pub instance_id: Option<String>,
    pub ip: Option<std::net::IpAddr>,
    pub phase: Phase,
    pub outcome: Option<Outcome>,
    pub last_error: Option<String>,
    pub artifact: Option<Artifact>,
}

impl Job {
    pub fn active(&self) -> bool {
        !matches!(self.phase, Phase::Finished)
    }
    pub fn running(&self) -> bool {
        matches!(self.phase, Phase::Running) && self.outcome.is_none()
    }
    pub fn metadata(&self) -> BTreeMap<String, String> {
        let mut items = self.snapshot.worker_profile.metadata.clone();
        items.insert(
            "control-tower-job-id".into(),
            self.submission.id.to_string(),
        );
        items.insert(
            "control-tower-worker-url".into(),
            self.snapshot.worker_url.clone(),
        );
        items.insert(
            "control-tower-prompt".into(),
            self.submission.prompt.clone(),
        );
        items.insert(
            "control-tower-setup-script".into(),
            self.submission.setup_script.clone(),
        );
        items
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Ledger {
    pub version: u32,
    pub jobs: BTreeMap<Uuid, Job>,
    pub retired_ids: BTreeSet<Uuid>,
}

impl Default for Ledger {
    fn default() -> Self {
        Self {
            version: 2,
            jobs: BTreeMap::new(),
            retired_ids: BTreeSet::new(),
        }
    }
}
