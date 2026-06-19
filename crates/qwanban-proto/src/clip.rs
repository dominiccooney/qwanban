//! Clip request/asset types (§artifact-store-and-clipping). Clips are cut from
//! the stored recording between two breadcrumb timeline points.

use crate::id::{CaseId, ClipId};
use crate::transcript::BreadcrumbRef;
use serde::{Deserialize, Serialize};

/// Input to the `clip` MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipRequest {
    pub case_id: CaseId,
    pub from: BreadcrumbRef,
    pub to: BreadcrumbRef,
    pub label: String,
}

/// A produced clip asset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipAsset {
    pub clip_id: ClipId,
    pub case_id: CaseId,
    pub label: String,
    pub start_ns: i64,
    pub end_ns: i64,
    pub bytes_hash: String,
    pub bytes_len: u64,
    /// Web URL for linking from PRs / reports.
    pub web_url: String,
}
