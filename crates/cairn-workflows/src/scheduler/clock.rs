//! Clock injection for the scheduler. The `JobStore` contract takes
//! `now_ms: i64` on every call; the scheduler owns the canonical
//! source of time. Tests use [`MockClock`] to drive lease expiry.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Source of wall-clock milliseconds.
pub trait Clock: Send + Sync + 'static {
    /// Current wall-clock in epoch milliseconds.
    fn now_ms(&self) -> i64;
}

/// Production clock backed by `SystemTime`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
    }
}

/// Test clock with manually advanced time.
#[derive(Debug, Clone)]
pub struct MockClock(Arc<AtomicI64>);

impl MockClock {
    /// Start at `start_ms`.
    #[must_use]
    pub fn at(start_ms: i64) -> Self {
        Self(Arc::new(AtomicI64::new(start_ms)))
    }
    /// Advance by `delta_ms`.
    pub fn advance(&self, delta_ms: i64) {
        self.0.fetch_add(delta_ms, Ordering::SeqCst);
    }
}

impl Clock for MockClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_clock_advances() {
        let c = MockClock::at(1_000);
        assert_eq!(c.now_ms(), 1_000);
        c.advance(500);
        assert_eq!(c.now_ms(), 1_500);
    }

    #[test]
    fn system_clock_returns_monotonic_positive() {
        let c = SystemClock;
        let a = c.now_ms();
        let b = c.now_ms();
        assert!(a > 0 && b >= a);
    }
}
