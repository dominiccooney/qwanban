//! `qwanban-artifacts` — content-addressed store for recordings/clips/logs
//! (§artifact-store-and-clipping). Owns the storage layout, segment indices,
//! clip cutting (keyframe-aligned, no re-encode where possible), and web report
//! serving. Read-only web surface suitable for linking from PRs.

pub mod store;
pub mod index;
pub mod fs_store;
pub mod transcript_index;
pub mod clip_cutter;
pub mod web_report;

pub use store::ArtifactStore;
pub use index::SegmentIndex;
pub use fs_store::FsArtifactStore;
pub use transcript_index::TranscriptIndex;
pub use clip_cutter::{ClipCutter, ClipCutResult};
pub use web_report::{WebReport, StubWebReport};
