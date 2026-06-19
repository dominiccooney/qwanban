//! Host config (§5.1, §5.8): `qwanban.toml` — image registry, resource caps,
//! proxy allowlist, inference routes. `secrets.toml` (the `[real]` + `[[rewrite]]`
//! table) is owned by `qwanban-vault`, not here.

use serde::{Deserialize, Serialize};

/// Top-level host config, deserialized from `qwanban.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostConfig {
    #[serde(default)]
    pub images: Vec<ImageEntry>,
    #[serde(default)]
    pub defaults: ResourceDefaults,
    #[serde(default)]
    pub proxy: ProxyConfig,
    #[serde(default)]
    pub inference: InferenceConfig,
}

/// A registered base image the maintainer points at by file path (§5.6).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageEntry {
    pub name: String,
    pub os: crate::broker::GuestOs,
    pub path: String, // maintainer-supplied VHD/VHDX file path
    #[serde(default)]
    pub caps: ResourceCaps,
}

/// Per-image resource defaults (§5.8).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ResourceDefaults {
    #[serde(default = "default_vcpu")]
    pub vcpu: u32,
    #[serde(default = "default_memory_mb")]
    pub memory_mb: u64,
    #[serde(default = "default_disk_gb")]
    pub disk_gb: u64,
    #[serde(default = "default_max_runtime_s")]
    pub max_runtime_s: u64,
    #[serde(default = "default_max_concurrent_cases")]
    pub max_concurrent_cases: u32,
}
fn default_vcpu() -> u32 {
    4
}
fn default_memory_mb() -> u64 {
    8192
}
fn default_disk_gb() -> u64 {
    64
}
fn default_max_runtime_s() -> u64 {
    2700
}
fn default_max_concurrent_cases() -> u32 {
    3
}

/// Per-case resource caps (override defaults from the job spec).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ResourceCaps {
    pub vcpu: Option<u32>,
    pub memory_mb: Option<u64>,
    pub disk_gb: Option<u64>,
    pub max_runtime_s: Option<u64>,
}

/// Proxy allowlist (hosts the guest may reach). The search→replace rewrite table
/// lives in `secrets.toml` (qwanban-vault), NOT here.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub listen: String,
    #[serde(default)]
    pub hosts: Vec<HostRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostRule {
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub host_suffix: Option<String>,
    #[serde(default)]
    pub allow_methods: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InferenceConfig {
    pub lmstudio_url: String,
    #[serde(default)]
    pub routes: Vec<InferenceRoute>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceRoute {
    pub model: String,
    pub target: RouteTarget,
    /// Cloud provider base URL (only for `Cloud` routes; LM Studio uses
    /// `InferenceConfig::lmstudio_url`). No `secret` field — the proxy's
    /// search→replace table (secrets.toml) maps the case's dummy → real key.
    #[serde(default)]
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RouteTarget {
    Lmstudio,
    Cloud,
}
