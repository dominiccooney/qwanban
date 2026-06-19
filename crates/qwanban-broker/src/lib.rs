//! `qwanban-broker` — host-side service guests call for mediated operations
//! (§broker-protocol). Owns: case registry (open/register/heartbeat), ingest
//! (transcript/video/clip), and the case-token↔case binding. The integration
//! harness drives 7.2→7.12 against a mock guest with no VM (dev-workflow.md).

pub mod case_registry;
pub mod ingest;
pub mod artifact_sink;

pub use case_registry::CaseRegistry;
pub use artifact_sink::ArtifactIngestSink;
