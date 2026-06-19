//! Segment index: maps `timeline_ns` ranges to stored video segments, so a
//! breadcrumb (timeline point) can resolve to the segment(s) covering it for
//! playback + clip cutting.

use parking_lot::Mutex;
use qwanban_proto::id::CaseId;
use qwanban_proto::timeline::TimelineNs;
use qwanban_proto::video::VideoSegment;
use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct SegmentIndex {
    by_case: Mutex<HashMap<CaseId, Vec<VideoSegment>>>,
}

impl SegmentIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn append(&self, seg: VideoSegment) {
        self.by_case
            .lock()
            .entry(seg.case_id.clone())
            .or_default()
            .push(seg);
    }

    /// Find the segment covering `timeline_ns` for a case, if any.
    pub fn segment_at(&self, case_id: &CaseId, timeline_ns: TimelineNs) -> Option<VideoSegment> {
        let guard = self.by_case.lock();
        guard
            .get(case_id)?
            .iter()
            .find(|s| timeline_ns >= s.start_ns && timeline_ns < s.end_ns)
            .cloned()
    }

    /// Segments overlapping `[from, to]` (for clip cutting).
    pub fn segments_in(
        &self,
        case_id: &CaseId,
        from: TimelineNs,
        to: TimelineNs,
    ) -> Vec<VideoSegment> {
        self.by_case
            .lock()
            .get(case_id)
            .map(|v| {
                v.iter()
                    .filter(|s| s.start_ns < to && s.end_ns > from)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qwanban_proto::id::VideoSegmentId;
    use qwanban_proto::video::VideoCodec;

    fn seg(case: &CaseId, idx: u32, start: i64, end: i64) -> VideoSegment {
        VideoSegment {
            segment_id: VideoSegmentId(format!("seg_{idx}")),
            case_id: case.clone(),
            index: idx,
            start_ns: start,
            end_ns: end,
            codec: VideoCodec::H264,
            width: 1280,
            height: 720,
            fps: 5.0,
            bytes_hash: "h".into(),
            bytes_len: 0,
        }
    }

    #[test]
    fn finds_segment_covering_point() {
        let idx = SegmentIndex::new();
        let c = CaseId::from_str_inner("c1");
        idx.append(seg(&c, 0, 0, 4_000_000_000));
        idx.append(seg(&c, 1, 4_000_000_000, 8_000_000_000));
        let s = idx.segment_at(&c, 5_000_000_000).unwrap();
        assert_eq!(s.index, 1);
    }

    #[test]
    fn segments_in_range() {
        let idx = SegmentIndex::new();
        let c = CaseId::from_str_inner("c1");
        idx.append(seg(&c, 0, 0, 4_000_000_000));
        idx.append(seg(&c, 1, 4_000_000_000, 8_000_000_000));
        idx.append(seg(&c, 2, 8_000_000_000, 12_000_000_000));
        // [3s, 9s] overlaps all three (seg2 starts at 8s < 9s)
        let v = idx.segments_in(&c, 3_000_000_000, 9_000_000_000);
        assert_eq!(v.len(), 3);
        // a tighter range [3s, 7s] overlaps only the first two
        let v = idx.segments_in(&c, 3_000_000_000, 7_000_000_000);
        assert_eq!(v.len(), 2);
    }
}
