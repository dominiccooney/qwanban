//! Video segment types (§video-capture-encode). Segments are uploaded by the
//! guest; the host indexes them by `timeline_ns` range for clipping + playback.

use crate::id::{CaseId, VideoSegmentId};
use crate::timeline::TimelineNs;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoSegment {
    pub segment_id: VideoSegmentId,
    pub case_id: CaseId,
    pub index: u32,
    /// Inclusive start on the case timeline.
    pub start_ns: TimelineNs,
    /// Exclusive end on the case timeline.
    pub end_ns: TimelineNs,
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub fps: f32,
    pub bytes_hash: String,
    pub bytes_len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoCodec {
    H264,
    Av1,
    Vp9,
}

/// Screenshot pull format for the computer-use backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImgFmt {
    Png,
    Jpeg,
}
