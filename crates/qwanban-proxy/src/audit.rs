//! Audit records (§mitm-proxy). Records which dummy matched (never the real
//! secret). Forwarded to the broker; the hook for future rate-limit/abuse work.

use async_trait::async_trait;
use qwanban_proto::id::CaseId;

#[derive(Debug, Clone)]
pub struct AuditRecord {
    pub case_id: CaseId,
    pub host: String,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub bytes_up: u64,
    pub bytes_down: u64,
    /// Which rewrite-table indices matched (NOT the secret).
    pub matched_dummies: Vec<usize>,
}

#[async_trait]
pub trait AuditSink: Send + Sync {
    async fn record(&self, r: AuditRecord);
}
