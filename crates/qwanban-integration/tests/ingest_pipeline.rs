//! Integration test: the full ingest pipeline.
//!
//! guest `BreadcrumbTable` (emits a breadcrumb with a real timeline_ns)
//!   → serialized as `IngestItem::Transcript`
//!   → `ArtifactIngestSink` ingests it (CaseRegistry auth + TranscriptIndex append)
//!   → `TranscriptIndex` reads it back
//!   → `StubWebReport` renders an HTML page containing the label
//!
//! Also tests video segment + clip asset ingest routing end-to-end. Proves the
//! proto/broker/artifacts/guest types compose with no shape mismatches.

use qwanban_artifacts::{StubWebReport, TranscriptIndex, WebReport};
use qwanban_broker::artifact_sink::ArtifactIngestSink;
use qwanban_broker::case_registry::CaseRegistry;
use qwanban_broker::ingest::IngestSink;
use qwanban_guest::breadcrumbs::{BreadcrumbSink, BreadcrumbTable};
use qwanban_proto::broker::IngestItem;
use qwanban_proto::clip::ClipAsset;
use qwanban_proto::id::CaseId;
use qwanban_proto::transcript::{BreadcrumbKind, TranscriptEntry};
use qwanban_proto::video::{VideoCodec, VideoSegment};
use std::sync::Arc;

#[tokio::test]
async fn transcript_flows_guest_to_broker_to_artifact_store_to_web() {
    let dir = tempfile::tempdir().unwrap();
    let case_id = CaseId::from_str_inner("case_pipeline");

    // 1. Guest side: emit a breadcrumb via the real BreadcrumbTable.
    let bc = BreadcrumbTable::new(case_id.clone());
    let breadcrumb = bc
        .emit(qwanban_proto::transcript::BreadcrumbIn {
            kind: BreadcrumbKind::Bug,
            label: "repro the null-deref crash".into(),
            detail: Some("happens on line 42".into()),
        })
        .await
        .unwrap();

    // 2. Broker side: the case must be open in the registry, then ingest.
    let registry = Arc::new(CaseRegistry::new());
    registry.open(case_id.clone(), "tok-123".into());
    let sink = ArtifactIngestSink::new(registry, dir.path());

    let entry = TranscriptEntry::Breadcrumb(breadcrumb);
    sink.ingest(IngestItem::Transcript(entry)).await.unwrap();

    // 3. Artifact store side: read the transcript back via TranscriptIndex.
    let idx = TranscriptIndex::new(dir.path());
    let all = idx.all(&case_id).unwrap();
    assert_eq!(all.len(), 1, "exactly one transcript entry should be stored");
    match &all[0] {
        TranscriptEntry::Breadcrumb(b) => {
            assert_eq!(b.label, "repro the null-deref crash");
            assert_eq!(b.kind, BreadcrumbKind::Bug);
        }
        _ => panic!("expected a Breadcrumb entry"),
    }

    // 4. Web report side: render a page and assert the label appears.
    let report = StubWebReport::new(dir.path(), TranscriptIndex::new(dir.path()));
    let html = report.render_case(&case_id).await.unwrap();
    assert!(html.contains("repro the null-deref crash"), "web report should contain the breadcrumb label");
    assert!(html.contains(&case_id.to_string()), "web report should name the case");
    eprintln!("[ingest_pipeline] rendered HTML ({} bytes):\n{}", html.len(), html);
}

#[tokio::test]
async fn video_segment_flows_broker_to_artifact_store() {
    let dir = tempfile::tempdir().unwrap();
    let case_id = CaseId::from_str_inner("case_video");
    let registry = Arc::new(CaseRegistry::new());
    registry.open(case_id.clone(), "tok".into());
    let sink = ArtifactIngestSink::new(registry, dir.path());

    let seg = VideoSegment {
        segment_id: qwanban_proto::id::VideoSegmentId::from_str_inner("seg_0"),
        case_id: case_id.clone(),
        index: 0,
        start_ns: 0,
        end_ns: 4_000_000_000,
        codec: VideoCodec::H264,
        width: 1280,
        height: 720,
        fps: 5.0,
        bytes_hash: "abc123".into(),
        bytes_len: 4096,
    };
    sink.ingest(IngestItem::VideoSegment(seg)).await.unwrap();

    let meta_path = dir
        .path()
        .join("cases")
        .join(case_id.as_str())
        .join("video")
        .join("seg-000000.json");
    assert!(meta_path.exists(), "segment metadata JSON should exist");
    let meta_text = std::fs::read_to_string(&meta_path).unwrap();
    let meta: serde_json::Value = serde_json::from_str(&meta_text).unwrap();
    assert_eq!(meta["bytes_hash"], "abc123");
    eprintln!("[ingest_pipeline] video segment metadata written to {}", meta_path.display());
}

#[tokio::test]
async fn clip_asset_flows_broker_to_artifact_store() {
    let dir = tempfile::tempdir().unwrap();
    let case_id = CaseId::from_str_inner("case_clip");
    let registry = Arc::new(CaseRegistry::new());
    registry.open(case_id.clone(), "tok".into());
    let sink = ArtifactIngestSink::new(registry, dir.path());

    let asset = ClipAsset {
        clip_id: qwanban_proto::id::ClipId::from_str_inner("clip_demo"),
        case_id: case_id.clone(),
        label: "evidence for PR #42".into(),
        start_ns: 1000,
        end_ns: 5000,
        bytes_hash: "h".into(),
        bytes_len: 999,
        web_url: "/clips/clip_demo".into(),
    };
    sink.ingest(IngestItem::ClipAsset(asset)).await.unwrap();

    let clip_path = dir
        .path()
        .join("cases")
        .join(case_id.as_str())
        .join("clips")
        .join("clip_demo.json");
    assert!(clip_path.exists(), "clip asset JSON should exist");
    eprintln!("[ingest_pipeline] clip asset written to {}", clip_path.display());
}
