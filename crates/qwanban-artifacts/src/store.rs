//! Content-addressed storage. Bytes are keyed by sha256; metadata by case.

use async_trait::async_trait;
use qwanban_proto::QwanResult;
use sha2::{Digest, Sha256};

#[async_trait]
pub trait ArtifactStore: Send + Sync {
    async fn put(&self, bytes: Vec<u8>) -> QwanResult<String>; // returns sha256 hex
    async fn get(&self, hash: &str) -> QwanResult<Vec<u8>>;
}

/// Compute the content-addressed key for a blob.
pub fn content_hash(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex_encode(&h.finalize())
}

fn hex_encode(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for byte in b {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_is_deterministic() {
        let a = content_hash(b"hello");
        let b = content_hash(b"hello");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        // different content -> different hash
        assert_ne!(a, content_hash(b"world"));
    }
}
