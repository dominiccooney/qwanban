//! `FsArtifactStore` — filesystem-backed content-addressed blob store.
//! Bytes are keyed by sha256; stored under `<root>/blobs/<hash>`. Dedup: a
//! second put of the same content is a no-op.

use crate::store::{content_hash, ArtifactStore};
use async_trait::async_trait;
use qwanban_proto::QwanResult;
use std::path::PathBuf;

/// A filesystem-backed content-addressed store.
#[derive(Debug, Clone)]
pub struct FsArtifactStore {
    root: PathBuf,
}

impl FsArtifactStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn blob_path(&self, hash: &str) -> PathBuf {
        self.root.join("blobs").join(hash)
    }
}

#[async_trait]
impl ArtifactStore for FsArtifactStore {
    async fn put(&self, bytes: Vec<u8>) -> QwanResult<String> {
        let hash = content_hash(&bytes);
        let path = self.blob_path(&hash);
        if !path.exists() {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await.map_err(|e| {
                    qwanban_proto::internal(format!("mkdir {}: {e}", parent.display()))
                })?;
            }
            tokio::fs::write(&path, &bytes).await.map_err(|e| {
                qwanban_proto::internal(format!("write {}: {e}", path.display()))
            })?;
        }
        Ok(hash)
    }

    async fn get(&self, hash: &str) -> QwanResult<Vec<u8>> {
        let path = self.blob_path(hash);
        tokio::fs::read(&path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                qwanban_proto::not_found(format!("blob {hash}"))
            } else {
                qwanban_proto::internal(format!("read {}: {e}", path.display()))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn put_get_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let s = FsArtifactStore::new(dir.path());
        let h = s.put(b"hello world".to_vec()).await.unwrap();
        let got = s.get(&h).await.unwrap();
        assert_eq!(got, b"hello world");
    }

    #[tokio::test]
    async fn put_dedups() {
        let dir = tempfile::tempdir().unwrap();
        let s = FsArtifactStore::new(dir.path());
        let h1 = s.put(b"same".to_vec()).await.unwrap();
        let h2 = s.put(b"same".to_vec()).await.unwrap();
        assert_eq!(h1, h2);
        // the blob file exists exactly once
        let blob = dir.path().join("blobs").join(&h1);
        assert!(blob.exists());
    }

    #[tokio::test]
    async fn get_missing_returns_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let s = FsArtifactStore::new(dir.path());
        let err = s.get("nonexistent").await.unwrap_err();
        assert_eq!(err.code(), qwanban_proto::QwanCode::NotFound);
    }
}
