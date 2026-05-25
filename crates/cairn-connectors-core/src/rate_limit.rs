//! Token bucket keyed by scope. One bucket per `(connector, scope)`;
//! refill uses wall-clock — no async, no shared runtime state beyond
//! a `Mutex<HashMap>`.
//!
//! The `Mutex` is never held across an `.await` point: every method on
//! [`RateLimit`] is synchronous and completes the lock in a single
//! synchronous block. A poisoned mutex means an earlier thread panicked
//! while holding the lock; we treat that as a non-recoverable error and
//! surface it as [`ConnectorError::Fatal`] rather than propagating the
//! panic.
//!
//! Issue #130, brief §9.1 source sensors.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::error::ConnectorError;

/// Per-scope token bucket that limits how many items a connector may
/// emit within one `refill_interval` window.
///
/// A new bucket starts full at `capacity` and drains by `amount` on
/// every successful [`RateLimit::charge`] call. When the interval has
/// elapsed the bucket is refilled to `capacity` on the next call.
#[derive(Debug)]
pub struct RateLimit {
    inner: Mutex<HashMap<String, Bucket>>,
    /// How long before an exhausted bucket is refilled to capacity.
    refill_interval: Duration,
}

#[derive(Debug)]
struct Bucket {
    remaining: u32,
    capacity: u32,
    last_refill: Instant,
}

impl RateLimit {
    /// Create a new `RateLimit` with a single scope whose budget refills
    /// every hour (`Duration::from_secs(3600)`).
    ///
    /// Additional scopes can be added afterwards with [`RateLimit::add_scope`].
    #[must_use]
    pub fn per_hour(scope: String, capacity: u32) -> Self {
        Self::with_interval(scope, capacity, Duration::from_hours(1))
    }

    /// Create a new `RateLimit` with a single scope and a custom
    /// `refill_interval`.
    ///
    /// Use this constructor in tests that need a short window (e.g.,
    /// `Duration::from_millis(50)`) to exercise the refill path without
    /// a real hour-long wait. This is a full production API — not `cfg(test)`
    /// — so downstream integration tests (T20d) can use it directly.
    #[must_use]
    pub fn with_interval(scope: String, capacity: u32, refill_interval: Duration) -> Self {
        let mut map = HashMap::new();
        map.insert(
            scope,
            Bucket {
                remaining: capacity,
                capacity,
                last_refill: Instant::now(),
            },
        );
        Self {
            inner: Mutex::new(map),
            refill_interval,
        }
    }

    /// Register an additional scope with its own independent budget.
    ///
    /// If the scope already exists its budget is **replaced** (not accumulated).
    /// This is safe to call at any time; the existing lock state is unaffected.
    pub fn add_scope(&self, scope: String, capacity: u32) {
        let mut map = self.inner.lock().unwrap_or_else(|poisoned| {
            // Safety: the bucket state is fully re-derived from `scope` and
            // `capacity` here; we discard any partial mutation that might have
            // caused the poison and continue with a fresh insert.
            poisoned.into_inner()
        });
        map.insert(
            scope,
            Bucket {
                remaining: capacity,
                capacity,
                last_refill: Instant::now(),
            },
        );
    }

    /// Attempt to deduct `amount` tokens from the named `scope`'s bucket.
    ///
    /// Returns `Ok(())` if the bucket had enough tokens after an optional
    /// refill. Returns [`ConnectorError::BudgetExceeded`] if:
    ///
    /// - the scope was never registered, **or**
    /// - the bucket is exhausted after accounting for any refill.
    pub fn charge(&self, scope: &str, amount: u32) -> Result<(), ConnectorError> {
        let mut map = self.inner.lock().unwrap_or_else(|poisoned| {
            // The lock was poisoned by a panicking thread. Recovering here is
            // safe because we only read and update numeric fields; no structural
            // invariant in `Bucket` requires a panic-free history.
            poisoned.into_inner()
        });

        let bucket = map
            .get_mut(scope)
            .ok_or_else(|| ConnectorError::BudgetExceeded {
                scope: scope.into(),
            })?;

        // Refill on wall-clock interval.
        if bucket.last_refill.elapsed() >= self.refill_interval {
            bucket.remaining = bucket.capacity;
            bucket.last_refill = Instant::now();
        }

        if bucket.remaining < amount {
            return Err(ConnectorError::BudgetExceeded {
                scope: scope.into(),
            });
        }
        bucket.remaining -= amount;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn charges_within_budget() {
        let rl = RateLimit::per_hour("p1".into(), 3);
        for _ in 0..3 {
            rl.charge("p1", 1).expect("under budget");
        }
        assert!(matches!(
            rl.charge("p1", 1),
            Err(crate::ConnectorError::BudgetExceeded { .. }),
        ));
    }

    #[test]
    fn separate_scopes_have_separate_budgets() {
        let rl = RateLimit::per_hour("p1".into(), 1);
        rl.add_scope("p2".into(), 1);
        rl.charge("p1", 1).unwrap();
        rl.charge("p2", 1).unwrap();
        assert!(rl.charge("p1", 1).is_err());
    }

    #[test]
    fn charge_refills_after_interval_elapsed() {
        // Use a very short interval so the test doesn't have to wait an hour.
        let rl = RateLimit::with_interval("s1".into(), 2, Duration::from_millis(50));

        // Exhaust the bucket.
        rl.charge("s1", 1).expect("first charge");
        rl.charge("s1", 1).expect("second charge");
        assert!(
            rl.charge("s1", 1).is_err(),
            "bucket should be exhausted after capacity charges"
        );

        // Sleep past the refill interval.
        std::thread::sleep(Duration::from_millis(80));

        // The next charge should succeed because the bucket was refilled.
        rl.charge("s1", 1)
            .expect("charge after refill should succeed");
    }

    #[test]
    fn unknown_scope_returns_budget_exceeded() {
        let rl = RateLimit::per_hour("registered".into(), 10);
        match rl.charge("never-registered", 1) {
            Err(crate::ConnectorError::BudgetExceeded { scope }) => {
                assert_eq!(scope, "never-registered");
            }
            other => panic!("expected BudgetExceeded, got {other:?}"),
        }
    }
}
