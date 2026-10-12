use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub operator_addr: SocketAddr,
    pub worker_addr: SocketAddr,
    pub data_dir: PathBuf,
    pub ui_dir: PathBuf,
    pub cloud: Cloud,
    pub friendzone_url: String,
    pub worker_profiles: BTreeMap<String, WorkerProfile>,
    pub limits: Limits,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Cloud {
    pub endpoint: String,
    pub project: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WorkerProfile {
    pub source_image: String,
    pub zone: String,
    pub machine_type: String,
    pub subnetwork: String,
    pub disk_gb: u32,
    pub observatory_port: u16,
    #[serde(default)]
    pub network_tags: Vec<String>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub max_jobs: usize,
    pub max_finished_jobs: usize,
    pub max_artifact_bytes: u64,
    pub artifact_budget_bytes: u64,
    pub upload_timeout_secs: u64,
    pub request_timeout_secs: u64,
    pub job_timeout_secs: u64,
    pub reconcile_secs: u64,
    pub max_relays: usize,
    pub max_relay_message_bytes: usize,
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        && name
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
}

pub fn http_url(value: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(value)?;
    ensure!(
        matches!(url.scheme(), "http" | "https"),
        "expected HTTP(S) URL"
    );
    ensure!(
        url.host_str().is_some() && url.username().is_empty() && url.password().is_none(),
        "URL must have a host and no credentials"
    );
    ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "URL must not have a query or fragment"
    );
    Ok(url)
}

impl Config {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        ensure!(bytes.len() <= 1024 * 1024, "configuration exceeds 1 MiB");
        let mut config: Self = serde_yaml::from_slice(&bytes)?;
        let parent = path.parent().context("configuration has no parent")?;
        for dir in [&mut config.data_dir, &mut config.ui_dir] {
            if dir.is_relative() {
                *dir = parent.join(&*dir);
            }
        }
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.operator_addr.ip().is_loopback(),
            "operator_addr must be loopback"
        );
        ensure!(
            self.operator_addr.port() != 0 && self.worker_addr.port() != 0,
            "listener ports must be nonzero"
        );
        ensure!(
            self.operator_addr.port() != self.worker_addr.port(),
            "operator and worker ports must differ"
        );
        ensure!(
            !self.worker_addr.ip().is_unspecified(),
            "worker_addr must name a specific interface, not a wildcard"
        );
        let friendzone = http_url(&self.friendzone_url)?;
        ensure!(
            friendzone.path() == "/",
            "friendzone_url must be an origin without a path"
        );
        ensure!(
            friendzone
                .host_str()
                .and_then(|h| h.parse::<std::net::IpAddr>().ok())
                .is_some_and(|ip| ip.is_loopback()),
            "friendzone_url must use a literal loopback address"
        );
        let endpoint = http_url(&self.cloud.endpoint)?;
        ensure!(
            endpoint.path() == "/",
            "cloud.endpoint must be an origin; the SDK supplies /compute/v1"
        );
        ensure!(
            valid_name(&self.cloud.project)
                || self.cloud.project.bytes().all(|c| c.is_ascii_digit())
                    && !self.cloud.project.is_empty(),
            "invalid project"
        );
        ensure!(
            !self.worker_profiles.is_empty(),
            "at least one worker profile is required"
        );
        for (name, profile) in &self.worker_profiles {
            ensure!(
                valid_name(name) && valid_name(&profile.zone) && valid_name(&profile.machine_type),
                "invalid worker profile, zone or machine type: {name}"
            );
            ensure!(
                profile.disk_gb > 0 && profile.observatory_port > 0,
                "disk size and observatory port must be nonzero"
            );
            ensure!(
                profile.source_image.starts_with("projects/")
                    && profile.subnetwork.starts_with("projects/"),
                "use project-qualified image and subnetwork paths"
            );
            ensure!(
                profile.network_tags.iter().all(|tag| valid_name(tag)),
                "invalid network tag"
            );
            for (key, value) in &profile.metadata {
                ensure!(
                    !key.starts_with("control-tower-"),
                    "control-tower-* metadata is reserved"
                );
                validate_metadata(key, value)?;
            }
        }
        let l = &self.limits;
        ensure!(
            l.max_jobs > 0 && l.max_finished_jobs > 0 && l.max_relays > 0,
            "job, history and relay limits must be nonzero"
        );
        ensure!(
            l.max_artifact_bytes >= 4 && l.max_artifact_bytes <= l.artifact_budget_bytes,
            "artifact limit must fit the storage budget"
        );
        ensure!(
            [
                l.upload_timeout_secs,
                l.request_timeout_secs,
                l.job_timeout_secs,
                l.reconcile_secs
            ]
            .iter()
            .all(|n| *n > 0),
            "timeouts must be nonzero"
        );
        ensure!(
            l.max_relay_message_bytes > 0,
            "relay message limit must be nonzero"
        );
        Ok(())
    }

    pub fn restart_fields_match(&self, next: &Self) -> bool {
        self.operator_addr == next.operator_addr
            && self.worker_addr == next.worker_addr
            && self.data_dir == next.data_dir
            && self.ui_dir == next.ui_dir
    }

    pub fn worker_url(&self) -> String {
        format!("http://{}", self.worker_addr)
    }
}

pub fn validate_metadata(key: &str, value: &str) -> Result<()> {
    ensure!(
        !key.is_empty()
            && key.len() < 128
            && key
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
        "invalid metadata key"
    );
    ensure!(
        value.len() <= 262_144,
        "metadata value exceeds GCE's 256 KiB limit"
    );
    Ok(())
}
