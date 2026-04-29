//! Job persistence contract for `WorkflowOrchestrator` implementations.
//!
//! Brief §10 (v0.1 row): the default `tokio` orchestrator is backed by a
//! durable `SQLite` job table providing **crash-safe resume, exponential
//! retry, single-writer queue per key, step-level idempotency via
//! `operation_id`**. This trait names that persistence surface so the
//! tokio scheduler can be tested against any backend that satisfies it
//! (in-memory for unit tests, `SQLite` for production), and so a future
//! Temporal adapter ignores it entirely (Temporal owns its own state
//! machine).
//!
//! Adapter rule (`CLAUDE.md` §6.1): no `cairn-core` -> adapter dep, so
//! the types below are pure-data — opaque payload bytes, integer epoch
//! milliseconds, newtyped IDs. Backend errors are surfaced through
//! [`JobStoreError::Backend`].

use crate::domain::Rfc3339Timestamp;

/// Stable identifier for a job row. ULID (or any string) chosen by the
/// caller of [`JobStore::enqueue`]. Identity is immutable on disk
/// (enforced by trigger in migration 0011).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JobId(pub String);

impl JobId {
    /// Wrap a raw string id.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Borrow the underlying string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Workflow discriminator — the worker name a handler registers under
/// (e.g. `"dream.light"`, `"expire.tier"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JobKind(pub String);

impl JobKind {
    /// Wrap a raw kind string.
    #[must_use]
    pub fn new(kind: impl Into<String>) -> Self {
        Self(kind.into())
    }

    /// Borrow the underlying string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for JobKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Opaque worker-supplied bytes. The `JobStore` does not interpret them.
/// Workflows are responsible for their own serialization (typically
/// `serde_json` or `bincode`).
pub type JobPayload = Vec<u8>;

/// Token identifying a successful lease. The scheduler must present the
/// same token for [`JobStore::heartbeat`], [`JobStore::complete`], and
/// [`JobStore::fail`]; an expired or stolen lease causes those calls to
/// fail with [`JobStoreError::LeaseLost`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseToken {
    /// Stable owner identity for the leasing scheduler (e.g.
    /// `"<incarnation_id>:<worker_id>"`).
    pub owner: String,
    /// Wall-clock at which the lease becomes eligible for reaping. Epoch
    /// milliseconds.
    pub expires_at_ms: i64,
}

/// Lifecycle states. Mirrors the SQL CHECK in migration 0011.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobState {
    /// Eligible for lease; `next_run_at` may be in the future.
    Queued,
    /// Held by a worker; `lease_owner` + `lease_expires_at` are set.
    Leased,
    /// Terminal success.
    Done,
    /// Terminal failure (max attempts exhausted or workflow surfaced a
    /// non-retryable error).
    Failed,
}

/// Backoff schedule applied when a workflow surfaces a retryable failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Maximum number of attempts. `attempts` exceeding this value
    /// transitions the row to [`JobState::Failed`].
    pub max_attempts: u32,
    /// Base delay before the first retry, milliseconds.
    pub base_backoff_ms: u32,
    /// Multiplier applied per attempt. `delay = base * multiplier^(attempt-1)`,
    /// capped by `max_backoff_ms`.
    pub backoff_multiplier: u32,
    /// Upper bound on backoff between retries, milliseconds.
    pub max_backoff_ms: u32,
}

impl RetryPolicy {
    /// P0 default: 5 attempts, 1s base, ×2 per try, capped at 60s.
    pub const DEFAULT: Self = Self {
        max_attempts: 5,
        base_backoff_ms: 1_000,
        backoff_multiplier: 2,
        max_backoff_ms: 60_000,
    };

    /// Compute the next-run delay for a given attempt count (1-based).
    /// Pure function — exposed so the scheduler and tests share one
    /// definition.
    #[must_use]
    pub const fn delay_for_attempt(self, attempt: u32) -> u32 {
        if attempt == 0 {
            return 0;
        }
        // Saturating-multiply to keep this `const`.
        let mut delay = self.base_backoff_ms;
        let mut i = 1;
        while i < attempt {
            delay = delay.saturating_mul(self.backoff_multiplier);
            if delay >= self.max_backoff_ms {
                return self.max_backoff_ms;
            }
            i += 1;
        }
        delay
    }
}

/// Insertion request to [`JobStore::enqueue`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnqueueRequest {
    /// Caller-chosen ULID. Must be unique across all jobs.
    pub job_id: JobId,
    /// Workflow discriminator.
    pub kind: JobKind,
    /// Opaque payload bytes; `JobStore` stores them verbatim.
    pub payload: JobPayload,
    /// Optional single-writer queue identifier. At most one job per
    /// `queue_key` may be `Queued | Leased` at a time (enforced by
    /// `workflow_jobs_queue_key_active_uniq`).
    pub queue_key: Option<String>,
    /// Optional step-level idempotency key (e.g. WAL `operation_id`).
    /// Combined with `kind` it forms a unique constraint that lets the
    /// scheduler call `enqueue` from a retry-prone path.
    pub dedupe_key: Option<String>,
    /// Earliest time the job becomes lease-eligible. Epoch milliseconds.
    pub not_before_ms: i64,
    /// Retry policy applied if [`JobStore::fail`] requests a retry.
    pub retry: RetryPolicy,
}

/// One leased job handed to a worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeasedJob {
    /// The job identifier.
    pub job_id: JobId,
    /// Workflow kind.
    pub kind: JobKind,
    /// Opaque payload bytes.
    pub payload: JobPayload,
    /// Number of attempts including the current one (≥1 once leased).
    pub attempts: u32,
    /// Retry policy persisted with the job.
    pub retry: RetryPolicy,
    /// Active lease token; the scheduler must present this on
    /// heartbeat / complete / fail.
    pub lease: LeaseToken,
}

