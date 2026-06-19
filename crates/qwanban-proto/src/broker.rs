//! Broker RPC request/response types (§broker-protocol). The broker is the
//! host-side service guests call for mediated operations over the private vSwitch.
//!
//! NOTE: these are the logical message shapes. The concrete transport (gRPC vs
//! HTTP/2) is decided in `qwanban-broker`; this module owns the *data*.

use crate::id::{CaseId, JobId};
use crate::timeline::TimelineOffsetNs;
use crate::transcript::TranscriptEntry;
use crate::video::VideoSegment;
use crate::clip::ClipAsset;
use serde::{Deserialize, Serialize};

/// `OpenCase` → guest gets a `case_token` + allowed models + a timeline offset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenCaseReq {
    pub case_id: CaseId,
    pub job_id: JobId,
    pub manifest: crate::manifest::Manifest,
    pub resource_caps: crate::config::ResourceCaps,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenCaseResp {
    pub case_token: String,
    pub allowed_models: Vec<String>,
    /// Presentation-only offset (0 for a normal case); §S2.
    pub timeline_offset_ns: TimelineOffsetNs,
}

/// Guest registers with the broker once the qwan agent is up (7.2.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterReq {
    pub case_id: CaseId,
    pub case_token: String,
    pub guest_info: GuestInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterResp {
    pub case_id: CaseId,
    pub timeline_offset_ns: TimelineOffsetNs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestInfo {
    pub os: GuestOs,
    pub arch: String,
    pub screen_w: u32,
    pub screen_h: u32,
    pub qwan_agent_version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GuestOs {
    Windows,
    Linux,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatReq {
    pub case_id: CaseId,
    pub status: CaseStatus,
    pub capture_health: CaptureHealth,
    pub queue_depths: QueueDepths,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatResp {
    pub directives: Vec<Directive>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseStatus {
    Booting,
    QwanAgentPushed,
    ClineAgentReady,
    Running,
    InterventionRequested,
    OsMigration,
    Completed,
    Failed,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureHealth {
    pub fps_observed: f32,
    pub dropped_frames: u64,
    pub encoder_ok: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueueDepths {
    pub transcript: u32,
    pub video: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Directive {
    /// Soft-stop: ask the agent to wrap up.
    SoftStop,
    /// Hard-stop: kill the VM shortly.
    HardStop { after_s: u32 },
    /// OS migration requested by the agent (7.11).
    MigrateOs { to: GuestOs },
    /// Resume after intervention hold.
    Resume,
}

/// Streaming ingest items the guest pushes to the broker.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IngestItem {
    Transcript(TranscriptEntry),
    VideoSegment(VideoSegment),
    ClipAsset(ClipAsset),
}

/// Final result of a case (7.12).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseResult {
    pub case_id: CaseId,
    pub result: CaseOutcome,
    pub summary: String,
    pub pr_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseOutcome {
    Pass,
    Fail,
    Fixed,
    Unreproducible,
    Error,
}
