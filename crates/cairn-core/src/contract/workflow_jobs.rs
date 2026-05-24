//! `WorkflowJobsReader` — read-only view of `workflow_jobs` for lint.
//!
//! Issue #92. Spec §4.8.
//!
//! Sync trait — the `lint` verb is sync today. The `SQLite` adapter
//! implements it with small read-only queries against `workflow_jobs`;
//! the `workflow_health` lint check (§4.10) consumes the surface.
//!
//! Issue #123: `failed_federation_jobs` surfaces `state = 'Failed'`
//! rows whose `kind` starts with the supplied prefix (e.g.
//! `"federation.propagate."`). Used by the
//! `federation_dead_propagation` lint check.

use crate::contract::job_store::{FailureClass, JobId, JobKind};

/// One `state = 'Failed'` federation propagation job row surfaced to
/// the `federation_dead_propagation` lint check (issue #123).
///
/// Dead-propagation rows differ from the main dead-letter queue in
/// that they may have been terminal-failed by a permanent error
/// (e.g. rejected by the remote hub) rather than by exhausting retry
/// attempts. The lint check surfaces them regardless of
/// `dead_letter_at_ms` — a `Failed` federation job is always
/// actionable because propagation is idempotent and a manual retry is
/// always safe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedFederationJob {
    /// The job identifier.
    pub job_id: JobId,
    /// Workflow kind starting with `"federation.propagate."`.
    pub kind: JobKind,
    /// Number of attempts when the row reached `Failed`.
    pub attempts: u32,
    /// Last error string persisted on the row by `JobStore::fail`.
    /// Included verbatim in the finding message so operators can
    /// diagnose the root cause without inspecting the DB directly.
    ///
    /// Privacy: `last_error` is set by the propagation worker itself
    /// and MUST NOT contain record bodies. Workers must only include
    /// the remote hub's error code/message and local metadata
    /// (`operation_id`, `kind`) in this field.
    pub last_error: String,
    /// Wall-clock (epoch ms) of the final `fail` call that set
    /// `state = 'Failed'`. `None` when the adapter cannot determine
    /// the exact timestamp (e.g. pre-migration rows).
    pub failed_at_ms: Option<i64>,
}

/// One dead-letter row surfaced to lint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadLetterRow {
    /// The job identifier.
    pub job_id: JobId,
    /// Workflow kind (e.g. `dream.light`).
    pub kind: JobKind,
    /// Number of attempts when the row terminated.
    pub attempts: u32,
    /// Failure class persisted on the row by `JobStore::fail` (§4.5).
    pub failure_class: FailureClass,
    /// Last error string persisted on the row.
    pub last_error: String,
    /// Wall-clock (epoch ms) the row was dead-lettered.
    pub dead_letter_at_ms: i64,
}

/// Read-only adapter for the lint `workflow_health` check.
///
/// Implementations are `Send + Sync` so a single shared reader can be
/// passed to the synchronous lint pipeline from any caller.
///
/// The trait methods stay infallible so call sites in `workflow_health`
/// remain cheap. Adapters that detect a runtime failure (locked DB,
/// poisoned mutex, post-construction schema drift) MUST record the
/// reason internally and surface it through [`Self::take_last_error`]
/// so lint can emit a `DeferredCheck` Info finding instead of silently
/// reporting "no workflow issues" — issue #92 round-7 finding 7.2.
pub trait WorkflowJobsReader: Send + Sync {
    /// Count of `state = 'failed'` rows whose `dead_letter_at_ms IS NOT NULL`.
    /// `Some(kind)` filters; `None` counts across all kinds.
    fn dead_letter_count(&self, kind: Option<&JobKind>) -> usize;

    /// `now_ms - next_run_at` for the oldest queued row matching `kind`.
    /// Returns `None` when no queued row exists.
    fn oldest_queued_age_ms(&self, kind: Option<&JobKind>, now_ms: i64) -> Option<i64>;

    /// `now_ms - lease_expires_at` for the leased row whose lease is held
    /// the longest (or whose lease is most expired). Returns `None` when
    /// no leased rows exist.
    fn longest_held_lease_ms(&self, now_ms: i64) -> Option<i64>;

    /// `completed_at_ms` of the most recent `state = 'done'` row for `kind`.
    /// Returns `None` when no row has completed.
    fn last_success_ms(&self, kind: &JobKind) -> Option<i64>;

    /// Up to `limit` dead-letter rows ordered by `dead_letter_at_ms` desc.
    fn dead_letter_rows(&self, limit: usize) -> Vec<DeadLetterRow>;

    /// All `state = 'Failed'` rows whose `kind` starts with `kind_prefix`
    /// (e.g. `"federation.propagate."`), ordered by `failed_at_ms` desc
    /// (or insertion order when the timestamp is unavailable).
    ///
    /// The query is the SQL equivalent of:
    ///
    /// ```sql
    /// SELECT job_id, kind, attempts, last_error, failed_at_ms
    ///   FROM workflow_jobs
    ///  WHERE kind LIKE '<prefix>%'
    ///    AND state = 'Failed'
    ///  ORDER BY failed_at_ms DESC NULLS LAST;
    /// ```
    ///
    /// Default implementation returns an empty `Vec` so adapters that
    /// pre-date issue #123 stay compilable without change.
    fn failed_federation_jobs(&self, kind_prefix: &str) -> Vec<FailedFederationJob> {
        let _ = kind_prefix;
        Vec::new()
    }

    /// Drain the most recent runtime failure observed by any of the
    /// other trait methods. `None` means every method that ran since
    /// the last drain succeeded (or returned a legitimate empty
    /// result). `Some(reason)` means at least one method swallowed a
    /// backend error — lint MUST surface this as a `DeferredCheck`
    /// Info finding so operators do not mistake "queries failed" for
    /// "everything is healthy".
    ///
    /// Default implementation returns `None` for adapters that cannot
    /// fail (e.g. in-memory test fakes). Real adapters override.
    fn take_last_error(&self) -> Option<String> {
        None
    }
}
