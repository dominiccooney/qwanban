//! Admission scheduler (§5.8). **Hard cap, reject immediately, no queue.**
//! `submit` accepts only if a `max_concurrent_cases` slot is free; otherwise it
//! returns `ResourceExhausted`. No queue, no bin-packing, no priority in v1.

use qwanban_proto::{QwanCode, QwanError, QwanResult};
use std::sync::atomic::{AtomicU32, Ordering};

/// A simple counting admission controller. Thread-safe.
pub struct Scheduler {
    max_concurrent: u32,
    live: AtomicU32,
}

impl Scheduler {
    pub fn new(max_concurrent: u32) -> Self {
        Self {
            max_concurrent,
            live: AtomicU32::new(0),
        }
    }

    /// Live (admitted, not-yet-torn-down) case count.
    pub fn live_count(&self) -> u32 {
        self.live.load(Ordering::Acquire)
    }

    /// Try to admit a case. Returns Err(ResourceExhausted) if no slot.
    pub fn admit(&self) -> QwanResult<Admission<'_>> {
        let prev = self.live.fetch_add(1, Ordering::AcqRel);
        if prev >= self.max_concurrent {
            // roll back
            self.live.fetch_sub(1, Ordering::AcqRel);
            return Err(QwanError::new(
                QwanCode::ResourceExhausted,
                format!(
                    "no free slot: {}/{} cases live",
                    prev, self.max_concurrent
                ),
            ));
        }
        Ok(Admission {
            live: &self.live,
            released: false,
        })
    }

    /// Manually release a slot (decrement the live count). Used when the RAII
    /// `Admission` guard is forgotten (e.g. stored across an await boundary in
    /// the orchestrator). Idempotent-safe: only call once per admitted slot.
    pub fn release(&self) {
        let prev = self.live.load(Ordering::Acquire);
        if prev > 0 {
            self.live.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// A RAII guard representing an admitted slot. Dropping releases it.
#[derive(Debug)]
pub struct Admission<'a> {
    live: &'a AtomicU32,
    released: bool,
}

impl Admission<'_> {
    /// Explicitly release the slot (also happens on Drop).
    pub fn release(mut self) {
        if !self.released {
            self.live.fetch_sub(1, Ordering::AcqRel);
            self.released = true;
        }
    }
}

impl Drop for Admission<'_> {
    fn drop(&mut self) {
        if !self.released {
            self.live.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admits_up_to_cap_then_rejects() {
        let s = Scheduler::new(2);
        let a = s.admit().unwrap();
        let b = s.admit().unwrap();
        assert_eq!(s.live_count(), 2);
        let err = s.admit().unwrap_err();
        assert_eq!(err.code(), QwanCode::ResourceExhausted);
        assert_eq!(s.live_count(), 2);
        drop(a);
        assert_eq!(s.live_count(), 1);
        let c = s.admit().unwrap();
        assert_eq!(s.live_count(), 2);
        drop(b);
        drop(c);
        assert_eq!(s.live_count(), 0);
    }

    #[test]
    fn zero_cap_rejects_everything() {
        let s = Scheduler::new(0);
        assert_eq!(s.admit().unwrap_err().code(), QwanCode::ResourceExhausted);
    }
}
