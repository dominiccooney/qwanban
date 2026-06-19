//! Broker client (guest side). Registers (7.2.3), heartbeats (7.2.9), and pushes
//! transcript/video/clip ingest items (7.5/7.6/7.7). Transport is HTTP/gRPC over
//! the private vSwitch; this module owns the logical client + retry/queue logic.

use async_trait::async_trait;
use qwanban_proto::broker::{HeartbeatReq, HeartbeatResp, IngestItem, RegisterReq, RegisterResp};
use qwanban_proto::QwanResult;

#[async_trait]
pub trait BrokerClient: Send + Sync {
    async fn register(&self, req: RegisterReq) -> QwanResult<RegisterResp>;
    async fn heartbeat(&self, req: HeartbeatReq) -> QwanResult<HeartbeatResp>;
    async fn ingest(&self, item: IngestItem) -> QwanResult<()>;
}
