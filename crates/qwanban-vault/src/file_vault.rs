//! `FileVault` — the concrete `Vault` impl backed by `VaultWatcher` (a file on
//! the host filesystem). This is the Q6 "convenient plain file" decision.

use crate::snapshot::{RealSecret, RewriteEntry, SecretSnapshot};
use crate::watcher::VaultWatcher;
use crate::Vault;
use async_trait::async_trait;
use qwanban_proto::QwanResult;
use std::path::PathBuf;
use std::sync::Arc;

/// A file-backed vault. Cheap to clone (shares the inner `Arc<VaultWatcher>`).
#[derive(Clone)]
pub struct FileVault {
    inner: Arc<VaultWatcher>,
}

impl FileVault {
    /// Load the initial snapshot from `path`.
    pub fn load(path: impl Into<PathBuf>) -> QwanResult<Self> {
        Ok(Self {
            inner: Arc::new(VaultWatcher::load(path)?),
        })
    }

    /// Spawn a background task that polls the file mtime every `interval` and
    /// hot-reloads on change. Errors during reload are logged but keep the
    /// previous snapshot (per Q6: never a broken partial config).
    pub fn spawn_watcher(&self, interval: std::time::Duration) -> tokio::task::JoinHandle<()>
    where
        Self: 'static,
    {
        let me = self.clone();
        let path = me.inner.path().to_path_buf();
        tokio::spawn(async move {
            let mut last_mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            loop {
                tokio::time::sleep(interval).await;
                let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                if mtime != last_mtime {
                    last_mtime = mtime;
                    match me.inner.reload().await {
                        Ok(()) => tracing::debug!("vault reloaded {}", path.display()),
                        Err(e) => tracing::warn!("vault reload failed (kept previous): {e}"),
                    }
                }
            }
        })
    }
}

#[async_trait]
impl Vault for FileVault {
    fn secret(&self, name: &str) -> Option<RealSecret> {
        self.inner.snapshot().secret(name)
    }

    fn rewrite_table(&self) -> Vec<RewriteEntry> {
        self.inner.snapshot().rewrite.clone()
    }

    fn validate(&self) -> QwanResult<()> {
        // SecretSnapshot::from_file already validated on load; re-check live.
        let snap = self.inner.snapshot();
        for e in &snap.rewrite {
            if !snap.real.contains_key(&e.replace) {
                return Err(qwanban_proto::invalid_arg(format!(
                    "rewrite '{}' -> unknown real secret '{}'",
                    e.search, e.replace
                )));
            }
        }
        Ok(())
    }

    async fn reload(&self) -> QwanResult<()> {
        self.inner.reload().await
    }

    fn snapshot(&self) -> Arc<SecretSnapshot> {
        self.inner.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn file_vault_trait_methods() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.toml");
        std::fs::write(
            &path,
            "[real]\ngh = \"ghp_REAL\"\n\n[[rewrite]]\nsearch=\"ghp_DUMMY\"\nreplace=\"gh\"\n",
        )
        .unwrap();
        let v = FileVault::load(&path).unwrap();
        assert_eq!(v.secret("gh").unwrap().as_str(), "ghp_REAL");
        assert_eq!(v.rewrite_table().len(), 1);
        v.validate().unwrap();
        // unknown secret
        assert!(v.secret("missing").is_none());
    }

    #[tokio::test]
    async fn file_vault_reload_picks_up_new_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.toml");
        std::fs::write(
            &path,
            "[real]\ngh = \"OLD\"\n\n[[rewrite]]\nsearch=\"d\"\nreplace=\"gh\"\n",
        )
        .unwrap();
        let v = FileVault::load(&path).unwrap();
        assert_eq!(v.secret("gh").unwrap().as_str(), "OLD");
        std::fs::write(
            &path,
            "[real]\ngh = \"NEW\"\n\n[[rewrite]]\nsearch=\"d\"\nreplace=\"gh\"\n",
        )
        .unwrap();
        v.reload().await.unwrap();
        assert_eq!(v.secret("gh").unwrap().as_str(), "NEW");
    }
}
