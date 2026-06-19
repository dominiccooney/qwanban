//! The qwan MCP server (§mcp-server). **qwan-only tools**: breadcrumb, clip,
//! request_intervention, request_os_migration, finish. Computer control is NOT
//! here — it's the Anthropic computer-use tool executed by `cuxec`.

use crate::breadcrumbs::BreadcrumbSink;
use async_trait::async_trait;
use qwanban_proto::broker::{CaseResult, GuestOs};
use qwanban_proto::transcript::{BreadcrumbIn, BreadcrumbKind};
use qwanban_proto::QwanResult;
use std::sync::Arc;

#[async_trait]
pub trait HandoffSink: Send + Sync {
    async fn request_intervention(&self, reason: String) -> QwanResult<()>;
    async fn request_os_migration(&self, to: GuestOs, reason: String) -> QwanResult<()>;
}

#[async_trait]
pub trait FinishSink: Send + Sync {
    async fn finish(&self, result: CaseResult) -> QwanResult<()>;
}

#[derive(Debug, Clone)]
pub enum ToolResponse {
    Breadcrumb { id: String, timeline_ns: i64 },
    Clip { clip_id: String, web_url: String },
    Handoff { accepted: bool },
    Finish { accepted: bool },
    Error { message: String },
}

/// The qwan MCP server. Holds the breadcrumb table + optional handoff/finish sinks.
pub struct QwanMcpServer {
    breadcrumbs: Arc<dyn BreadcrumbSink>,
    handoff: Option<Arc<dyn HandoffSink>>,
    finish: Option<Arc<dyn FinishSink>>,
}

impl QwanMcpServer {
    pub fn new(breadcrumbs: Arc<dyn BreadcrumbSink>) -> Self {
        Self { breadcrumbs, handoff: None, finish: None }
    }
    pub fn with_handoff(mut self, sink: Arc<dyn HandoffSink>) -> Self {
        self.handoff = Some(sink);
        self
    }
    pub fn with_finish(mut self, sink: Arc<dyn FinishSink>) -> Self {
        self.finish = Some(sink);
        self
    }

    pub async fn tool_breadcrumb(&self, kind: BreadcrumbKind, label: String, detail: Option<String>) -> ToolResponse {
        match self.breadcrumbs.emit(BreadcrumbIn { kind, label, detail }).await {
            Ok(b) => ToolResponse::Breadcrumb { id: b.breadcrumb_id.as_str().to_string(), timeline_ns: b.timeline_ns },
            Err(e) => ToolResponse::Error { message: e.to_string() },
        }
    }

    pub async fn tool_clip(&self, from_ts: i64, to_ts: i64, label: String) -> ToolResponse {
        match self.breadcrumbs.make_clip(from_ts, to_ts, label).await {
            Ok(asset) => ToolResponse::Clip { clip_id: asset.clip_id.as_str().to_string(), web_url: asset.web_url },
            Err(e) => ToolResponse::Error { message: e.to_string() },
        }
    }

    pub async fn tool_request_intervention(&self, reason: String) -> ToolResponse {
        match &self.handoff {
            Some(sink) => match sink.request_intervention(reason).await {
                Ok(()) => ToolResponse::Handoff { accepted: true },
                Err(e) => ToolResponse::Error { message: e.to_string() },
            },
            None => ToolResponse::Error { message: "no handoff sink wired".into() },
        }
    }

    pub async fn tool_request_os_migration(&self, to: GuestOs, reason: String) -> ToolResponse {
        match &self.handoff {
            Some(sink) => match sink.request_os_migration(to, reason).await {
                Ok(()) => ToolResponse::Handoff { accepted: true },
                Err(e) => ToolResponse::Error { message: e.to_string() },
            },
            None => ToolResponse::Error { message: "no handoff sink wired".into() },
        }
    }

    pub async fn tool_finish(&self, outcome: qwanban_proto::broker::CaseOutcome, summary: String, pr_url: Option<String>) -> ToolResponse {
        match &self.finish {
            Some(sink) => {
                let result = CaseResult { case_id: qwanban_proto::id::CaseId::from_str_inner(""), result: outcome, summary, pr_url };
                match sink.finish(result).await {
                    Ok(()) => ToolResponse::Finish { accepted: true },
                    Err(e) => ToolResponse::Error { message: e.to_string() },
                }
            }
            None => ToolResponse::Error { message: "no finish sink wired".into() },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::breadcrumbs::BreadcrumbTable;
    use qwanban_proto::id::CaseId;
    use parking_lot::Mutex;

    struct MockHandoff { interventions: Mutex<Vec<String>> }
    #[async_trait]
    impl HandoffSink for MockHandoff {
        async fn request_intervention(&self, reason: String) -> QwanResult<()> {
            self.interventions.lock().push(reason); Ok(())
        }
        async fn request_os_migration(&self, _to: GuestOs, _reason: String) -> QwanResult<()> { Ok(()) }
    }

    struct MockFinish { called: Mutex<bool> }
    #[async_trait]
    impl FinishSink for MockFinish {
        async fn finish(&self, _result: CaseResult) -> QwanResult<()> {
            *self.called.lock() = true; Ok(())
        }
    }

    #[tokio::test]
    async fn breadcrumb_tool_emits_and_returns_id() {
        let bc = Arc::new(BreadcrumbTable::new(CaseId::from_str_inner("c1")));
        let mcp = QwanMcpServer::new(bc);
        let resp = mcp.tool_breadcrumb(BreadcrumbKind::StepBegin, "step1".into(), None).await;
        assert!(matches!(resp, ToolResponse::Breadcrumb { .. }));
    }

    #[tokio::test]
    async fn clip_tool_returns_clip_id() {
        let bc = Arc::new(BreadcrumbTable::new(CaseId::from_str_inner("c1")));
        let mcp = QwanMcpServer::new(bc);
        let resp = mcp.tool_clip(0, 1000, "repro".into()).await;
        assert!(matches!(resp, ToolResponse::Clip { .. }));
    }

    #[tokio::test]
    async fn handoff_without_sink_errors() {
        let bc = Arc::new(BreadcrumbTable::new(CaseId::from_str_inner("c1")));
        let mcp = QwanMcpServer::new(bc);
        let resp = mcp.tool_request_intervention("stuck".into()).await;
        assert!(matches!(resp, ToolResponse::Error { .. }));
    }

    #[tokio::test]
    async fn handoff_with_sink_accepted() {
        let bc = Arc::new(BreadcrumbTable::new(CaseId::from_str_inner("c1")));
        let handoff = Arc::new(MockHandoff { interventions: Mutex::new(vec![]) });
        let mcp = QwanMcpServer::new(bc).with_handoff(handoff.clone());
        let resp = mcp.tool_request_intervention("need help".into()).await;
        assert!(matches!(resp, ToolResponse::Handoff { accepted: true }));
        assert_eq!(handoff.interventions.lock().len(), 1);
    }

    #[tokio::test]
    async fn finish_with_sink_accepted() {
        let bc = Arc::new(BreadcrumbTable::new(CaseId::from_str_inner("c1")));
        let finish = Arc::new(MockFinish { called: Mutex::new(false) });
        let mcp = QwanMcpServer::new(bc).with_finish(finish.clone());
        let resp = mcp.tool_finish(qwanban_proto::broker::CaseOutcome::Pass, "done".into(), None).await;
        assert!(matches!(resp, ToolResponse::Finish { accepted: true }));
        assert!(*finish.called.lock());
    }
}