//! Shared wire types, identifiers, timeline model, and error types for qwanban.
//!
//! This crate is the **foundational dependency** everything else builds on
//! (components README §S1–S8). It contains no I/O and no async runtime; pure data
//! so it can be unit-tested in the dev VM without Hyper-V or networking.
//!
//! Modules mirror the shared-contract sections of the design:
//! - [`id`] — typed identifiers (§S3)
//! - [`timeline`] — the guest-local monotonic timeline model (§S2)
//! - [`error`] — `QwanError` + canonical codes (§S5)
//! - [`manifest`] — the per-case manifest written into the guest (§agent-lifecycle)
//! - [`transcript`] — breadcrumb/transcript entry types (§breadcrumbs-transcript)
//! - [`video`] — `VideoSegment` + segment index (§video-capture-encode)
//! - [`broker`] — broker RPC request/response types (§broker-protocol)
//! - [`clip`] — clip request/asset (§artifact-store-and-clipping)
//! - [`input`] — `InputEvent`/`InputAck` + Anthropic computer-use action mapping (§input-injection)
//! - [`config`] — host `qwanban.toml` + image registry + resource caps (§5.1, §5.8)

pub mod id;
pub mod timeline;
pub mod error;
pub mod manifest;
pub mod transcript;
pub mod video;
pub mod broker;
pub mod clip;
pub mod input;
pub mod config;

pub use error::{QwanError, QwanResult, QwanCode, invalid_arg, not_found, internal};
pub use id::*;
