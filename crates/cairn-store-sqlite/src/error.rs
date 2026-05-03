//! Store-level error type.

use thiserror::Error;

/// Errors raised by the `SQLite` store adapter.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StoreError {
    /// Underlying `SQLite` failure.
    #[error("sqlite error")]
    Sqlite(#[from] rusqlite::Error),

    /// Migration runner failure.
    #[error("migration error")]
    Migration(#[from] rusqlite_migration::Error),

    /// Vault path is unusable (cannot create parent directory, etc.).
    #[error("vault path error: {0}")]
    VaultPath(String),

    /// On-disk schema diverged from the binary's expected manifest.
    #[error("schema drift: {0}")]
    SchemaDrift(String),

    /// `ConsentEvent` failed structural validation (kind/payload mismatch,
    /// malformed hash, etc.) before insert.
    #[error("invalid consent event: {0}")]
    InvalidConsentEvent(#[from] cairn_core::domain::ConsentEventError),

    /// `MemoryRecord` failed structural validation at the write boundary
    /// (out-of-range scalars, missing scope.user, empty body, malformed
    /// actor chain, …) before insert. Rejecting here keeps malformed
    /// records out of the row store rather than letting them persist and
    /// fail later on read-back.
    #[error("invalid record: {0}")]
    InvalidRecord(#[from] cairn_core::domain::DomainError),

    /// Record id was looked up but not present (or only present as a
    /// tombstoned row that callers must not see via `get`).
    #[error("record not found: {id}")]
    NotFound {
        /// The record id that was not found.
        id: String,
    },

    /// Method requires a capability the store does not advertise.
    /// `what` is the cap flag name (`"fts"`, `"vector"`, `"graph_edges"`,
    /// `"transactions"`).
    #[error("capability unavailable: {what}")]
    CapabilityUnavailable {
        /// The capability flag name (e.g. `"fts"`, `"vector"`,
        /// `"graph_edges"`, `"transactions"`).
        what: &'static str,
    },

    /// FTS5 query parse error. Surfaced as a separate variant so the
    /// verb layer can return user-actionable errors instead of generic
    /// SQL failures.
    #[error("FTS5 query parse error: {message}")]
    FtsQuery {
        /// Human-readable parse error message from the FTS5 engine.
        message: String,
    },

    /// Background `tokio_rusqlite` worker error (channel closed, panic
    /// in the worker thread, etc.).
    #[error("tokio_rusqlite worker error")]
    Worker(#[from] tokio_rusqlite::Error),

    /// Record JSON ↔ struct codec error.
    #[error("record codec error")]
    Codec(#[from] serde_json::Error),

    /// Invariant violation. Indicates a bug in the store, not a user
    /// error. Logged and surfaced.
    #[error("invariant violated: {what}")]
    Invariant {
        /// Description of the violated invariant.
        what: String,
    },

    /// An explicit session id was supplied (via `--session`, env, or
    /// harness) but the persisted row's `(user, agent, project_root)`
    /// does not match the caller's identity. Treat this as a hard
    /// authentication failure — the id is foreign and must not be used.
    /// Brief §8.1.
    #[error(
        "session identity mismatch for session_id `{session_id}`: \
         the persisted row belongs to a different (user, agent, project_root)"
    )]
    SessionIdentityMismatch {
        /// The session id whose ownership check failed.
        session_id: String,
    },

    /// An explicit session id was supplied but no such session exists in
    /// the store. Brief §8.1: explicit ids are authoritative — a typo or
    /// stale `CAIRN_SESSION_ID` should fail closed rather than silently
    /// fall through to auto-discover, which would split writes into a
    /// different session.
    #[error("explicit session id `{session_id}` does not exist")]
    SessionNotFound {
        /// The session id that was not found.
        session_id: String,
    },

    /// An explicit session id was supplied but the row has already been
    /// closed (`ended_at IS NOT NULL`). Brief §8.1: the caller asked
    /// specifically for this session — silently moving them to a new
    /// session would mix two conversations.
    #[error("explicit session id `{session_id}` is ended (closed at {ended_at_unix_ms}ms)")]
    SessionEnded {
        /// The session id that was found but ended.
        session_id: String,
        /// Unix epoch milliseconds when the session was ended.
        ended_at_unix_ms: i64,
    },

    /// Sustained write contention exceeded the operation's deadline.
    /// Distinct from `Sqlite(SQLITE_BUSY)` so callers can classify this as
    /// retriable on the next user action without scraping error codes.
    #[error("store busy after {elapsed_ms}ms of retries on `{operation}`")]
    Busy {
        /// The store operation that exhausted its retry deadline.
        operation: &'static str,
        /// Elapsed time spent retrying, in milliseconds.
        elapsed_ms: u64,
    },

    /// Method called on a store constructed via `Default::default()`
    /// (the registry stub) instead of [`crate::open()`] /
    /// [`crate::open_in_memory()`]. Distinct from `Invariant` so callers
    /// can detect the misuse and surface a clear "open the store first"
    /// hint.
    #[error(
        "cairn-store-sqlite: {method} called on unconnected store \
         (use cairn_store_sqlite::open(path).await first)"
    )]
    NotInitialized {
        /// The trait-method name that was invoked.
        method: &'static str,
    },

    /// Two distinct events tried to land at the same trace `sequence`.
    /// The `records_trace_seq` unique index enforces `(session_id,
    /// turn_id, sequence)` uniqueness; this variant surfaces when two
    /// different `capture_event_id`s collide on the same slot.
    #[error("trace sequence conflict at session={session_id} turn={turn_id} seq={sequence}")]
    TraceSequenceConflict {
        /// The session id of the conflicting trace record.
        session_id: String,
        /// The turn id of the conflicting trace record.
        turn_id: String,
        /// The sequence number that was already claimed.
        sequence: u64,
    },

    /// A `post_tool` or `tool_output` record has a `parent_event_id` that
    /// either doesn't resolve to a `pre_tool` in the same turn or has a
    /// mismatched `tool_call_id`.
    #[error("trace link orphan: child={child_id} parent={parent_id}: {reason}")]
    TraceLinkOrphan {
        /// The `capture_event_id` of the child record with the broken link.
        child_id: String,
        /// The `parent_event_id` referenced by the child (may be empty string
        /// if the field was missing entirely).
        parent_id: String,
        /// Human-readable description of the violated invariant.
        reason: String,
    },

    /// Two distinct record ids share the same `trace.capture_event_id`.
    /// This should not happen under the documented projector contract
    /// (Task 6 derives `record_id` deterministically from
    /// `capture_event_id`), so this variant indicates a projector
    /// contract violation.
    #[error("trace capture_event_id collision: {capture_event_id}")]
    TraceCaptureEventIdConflict {
        /// The `capture_event_id` shared by two distinct record ids.
        capture_event_id: String,
    },
}

// Note: the plan specifies an explicit
// `impl From<StoreError> for Box<dyn std::error::Error + Send + Sync + 'static>`,
// but the standard library already provides a blanket
// `impl<E: Error + Send + Sync + 'a> From<E> for Box<dyn Error + Send + Sync + 'a>`
// which would conflict (E0119). Since `StoreError: Error + Send + Sync + 'static`
// via `thiserror`, `?`-propagation into the trait alias
// `Box<dyn Error + Send + Sync + 'static>` works automatically through the
// blanket impl, satisfying the plan's intent.
