//! Lock-table errors + structured retry hints (brief §5.6).
//!
//! `Held` and `Fenced` carry enough metadata for callers to render
//! actionable messages (`current_holder`, `ttl_remaining`, `retry`)
//! without re-querying the lock table.

use std::time::Duration;

use thiserror::Error;

use super::kinds::LockMode;

/// Structured retry guidance returned with every retryable `LockError`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RetryHint {
    /// Caller should retry with exponential backoff between `initial` and `max`.
    BackoffJitter {
        /// Initial backoff before first retry.
        initial: Duration,
        /// Maximum backoff between retries.
        max: Duration,
    },
    /// Caller should call `wait_for_drain(resource, suggested_timeout)` before retry.
    WaitForDrain {
        /// Resource string to wait on.
        resource: String,
        /// Suggested timeout for `wait_for_drain`.
        suggested_timeout: Duration,
    },
    /// Terminal: no retry will succeed (e.g. validation failure).
    NoRetry,
}

/// Lock-table errors.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LockError {
    /// Resource is held by a non-expired incumbent.
    #[error(
        "lock held: resource={resource} mode={mode} held_by={current_holder} \
         {since_ms}ms ago, ttl_remaining={ttl_remaining_ms}ms, retry={retry:?}"
    )]
    Held {
        /// Resource string (`ResourceKey::as_resource_str`).
        resource: String,
        /// Mode of the incumbent holder.
        mode: LockMode,
        /// Verb / operation requesting the lock (caller-supplied).
        operation: String,
        /// `holder_id` of the incumbent.
        current_holder: String,
        /// Milliseconds until incumbent's `expires_at`.
        ttl_remaining_ms: i64,
        /// Milliseconds since incumbent acquired.
        since_ms: i64,
        /// Structured retry guidance.
        retry: RetryHint,
    },

    /// Per-holder fencing CAS failed: epoch advanced or holder reclaimed.
    #[error(
        "fencing CAS failed: resource={resource} expected_epoch={expected_epoch} \
         observed={observed_epoch} — holder reclaimed"
    )]
    Fenced {
        /// Resource string.
        resource: String,
        /// Epoch the holder cached at acquisition.
        expected_epoch: i64,
        /// Epoch observed at CAS time.
        observed_epoch: i64,
        /// Structured retry guidance.
        retry: RetryHint,
    },

    /// `wait_for_drain` timed out with PENDING reader fences remaining.
    #[error(
        "draining timeout: {pending} reader-fence rows still PENDING for \
         resource={resource} after {waited_ms}ms"
    )]
    DrainTimeout {
        /// Resource string.
        resource: String,
        /// Count of still-PENDING fence rows.
        pending: i64,
        /// Time spent waiting before timeout, in milliseconds.
        waited_ms: u128,
        /// Structured retry guidance.
        retry: RetryHint,
    },

    /// Underlying `SQLite` / `tokio_rusqlite` error.
    #[error("lock db error")]
    Db(#[source] tokio_rusqlite::Error),

    /// System clock is before UNIX epoch.
    #[error("system clock pre-epoch")]
    Clock,

    /// `init_incarnation` was never called on this `Store`.
    #[error("daemon incarnation not initialized — call init_incarnation after migrations")]
    NoIncarnation,
}

impl From<tokio_rusqlite::Error> for LockError {
    fn from(e: tokio_rusqlite::Error) -> Self {
        Self::Db(e)
    }
}

/// Default `RetryHint` for a `Held` error: 50ms initial, 5s max backoff.
#[must_use]
pub fn default_held_retry() -> RetryHint {
    RetryHint::BackoffJitter {
        initial: Duration::from_millis(50),
        max: Duration::from_secs(5),
    }
}

/// Default `RetryHint` for a `Fenced` error: same backoff as `Held` —
/// fencing failure is transient (re-acquire and retry the WAL op).
#[must_use]
pub fn default_fenced_retry() -> RetryHint {
    RetryHint::BackoffJitter {
        initial: Duration::from_millis(50),
        max: Duration::from_secs(5),
    }
}

/// Default `RetryHint` for `DrainTimeout`: tells caller to wait again with longer timeout.
#[must_use]
pub fn default_drain_retry(resource: String) -> RetryHint {
    RetryHint::WaitForDrain {
        resource,
        suggested_timeout: Duration::from_secs(30),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_display_includes_owner_operation_ttl_retry() {
        let e = LockError::Held {
            resource: "vault:v1".into(),
            mode: LockMode::Exclusive,
            operation: "lint --fix-markdown".into(),
            current_holder: "pid=42-01HQZ".into(),
            ttl_remaining_ms: 4500,
            since_ms: 500,
            retry: default_held_retry(),
        };
        let s = format!("{e}");
        assert!(s.contains("vault:v1"));
        assert!(s.contains("EXCLUSIVE"));
        assert!(s.contains("pid=42-01HQZ"));
        assert!(s.contains("4500ms"));
        assert!(s.contains("BackoffJitter"));
    }

    #[test]
    fn fenced_display_shows_epoch_delta() {
        let e = LockError::Fenced {
            resource: "entity:t1:d:rec1".into(),
            expected_epoch: 5,
            observed_epoch: 7,
            retry: default_fenced_retry(),
        };
        let s = format!("{e}");
        assert!(s.contains("expected_epoch=5"));
        assert!(s.contains("observed=7"));
    }

    #[test]
    fn db_error_preserves_source_chain() {
        use std::error::Error as _;
        let inner = tokio_rusqlite::Error::Other("boom".into());
        let e = LockError::Db(inner);
        assert!(e.source().is_some());
    }
}
