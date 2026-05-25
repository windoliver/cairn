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
    /// Per-scope capacity used when registering a new scope lazily.
    capacity: u32,
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

    /// Create a new `RateLimit` with no initial scopes, the given per-scope
    /// `capacity`, and an hourly refill interval.
    ///
    /// Scopes are registered lazily via [`RateLimit::add_scope`] when the
    /// first event for that scope arrives. This constructor is used by the
    /// registry to initialise a connector's budget before any events are
    /// processed — the registry does not know which scopes will be used until
    /// events arrive.
    #[must_use]
    pub fn empty_per_hour(capacity: u32) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            refill_interval: Duration::from_hours(1),
            capacity,
        }
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
            capacity,
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

    /// Lazily register a scope using the stored per-connector capacity.
    ///
    /// If the scope is already registered this is a no-op (the existing bucket
    /// is not reset). This is used by `process_event` to register a scope on
    /// the first event it sees for that scope, without requiring the registry
    /// to enumerate scopes up front.
    pub fn ensure_scope(&self, scope: &str) {
        let mut map = self.inner.lock().unwrap_or_else(|poisoned| {
            // Safety: the bucket state is fully derived from `self.capacity`;
            // discarding partial mutations from a panicking thread is safe here.
            poisoned.into_inner()
        });
        let capacity = self.capacity;
        map.entry(scope.to_owned()).or_insert_with(|| Bucket {
            remaining: capacity,
            capacity,
            last_refill: Instant::now(),
        });
    }

    /// Attempt to deduct `amount` tokens from the named `scope`'s bucket.
    ///
    /// Returns `Ok(())` if the bucket had enough tokens after an optional
    /// refill. Returns [`ConnectorError::BudgetExceeded`] if:
    ///
    /// - the scope was never registered, **or**
    /// - the bucket is exhausted after accounting for any refill.
    ///
    /// This is the legacy one-shot deduction API. For the reserve/release
    /// pattern used by `process_event` prefer [`Self::try_reserve`] +
    /// [`Self::refund`].
    pub fn charge(&self, scope: &str, amount: u32) -> Result<(), ConnectorError> {
        self.try_reserve(scope, amount)
    }

    /// Reserve `amount` tokens from the named `scope`'s bucket **before**
    /// performing the downstream operation.
    ///
    /// This is the first half of the reserve/release pattern used by
    /// `process_event` (Finding K). If the downstream operation succeeds the
    /// reservation is implicitly committed (tokens stay deducted). If it
    /// fails, call [`Self::refund`] to restore the tokens so the event can be
    /// retried without permanently reducing available budget.
    ///
    /// Semantically identical to [`Self::charge`]; provided as a separate
    /// entry-point to make the reserve/release intent explicit at call sites.
    ///
    /// Returns [`ConnectorError::BudgetExceeded`] if the scope is unknown or
    /// the bucket is exhausted.
    pub fn try_reserve(&self, scope: &str, amount: u32) -> Result<(), ConnectorError> {
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

    /// Refund `amount` tokens to the named `scope`'s bucket.
    ///
    /// This is the release half of the reserve/release pattern (Finding K).
    /// Call this when an operation that previously succeeded [`Self::try_reserve`]
    /// later fails (e.g. `emit` returns an error), so the reserved tokens are
    /// returned and the event can be retried on the next tick without
    /// permanently reducing available budget.
    ///
    /// The refund is clamped at `capacity` — adding back more tokens than the
    /// bucket can hold is silently ignored. This prevents a buggy call site
    /// from inflating the budget beyond its configured maximum.
    ///
    /// If the scope was never registered this is a no-op (the refund cannot
    /// make the budget negative, so silent discard is safe).
    pub fn refund(&self, scope: &str, amount: u32) {
        let mut map = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(bucket) = map.get_mut(scope) {
            bucket.remaining = bucket.remaining.saturating_add(amount).min(bucket.capacity);
        }
        // Unknown scope: no-op (see doc-comment).
    }
}

// ---------------------------------------------------------------------------
// ByteRateLimit — daily byte-volume cap (Finding R)
// ---------------------------------------------------------------------------

