//! The qwan MCP server (§mcp-server). **qwan-only tools**: breadcrumb, clip,
//! request_intervention, request_os_migration, finish. Computer control is NOT
//! here — it's the Anthropic computer-use tool executed by `cuxec`.

use async_trait::async_trait;
use qwanban_proto::QwanResult;

#[async_trait]
pub trait HandoffSink: Send + Sync {
    async fn request_intervention(&self, reason: String) -> QwanResult<()>;
    async fn request_os_migration(&self, to: qwanban_proto::broker::GuestOs, reason: String) -> QwanResult<()>;
}

#[async_trait]
pub trait FinishSink: Send + Sync {
    async fn finish(&self, result: qwanban_proto::broker::CaseResult) -> QwanResult<()>;
}

/// Placeholder for the MCP server wiring; concrete impl owns the loopback
/// transport (stdio / 127.0.0.1) the launched agent connects to.
pub struct QwanMcpServer;
