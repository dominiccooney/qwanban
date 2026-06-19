//! Hot-reload watcher (Q6). Watches `secrets.toml`; on change, parses →
//! validates → atomically swaps the snapshot behind an `ArcSwap`. In-flight
//! requests keep their old snapshot; new requests see the new one. A malformed
//! reload keeps the previous snapshot + emits a log.

use crate::snapshot::{SecretSnapshot, SecretsFile};
use parking_lot::RwLock;
use std::path::PathBuf;
use std::sync::Arc;

/// Holds the current snapshot, swappable atomically.
pub struct VaultWatcher {
    path: PathBuf,
    snap: Arc<RwLock<Arc<SecretSnapshot>>>,
}

impl VaultWatcher {
    /// Load the initial snapshot from `path`.
    pub fn load(path: impl Into<PathBuf>) -> qwanban_proto::QwanResult<Self> {
        let path = path.into();
        let text = std::fs::read_to_string(&path).map_err(|e| {
            qwanban_proto::QwanError::new(
                qwanban_proto::QwanCode::Internal,
                format!("read {}: {e}", path.display()),
            )
        })?;
        let file = SecretsFile::parse(&text)?;
        let snap = Arc::new(SecretSnapshot::from_file(file)?);
        Ok(Self {
            path,
            snap: Arc::new(RwLock::new(snap)),
        })
    }

    /// Current snapshot (cheap Arc clone).
    pub fn snapshot(&self) -> Arc<SecretSnapshot> {
        self.snap.read().clone()
    }

    /// Re-read + validate + swap. On error, keeps the previous snapshot.
    pub async fn reload(&self) -> qwanban_proto::QwanResult<()> {
        let text = tokio::fs::read_to_string(&self.path).await.map_err(|e| {
            qwanban_proto::QwanError::new(
                qwanban_proto::QwanCode::Internal,
                format!("read {}: {e}", self.path.display()),
            )
        })?;
        let file = SecretsFile::parse(&text)?;
        let new_snap = Arc::new(SecretSnapshot::from_file(file)?);
        let mut guard = self.snap.write();
        *guard = new_snap;
        Ok(())
    }

    /// Path being watched.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reload_picks_up_new_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.toml");
        std::fs::write(&path, "[real]\nk = \"OLD\"\n[[rewrite]]\nsearch=\"d\"\nreplace=\"k\"\n").unwrap();
        let w = VaultWatcher::load(&path).unwrap();
        assert_eq!(w.snapshot().secret("k").unwrap().as_str(), "OLD");
        // rewrite it
        std::fs::write(&path, "[real]\nk = \"NEW\"\n[[rewrite]]\nsearch=\"d\"\nreplace=\"k\"\n").unwrap();
        w.reload().await.unwrap();
        assert_eq!(w.snapshot().secret("k").unwrap().as_str(), "NEW");
    }

    #[tokio::test]
    async fn malformed_reload_keeps_previous() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.toml");
        std::fs::write(&path, "[real]\nk = \"OLD\"\n[[rewrite]]\nsearch=\"d\"\nreplace=\"k\"\n").unwrap();
        let w = VaultWatcher::load(&path).unwrap();
        // write a malformed/unresolved table
        std::fs::write(&path, "[[rewrite]]\nsearch=\"d\"\nreplace=\"missing\"\n").unwrap();
        let err = w.reload().await.unwrap_err();
        assert_eq!(err.code(), qwanban_proto::QwanCode::InvalidArg);
        // previous snapshot retained
        assert_eq!(w.snapshot().secret("k").unwrap().as_str(), "OLD");
    }
}
