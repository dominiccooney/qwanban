//! `qwanban-artifacts` — content-addressed store for recordings/clips/logs
//! (§artifact-store-and-clipping). Owns the storage layout, segment indices,
//! clip cutting (keyframe-aligned, no re-encode where possible), and web report
//! serving. Read-only web surface suitable for linking from PRs.

pub mod store;
pub mod index;

pub use store::ArtifactStore;
pub use index::SegmentIndex;
