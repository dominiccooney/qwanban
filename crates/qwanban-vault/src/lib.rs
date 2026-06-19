//! `qwanban-vault` — the host secret vault (§mitm-proxy, Q6). Owns
//! `secrets.toml` with two sections:
//! - `[real]` — real secret values by name
//! - `[[rewrite]]` — dummy (search) → real secret name (replace)
//!
//! The rewriter is a **global search→replace** table: dummies are real-looking,
//! unique, secret-shaped strings (so the agent can juggle multiple tokens and
//! hide them in a chroot). No header-format logic, no auto-injection.
//! **Hot-reloaded** via file watch + atomic snapshot swap.

pub mod snapshot;
pub mod rewriter;
pub mod watcher;
pub mod file_vault;

pub use snapshot::{SecretSnapshot, RealSecret, RewriteEntry, SecretsFile};
pub use rewriter::{Rewriter, RewriteResult};
pub use watcher::VaultWatcher;
pub use file_vault::FileVault;

use async_trait::async_trait;
use qwanban_proto::QwanResult;

/// The shared vault trait (consumed by proxy + inference-router).
#[async_trait]
pub trait Vault: Send + Sync {
    /// Current value of a named real secret.
    fn secret(&self, name: &str) -> Option<RealSecret>;
    /// The current rewrite table (dummy → real secret name).
    fn rewrite_table(&self) -> Vec<RewriteEntry>;
    /// Validate that every `replace` name resolves to a `[real]` entry.
    fn validate(&self) -> QwanResult<()>;
    /// Hot-reload the snapshot from disk.
    async fn reload(&self) -> QwanResult<()>;
    /// Cheap clone of the current snapshot (for the rewriter + readers).
    fn snapshot(&self) -> std::sync::Arc<SecretSnapshot>;
}

pub use RealSecret as SecretString;
