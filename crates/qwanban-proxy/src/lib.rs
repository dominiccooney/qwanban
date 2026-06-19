//! `qwanban-proxy` — MITM HTTPS proxy (§mitm-proxy). Pins to an allowlist; swaps
//! any known dummy string for its real secret via the vault's global
//! search→replace (no header-format logic, no auto-injection); audits every
//! request. Hot-reloaded (Q6). Built on `hudsucker` + `rcgen` (real impl).

pub mod allowlist;
pub mod audit;

pub use allowlist::{Allowlist, HostRule, HostMatch};
pub use audit::{AuditRecord, AuditSink};

use async_trait::async_trait;

/// The proxy's vault access (re-exported shape from qwanban-vault).
#[async_trait]
pub trait ProxyVault: Send + Sync {
    fn snapshot(&self) -> std::sync::Arc<qwanban_vault::SecretSnapshot>;
}
