//! Breadcrumb + transcript types (§breadcrumbs-transcript). The guest authors
//! both the transcript and the video off one monotonic clock (§S2 timeline), so a
//! breadcrumb's `timeline_ns` indexes the video exact by construction.

use crate::id::{BreadcrumbId, CaseId, InputEventId};
use crate::timeline::TimelineNs;
use serde::{Deserialize, Serialize};

/// A breadcrumb marks a point in the case timeline the agent wants to be able to
/// jump to / clip from later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Breadcrumb {
    pub breadcrumb_id: BreadcrumbId,
    pub case_id: CaseId,
    pub kind: BreadcrumbKind,
    pub label: String,
    pub timeline_ns: TimelineNs,
    pub detail: Option<String>,
}

/// Input to `BreadcrumbSink::emit` (the id/timeline are assigned by the sink).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BreadcrumbIn {
    pub kind: BreadcrumbKind,
    pub label: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BreadcrumbKind {
    StepBegin,
    StepEnd,
    Assertion,
    Action,
    Note,
    Bug,
    Fix,
    Error,
}

/// A reference to a breadcrumb by id, used by the `clip` tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BreadcrumbRef {
    Id(BreadcrumbId),
    /// "latest" | "first" | an offset like "+5s"/"-3s" relative to another ref.
    Relative(String),
}

/// One entry in the ordered transcript stream. Every entry carries `timeline_ns`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptEntry {
    Breadcrumb(Breadcrumb),
    ToolIo {
        case_id: CaseId,
        event_id: InputEventId,
        timeline_ns: TimelineNs,
        summary: String,
    },
    Log {
        case_id: CaseId,
        timeline_ns: TimelineNs,
        level: LogLevel,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}
