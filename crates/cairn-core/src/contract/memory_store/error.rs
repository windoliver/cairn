//! Abstract `MemoryStore` errors.
//!
//! Adapter-specific errors wrap their concrete backend error type in
//! [`StoreError::Backend`]. `Conflict` variants are surfaced as the typed
//! variant rather than `Backend` so callers can pattern-match without
//! depending on the adapter crate.

use super::types::TargetId;
use thiserror::Error;

/// Discriminant for `StoreError::Conflict`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConflictKind {
    /// Caller staged a `(target_id, version)` that already exists (idempotency
    /// key violation on `stage_version`).
    VersionAlreadyStaged,
    /// `activate_version`'s `expected_prior` did not match the current active
    /// version, or the requested version is not strictly newer.
    ActivationRaced,
    /// Generic `SQLite` `UNIQUE` constraint violation.
    UniqueViolation,
    /// Generic `SQLite` foreign-key constraint violation.
    ForeignKey,
    /// `purge_target` was re-invoked with an `op_id` that already wrote a
    /// marker (reserved; the normal idempotent re-purge returns
    /// `PurgeOutcome::AlreadyPurged`, not this error).
    PurgeRaced,
}

/// Abstract error type for `MemoryStore` and `MemoryStoreApplyTx` methods.
///
/// Does not depend on any adapter crate. Adapters map their concrete errors
/// to this type via `From` implementations, using `Backend` for errors that
/// don't map to a more specific variant.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StoreError {
    /// No active record with the given `target_id`.
    #[error("record not found: target_id={0}")]
    NotFound(TargetId),
    /// A write-conflict condition (see [`ConflictKind`]).
    #[error("conflict: {kind:?}")]
    Conflict {
        /// Which kind of conflict occurred.
        kind: ConflictKind,
    },
    /// A runtime invariant was violated. The message describes what should
    /// have been true.
    #[error("invariant violated: {0}")]
    Invariant(&'static str),
    /// An opaque backend error (e.g. a rusqlite I/O error).
    #[error("backend error")]
    Backend(
        /// Source error from the adapter backend.
        #[source]
        Box<dyn std::error::Error + Send + Sync>,
    ),
    /// A JSON serialization/deserialization error.
    #[error("serialization: {0}")]
    Serde(#[from] serde_json::Error),
}