/// Disposition for [`JobStore::fail`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailDisposition {
    /// Bump `attempts`, schedule per `RetryPolicy`. If the new attempts
    /// would exceed `max_attempts`, the row terminates as `Failed`.
    Retry,
    /// Force terminal `Failed` regardless of remaining attempts.
    Permanent,
}

/// Errors raised by [`JobStore`] implementations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum JobStoreError {
    /// Caller's lease no longer matches the row (expired / reaped /
    /// stolen). The worker should drop its result and exit cleanly.
    #[error("lease lost for job {job_id}")]
    LeaseLost {
        /// Job whose lease no longer matched.
        job_id: JobId,
    },
    /// `enqueue` rejected because `(kind, dedupe_key)` already exists.
    #[error("duplicate dedupe_key {dedupe_key:?} for kind {kind}")]
    DuplicateDedupeKey {
        /// Workflow kind.
        kind: JobKind,
        /// The dedupe key that conflicted.
        dedupe_key: String,
    },
    /// `enqueue` rejected because `queue_key` already has an active row.
    #[error("queue_key {queue_key:?} already has an active job")]
    QueueKeyBusy {
        /// Queue key that conflicted.
        queue_key: String,
    },
    /// Backend-specific error (`SQLite` I/O, lock contention, etc.).
    #[error("job store backend: {0}")]
    Backend(String),
}

/// Persistence contract for the `tokio` workflow orchestrator's job table.
///
/// Implementors must be `Send + Sync` and safe to call from multiple
/// concurrent worker tasks. The `lease` operation must be atomic: at most
/// one caller observes `Some(_)` for any given queued row.
#[async_trait::async_trait]
pub trait JobStore: Send + Sync {
    /// Insert a new job. Idempotent on `(kind, dedupe_key)` when
    /// `dedupe_key` is `Some` — duplicates surface as
    /// [`JobStoreError::DuplicateDedupeKey`].
    ///
    /// # Errors
    ///
    /// Returns [`JobStoreError`] on duplicate keys, conflicting
    /// `queue_key`, or backend failure.
    async fn enqueue(&self, req: EnqueueRequest) -> Result<(), JobStoreError>;

    /// Atomically lease the next eligible job. Returns `Ok(None)` when no
    /// queued row has `next_run_at <= now_ms`. The lease expires at
    /// `now_ms + lease_duration_ms`; the scheduler must heartbeat or
    /// finish before then.
    ///
    /// # Errors
    ///
    /// Returns [`JobStoreError::Backend`] for backend failures.
    async fn lease(
        &self,
        owner: &str,
        now_ms: i64,
        lease_duration_ms: i64,
    ) -> Result<Option<LeasedJob>, JobStoreError>;

    /// Extend the lease deadline of a currently-held job.
    ///
    /// # Errors
    ///
    /// [`JobStoreError::LeaseLost`] if the row no longer matches the
    /// presented lease (reaped, completed, or owned by someone else).
    async fn heartbeat(
        &self,
        job_id: &JobId,
        lease: &LeaseToken,
        new_expires_at_ms: i64,
    ) -> Result<(), JobStoreError>;

    /// Mark the job as terminally `Done`.
    ///
    /// # Errors
    ///
    /// [`JobStoreError::LeaseLost`] if the lease no longer matches.
    async fn complete(&self, job_id: &JobId, lease: &LeaseToken) -> Result<(), JobStoreError>;

    /// Record a failure. With [`FailDisposition::Retry`] the row goes
    /// back to `Queued` (or terminates if `attempts == max_attempts`);
    /// [`FailDisposition::Permanent`] forces terminal `Failed`.
    ///
    /// # Errors
    ///
    /// [`JobStoreError::LeaseLost`] if the lease no longer matches.
    async fn fail(
        &self,
        job_id: &JobId,
        lease: &LeaseToken,
        disposition: FailDisposition,
        last_error: &str,
        now_ms: i64,
    ) -> Result<(), JobStoreError>;

    /// Reclaim leased rows whose `lease_expires_at <= now_ms`. Used by
    /// the scheduler's reaper loop and on startup to recover orphans
    /// from a prior incarnation. Returns the number of rows reclaimed.
    ///
    /// # Errors
    ///
    /// Backend failure only.
    async fn reap_expired(&self, now_ms: i64) -> Result<usize, JobStoreError>;
}

// `Rfc3339Timestamp` is used elsewhere in the contract surface; bring it
// into scope so the doc-link in module docs resolves once any future
// change starts emitting timestamped events.
#[allow(dead_code)]
fn _ts_doc_anchor(_: Rfc3339Timestamp) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_policy_default_grows_geometrically_until_cap() {
        let p = RetryPolicy::DEFAULT;
        assert_eq!(p.delay_for_attempt(0), 0);
        assert_eq!(p.delay_for_attempt(1), 1_000);
        assert_eq!(p.delay_for_attempt(2), 2_000);
        assert_eq!(p.delay_for_attempt(3), 4_000);
        assert_eq!(p.delay_for_attempt(4), 8_000);
        assert_eq!(p.delay_for_attempt(5), 16_000);
        assert_eq!(p.delay_for_attempt(6), 32_000);
        assert_eq!(p.delay_for_attempt(7), 60_000); // capped
        assert_eq!(p.delay_for_attempt(50), 60_000);
    }

    #[test]
    fn job_id_round_trip_string() {
        let id = JobId::new("01HQZ123");
        assert_eq!(id.as_str(), "01HQZ123");
        assert_eq!(format!("{id}"), "01HQZ123");
    }
}
