//! Ingest: receives transcript/video/clip items from guests and forwards to the
//! artifact store. (Stub: the artifact-store contract is owned by qwanban-artifacts.)

use async_trait::async_trait;
use qwanban_proto::broker::IngestItem;
use qwanban_proto::QwanResult;

#[async_trait]
pub trait IngestSink: Send + Sync {
    async fn ingest(&self, item: IngestItem) -> QwanResult<()>;
}
