//! Wall-clock abstraction for code that needs an injectable `now()`.
//!
//! Production code uses [`SystemClock`]; tests use [`FixedClock`] (gated
//! behind the `test-helpers` feature so it ships only in dev builds).

use chrono::{DateTime, Utc};

/// Source of wall-clock time. Sync because every consumer needs `now()`
/// at decision points where awaiting is unwanted (verifier, expiry checks).
pub trait Clock: Send + Sync {
    /// Current UTC instant at call time.
    fn now(&self) -> DateTime<Utc>;
}

/// Production clock — delegates to [`chrono::Utc::now`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Test clock returning a fixed instant.
#[cfg(any(test, feature = "test-helpers"))]
#[derive(Debug, Clone, Copy)]
pub struct FixedClock(pub DateTime<Utc>);

#[cfg(any(test, feature = "test-helpers"))]
impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_clock_returns_its_instant() {
        let t: DateTime<Utc> = "2026-05-02T12:00:00Z".parse().unwrap();
        let c = FixedClock(t);
        assert_eq!(c.now(), t);
    }

    #[test]
    fn system_clock_advances_or_equals() {
        let a = SystemClock.now();
        let b = SystemClock.now();
        assert!(b >= a);
    }
}
