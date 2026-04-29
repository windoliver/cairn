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
}

// Note: the plan specifies an explicit
// `impl From<StoreError> for Box<dyn std::error::Error + Send + Sync + 'static>`,
// but the standard library already provides a blanket
// `impl<E: Error + Send + Sync + 'a> From<E> for Box<dyn Error + Send + Sync + 'a>`
// which would conflict (E0119). Since `StoreError: Error + Send + Sync + 'static`
// via `thiserror`, `?`-propagation into the trait alias
// `Box<dyn Error + Send + Sync + 'static>` works automatically through the
// blanket impl, satisfying the plan's intent.
