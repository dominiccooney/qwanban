//! `ClipCutter` — cuts a labeled clip between two `timeline_ns` points. Resolves
//! the overlapping segments from the SegmentIndex and produces a ClipAsset
//! referencing them. v1 does **no actual video remux** — it records the source
//! segment references + metadata so the web player can play the range and a
//! future transcode step can materialize a standalone file.

use crate::index::SegmentIndex;
use qwanban_proto::clip::ClipAsset;
use qwanban_proto::id::{CaseId, ClipId};
use qwanban_proto::timeline::TimelineNs;
use qwanban_proto::video::VideoSegment;
use qwanban_proto::QwanResult;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct ClipCutResult {
    pub asset: ClipAsset,
    pub source_segments: Vec<VideoSegment>,
}

pub struct ClipCutter {
    root: PathBuf,
    segments: SegmentIndex,
}

impl ClipCutter {
    pub fn new(root: impl Into<PathBuf>, segments: SegmentIndex) -> Self {
        Self { root: root.into(), segments }
    }

    /// Cut a clip over `[from_ns, to_ns]`. Idempotent by `clip_id`.
    pub async fn cut(
        &self,
        case_id: &CaseId,
        clip_id: &ClipId,
        from_ns: TimelineNs,
        to_ns: TimelineNs,
        label: String,
    ) -> QwanResult<ClipCutResult> {
        if to_ns <= from_ns {
            return Err(qwanban_proto::invalid_arg("to_ns must be > from_ns"));
        }
        let sidecar = self.clip_sidecar_path(case_id, clip_id);
        if sidecar.exists() {
            let text = tokio::fs::read_to_string(&sidecar).await
                .map_err(|e| qwanban_proto::internal(format!("read sidecar: {e}")))?;
            let asset: ClipAsset = serde_json::from_str(&text)
                .map_err(|e| qwanban_proto::internal(format!("parse sidecar: {e}")))?;
            let source_segments = self.segments.segments_in(case_id, from_ns, to_ns);
            return Ok(ClipCutResult { asset, source_segments });
        }
        let source_segments = self.segments.segments_in(case_id, from_ns, to_ns);
        if source_segments.is_empty() {
            return Err(qwanban_proto::not_found(format!(
                "no video segments cover [{from_ns},{to_ns}] for case {case_id}"
            )));
        }
        let bytes_len: u64 = source_segments.iter().map(|s| s.bytes_len).sum();
        let bytes_hash = source_segments.iter().map(|s| s.bytes_hash.as_str()).collect::<Vec<_>>().join("+");
        let asset = ClipAsset {
            clip_id: clip_id.clone(),
            case_id: case_id.clone(),
            label,
            start_ns: from_ns,
            end_ns: to_ns,
            bytes_hash,
            bytes_len,
            web_url: format!("/jobs/_/cases/{}/clips/{}", case_id.as_str(), clip_id.as_str()),
        };
        if let Some(parent) = sidecar.parent() {
            tokio::fs::create_dir_all(parent).await
                .map_err(|e| qwanban_proto::internal(format!("mkdir: {e}")))?;
        }
        let json = serde_json::to_string(&asset).map_err(|e| qwanban_proto::internal(format!("serde: {e}")))?;
        tokio::fs::write(&sidecar, json).await
            .map_err(|e| qwanban_proto::internal(format!("write sidecar: {e}")))?;
        Ok(ClipCutResult { asset, source_segments })
    }

    fn clip_sidecar_path(&self, case_id: &CaseId, clip_id: &ClipId) -> PathBuf {
        self.root.join("cases").join(case_id.as_str()).join("clips").join(format!("{}.json", clip_id.as_str()))
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
            bytes_hash: format!("hash_{idx}"),
            bytes_len: 1000,
        }
    }

    #[tokio::test]
    async fn cut_references_overlapping_segments() {
        let dir = tempfile::tempdir().unwrap();
        let idx = SegmentIndex::new();
        let c = CaseId::from_str_inner("c1");
        idx.append(seg(&c, 0, 0, 4_000_000_000));
        idx.append(seg(&c, 1, 4_000_000_000, 8_000_000_000));
        let cutter = ClipCutter::new(dir.path(), idx);
        let clip_id = ClipId::new();
        let res = cutter.cut(&c, &clip_id, 3_000_000_000, 5_000_000_000, "repro".into()).await.unwrap();
        assert_eq!(res.source_segments.len(), 2);
        assert_eq!(res.asset.label, "repro");
        assert_eq!(res.asset.bytes_len, 2000);
    }

    #[tokio::test]
    async fn cut_idempotent_by_clip_id() {
        let dir = tempfile::tempdir().unwrap();
        let idx = SegmentIndex::new();
        let c = CaseId::from_str_inner("c1");
        idx.append(seg(&c, 0, 0, 4_000_000_000));
        let cutter = ClipCutter::new(dir.path(), idx);
        let clip_id = ClipId::new();
        let r1 = cutter.cut(&c, &clip_id, 0, 4_000_000_000, "x".into()).await.unwrap();
        let r2 = cutter.cut(&c, &clip_id, 0, 4_000_000_000, "x".into()).await.unwrap();
        assert_eq!(r1.asset.clip_id, r2.asset.clip_id);
    }

    #[tokio::test]
    async fn cut_no_segments_errors() {
        let dir = tempfile::tempdir().unwrap();
        let idx = SegmentIndex::new();
        let c = CaseId::from_str_inner("c1");
        let cutter = ClipCutter::new(dir.path(), idx);
        let clip_id = ClipId::new();
        let err = cutter.cut(&c, &clip_id, 0, 1_000_000_000, "x".into()).await.unwrap_err();
        assert_eq!(err.code(), qwanban_proto::QwanCode::NotFound);
    }
}