/// Per-scope byte-volume token bucket that limits how many bytes a connector
/// may emit within one 24-hour window (Finding R).
///
/// A new bucket starts with `capacity_bytes` available and drains by the number
/// of bytes in each accepted event. When the 24-hour interval has elapsed the
/// bucket is refilled to `capacity_bytes` on the next reservation attempt.
///
/// # Usage
///
/// - Construct with [`ByteRateLimit::new`] at connector-enable time, passing
///   `manifest.budget.max_bytes_per_day` as `capacity_bytes`.
/// - Call [`ByteRateLimit::ensure_scope`] the first time a scope is seen.
/// - Call [`ByteRateLimit::try_reserve`] before writing to spool.
/// - Call [`ByteRateLimit::refund`] if a subsequent step (spool, emit) fails.
#[derive(Debug)]
pub struct ByteRateLimit {
    inner: Mutex<HashMap<String, ByteBucket>>,
    /// Per-scope capacity in bytes, used when registering a scope lazily.
    capacity_bytes: u64,
}

#[derive(Debug)]
struct ByteBucket {
    remaining: u64,
    capacity: u64,
    last_refill: std::time::Instant,
}

impl ByteRateLimit {
    /// Create a new `ByteRateLimit` with no initial scopes and a 24-hour
    /// refill window.
    ///
    /// `capacity_bytes` is the maximum number of bytes the connector may emit
    /// in any 24-hour period. Derived from `manifest.budget.max_bytes_per_day`.
    #[must_use]
    pub fn new(capacity_bytes: u64) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            capacity_bytes,
        }
    }

    /// Lazily register a scope using the stored per-connector capacity.
    ///
    /// If the scope is already registered this is a no-op (the existing bucket
    /// is not reset). Call this before [`try_reserve`][Self::try_reserve] on
    /// the first event for each scope.
    pub fn ensure_scope(&self, scope: &str) {
        let mut map = self.inner.lock().unwrap_or_else(|poisoned| {
            // Discard partial mutations from a panicking thread — safe because
            // bucket state is fully derived from `self.capacity_bytes`.
            poisoned.into_inner()
        });
        let capacity = self.capacity_bytes;
        map.entry(scope.to_owned()).or_insert_with(|| ByteBucket {
            remaining: capacity,
            capacity,
            last_refill: std::time::Instant::now(),
        });
    }

    /// Attempt to reserve `bytes` from the named `scope`'s bucket.
    ///
    /// Refills the bucket to capacity if 24 hours have elapsed since the
    /// last refill. Returns [`ConnectorError::BudgetExceeded`] if the scope
    /// is unknown or the bucket does not have enough bytes remaining.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectorError::BudgetExceeded`] if the scope is unregistered
    /// or the daily byte cap has been reached.
    pub fn try_reserve(&self, scope: &str, bytes: u64) -> Result<(), ConnectorError> {
        let mut map = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let bucket = map
            .get_mut(scope)
            .ok_or_else(|| ConnectorError::BudgetExceeded {
                scope: scope.into(),
            })?;

        // Refill on 24-hour wall-clock interval.
        if bucket.last_refill.elapsed() >= Duration::from_hours(24) {
            bucket.remaining = bucket.capacity;
            bucket.last_refill = std::time::Instant::now();
        }

        if bucket.remaining < bytes {
            return Err(ConnectorError::BudgetExceeded {
                scope: scope.into(),
            });
        }
        bucket.remaining -= bytes;
        Ok(())
    }

    /// Refund `bytes` to the named `scope`'s bucket.
    ///
    /// Call this when an operation that previously succeeded
    /// [`try_reserve`][Self::try_reserve] later fails (e.g. emit returns an
    /// error). The refund is clamped at the bucket capacity. Unknown scopes
    /// are silently ignored.
    pub fn refund(&self, scope: &str, bytes: u64) {
        let mut map = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(bucket) = map.get_mut(scope) {
            bucket.remaining = bucket.remaining.saturating_add(bytes).min(bucket.capacity);
        }
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
