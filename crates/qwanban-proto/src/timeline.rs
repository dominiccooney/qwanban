//! The timeline model (components README §S2).
//!
//! **There is no cross-machine clock synchronization.** The guest owns one
//! monotonic clock per case. The capture pipeline sets `t0 = monotonic_now()` at
//! the first frame; everything (breadcrumbs, video fragments, input events, clips)
//! stamps `timeline_ns = monotonic_now() - t0`. Because the guest authors both
//! the video and the transcript off the same clock, a breadcrumb indexes into the
//! video **exact by construction** — no skew, no slew.
//!
//! The host stores `timeline_ns` verbatim and never reinterprets it. The only
//! host-side clock-ish field is `timeline_offset_ns` (presentation-only, 0 for a
//! normal case), used solely to concatenate multiple cases of an OS-migrated job
//! into one report timeline.

use serde::{Deserialize, Serialize};
use std::time::Instant;

/// A point on the guest-local case timeline, in nanoseconds since `t0`.
pub type TimelineNs = i64;

/// The guest-local case timeline handle. Created at case start; cloned cheaply
/// (the `Instant` and origin are shared by copy).
#[derive(Debug, Clone)]
pub struct Timeline {
    t0: Instant,
}

impl Timeline {
    /// Start a new case timeline. The first call to `now()` returns ~0.
    pub fn start() -> Self {
        Self { t0: Instant::now() }
    }

    /// Current `timeline_ns`.
    pub fn now(&self) -> TimelineNs {
        self.t0.elapsed().as_nanos() as i64
    }
}

impl Default for Timeline {
    fn default() -> Self {
        Self::start()
    }
}

/// Presentation-only offset used by the host to stitch multiple cases of an
/// OS-migrated job into a single report timeline. **0 for a normal case.**
/// The guest never sees this.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineOffsetNs(pub i64);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_is_monotonic_nonneg() {
        let t = Timeline::start();
        let a = t.now();
        std::thread::sleep(std::time::Duration::from_millis(1));
        let b = t.now();
        assert!(a >= 0);
        assert!(b > a, "timeline must advance: a={a} b={b}");
    }

    #[test]
    fn timeline_starts_near_zero() {
        let t = Timeline::start();
        // within a millisecond of zero
        assert!(t.now() < 1_000_000, "t0 should be ~0");
    }
}
