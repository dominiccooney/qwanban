//! `ArtifactIngestSink` — a concrete `IngestSink` that routes `IngestItem`
//! variants to the artifact store layout: transcript entries → TranscriptIndex,
//! video segments → per-segment metadata JSON, clip assets → clips directory.

use crate::case_registry::CaseRegistry;
use crate::ingest::IngestSink;
use async_trait::async_trait;
use qwanban_proto::broker::IngestItem;
use qwanban_proto::id::CaseId;
use qwanban_proto::QwanResult;
use std::path::PathBuf;
use std::sync::Arc;

pub struct ArtifactIngestSink {
    #[allow(dead_code)]
    registry: Arc<CaseRegistry>,
    root: PathBuf,
}

impl ArtifactIngestSink {
    pub fn new(registry: Arc<CaseRegistry>, root: impl Into<PathBuf>) -> Self {
        Self { registry, root: root.into() }
    }

    async fn handle_transcript(&self, entry: qwanban_proto::transcript::TranscriptEntry) -> QwanResult<()> {
        let idx = qwanban_artifacts::TranscriptIndex::new(&self.root);
        idx.append(&entry).await
    }

    async fn handle_video_segment(&self, seg: qwanban_proto::video::VideoSegment) -> QwanResult<()> {
        let case_dir = self.root.join("cases").join(seg.case_id.as_str()).join("video");
        tokio::fs::create_dir_all(&case_dir).await
            .map_err(|e| qwanban_proto::internal(format!("mkdir: {e}")))?;
        let meta_path = case_dir.join(format!("seg-{:06}.json", seg.index));
        let json = serde_json::to_string(&seg)
            .map_err(|e| qwanban_proto::internal(format!("serde: {e}")))?;
        tokio::fs::write(&meta_path, json).await
            .map_err(|e| qwanban_proto::internal(format!("write: {e}")))?;
        Ok(())
    }

    async fn handle_clip_asset(&self, asset: qwanban_proto::clip::ClipAsset) -> QwanResult<()> {
        let clips_dir = self.root.join("cases").join(asset.case_id.as_str()).join("clips");
        tokio::fs::create_dir_all(&clips_dir).await
            .map_err(|e| qwanban_proto::internal(format!("mkdir: {e}")))?;
        let json = serde_json::to_string(&asset)
            .map_err(|e| qwanban_proto::internal(format!("serde: {e}")))?;
        let path = clips_dir.join(format!("{}.json", asset.clip_id.as_str()));
        tokio::fs::write(&path, json).await
            .map_err(|e| qwanban_proto::internal(format!("write: {e}")))?;
        Ok(())
    }
}

#[async_trait]
impl IngestSink for ArtifactIngestSink {
    async fn ingest(&self, item: IngestItem) -> QwanResult<()> {
        match item {
            IngestItem::Transcript(e) => self.handle_transcript(e).await,
            IngestItem::VideoSegment(s) => self.handle_video_segment(s).await,
            IngestItem::ClipAsset(c) => self.handle_clip_asset(c).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qwanban_proto::id::{BreadcrumbId, VideoSegmentId};
    use qwanban_proto::transcript::{Breadcrumb, BreadcrumbKind, TranscriptEntry};
    use qwanban_proto::video::{VideoCodec, VideoSegment};
    use qwanban_proto::clip::ClipAsset;
    use qwanban_proto::id::ClipId;

    #[tokio::test]
    async fn transcript_routed_to_transcript_log() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Arc::new(CaseRegistry::new());
        let c = CaseId::from_str_inner("c1");
        reg.open(c.clone(), "tok".into());
        let sink = ArtifactIngestSink::new(reg, dir.path());
        let entry = TranscriptEntry::Breadcrumb(Breadcrumb {
            breadcrumb_id: BreadcrumbId("bc_0".into()),
            case_id: c.clone(),
            kind: BreadcrumbKind::StepBegin,
            label: "step1".into(),
            timeline_ns: 42,
            detail: None,
        });
        sink.ingest(IngestItem::Transcript(entry)).await.unwrap();
        let idx = qwanban_artifacts::TranscriptIndex::new(dir.path());
        assert_eq!(idx.all(&c).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn video_segment_routed_to_meta_json() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Arc::new(CaseRegistry::new());
        let c = CaseId::from_str_inner("c1");
        reg.open(c.clone(), "tok".into());
        let sink = ArtifactIngestSink::new(reg, dir.path());
        let seg = VideoSegment {
            segment_id: VideoSegmentId("seg_0".into()),
            case_id: c.clone(),
            index: 0,
            start_ns: 0,
            end_ns: 4_000_000_000,
            codec: VideoCodec::H264,
            width: 1280,
            height: 720,
            fps: 5.0,
            bytes_hash: "h0".into(),
            bytes_len: 1000,
        };
        sink.ingest(IngestItem::VideoSegment(seg)).await.unwrap();
        assert!(dir.path().join("cases").join("c1").join("video").join("seg-000000.json").exists());
    }

    #[tokio::test]
    async fn clip_asset_routed_to_clips_dir() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Arc::new(CaseRegistry::new());
        let c = CaseId::from_str_inner("c1");
        reg.open(c.clone(), "tok".into());
        let sink = ArtifactIngestSink::new(reg, dir.path());
        let asset = ClipAsset {
            clip_id: ClipId::from_str_inner("clip_1"),
            case_id: c,
            label: "repro".into(),
            start_ns: 0,
            end_ns: 1000,
            bytes_hash: "h".into(),
            bytes_len: 10,
            web_url: "/clips/clip_1".into(),
        };
        sink.ingest(IngestItem::ClipAsset(asset)).await.unwrap();
        assert!(dir.path().join("cases").join("c1").join("clips").join("clip_1.json").exists());
    }
}
