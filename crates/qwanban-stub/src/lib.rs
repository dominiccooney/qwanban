//! `qwan-stub` — the hvsocket stub loader baked into every base image (§stub-loader).
//! Single canonical bootstrap mechanism: push agent + files, launch, relay stdio.
//! **No SSH.** The protocol codec is pure logic and fully unit-testable here.

pub mod protocol;
pub mod serve;

pub use protocol::{Frame, FrameKind, Hello, PushAgent, WriteFile, Launch, is_ok};
pub use serve::{serve, ServeConfig, ServeOutcome};
