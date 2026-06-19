//! Breadcrumb sink + table (§breadcrumbs-transcript). Pure bookkeeping:
//! assigns ids, stamps `timeline_ns` from the guest-local Timeline (§S2),
//! keeps a local `{breadcrumb_id -> timeline_ns}` table for clip resolution.

use async_trait::async_trait;
use qwanban_proto::id::BreadcrumbId;
use qwanban_proto::timeline::{Timeline, TimelineNs};
use qwanban_proto::transcript::{Breadcrumb, BreadcrumbIn};
use qwanban_proto::QwanResult;
use std::collections::HashMap;
use std::sync::Mutex;

/// The sink the MCP `breadcrumb` tool + cuxec auto-breadcrumbs call into.
#[async_trait]
pub trait BreadcrumbSink: Send + Sync {
    async fn emit(&self, b: BreadcrumbIn) -> QwanResult<Breadcrumb>;
    async fn make_clip(
        &self,
        from_ts: TimelineNs,
        to_ts: TimelineNs,
        label: String,
    ) -> QwanResult<qwanban_proto::clip::ClipAsset>;
    fn resolve(&self, id: &BreadcrumbId) -> Option<TimelineNs>;
}

/// In-memory implementation (timeline-join bookkeeping, fully unit-testable).
pub struct BreadcrumbTable {
    case_id: qwanban_proto::id::CaseId,
    timeline: Timeline,
    next: Mutex<u64>,
    table: Mutex<HashMap<BreadcrumbId, TimelineNs>>,
}

impl BreadcrumbTable {
    pub fn new(case_id: qwanban_proto::id::CaseId) -> Self {
        Self {
            case_id,
            timeline: Timeline::start(),
            next: Mutex::new(0),
            table: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl BreadcrumbSink for BreadcrumbTable {
    async fn emit(&self, b: BreadcrumbIn) -> QwanResult<Breadcrumb> {
        let mut n = self.next.lock().unwrap();
        let id = BreadcrumbId(format!("bc_{n}"));
        *n += 1;
        drop(n);
        let ts = self.timeline.now();
        self.table.lock().unwrap().insert(id.clone(), ts);
        Ok(Breadcrumb {
            breadcrumb_id: id,
            case_id: self.case_id.clone(),
            kind: b.kind,
            label: b.label,
            timeline_ns: ts,
            detail: b.detail,
        })
    }

    async fn make_clip(
        &self,
        from_ts: TimelineNs,
        to_ts: TimelineNs,
        label: String,
    ) -> QwanResult<qwanban_proto::clip::ClipAsset> {
        Ok(qwanban_proto::clip::ClipAsset {
            clip_id: qwanban_proto::id::ClipId::new(),
            case_id: self.case_id.clone(),
            label,
            start_ns: from_ts,
            end_ns: to_ts,
            bytes_hash: "todo".into(),
            bytes_len: 0,
            web_url: "todo".into(),
        })
    }

    fn resolve(&self, id: &BreadcrumbId) -> Option<TimelineNs> {
        self.table.lock().unwrap().get(id).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qwanban_proto::transcript::BreadcrumbKind;

    #[tokio::test]
    async fn emit_assigns_monotonic_timeline() {
        let t = BreadcrumbTable::new(qwanban_proto::id::CaseId::from_str_inner("c1"));
        let b1 = t
            .emit(BreadcrumbIn {
                kind: BreadcrumbKind::StepBegin,
                label: "s1".into(),
                detail: None,
            })
            .await
            .unwrap();
        // small delay so timeline advances
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let b2 = t
            .emit(BreadcrumbIn {
                kind: BreadcrumbKind::StepEnd,
                label: "s1".into(),
                detail: None,
            })
            .await
            .unwrap();
        assert!(b2.timeline_ns > b1.timeline_ns);
        assert_eq!(t.resolve(&b1.breadcrumb_id), Some(b1.timeline_ns));
    }
}
