//! Bitemporal knowledge-graph storage layer (issue #186).
//!
//! All entity-graph mutations write through the generic `wal_ops` +
//! `wal_steps` schema (migration 0002, widened in 0031) using the helpers
//! in [`wal`]. Spec: `docs/superpowers/specs/2026-05-02-issue-186-bitemporal-kg-schema-design.md`.

/// Read-only graph-traversal query helpers (issue #190).
pub mod queries;
pub mod wal;

mod edge;
mod episode;
mod node;
mod query;
mod resolve;

use crate::error::StoreError;

/// SQL fragment that serializes an `entity_edges` row as a JSON object,
/// suitable for use as a WAL `pre_image` blob. Embed inline in SELECT
/// lists where a row is being captured for compensation/audit before
/// being mutated. `body_hash` is hex-encoded (BLOB → string) so the
/// payload survives JSON round-tripping by callers that don't tolerate
/// raw bytes. The schema is stable across the substrate; bumping it is
/// a brief-level change.
pub(crate) const ENTITY_EDGE_PRE_IMAGE_JSON: &str = "json_object(\
    'id', id, \
    'source_id', source_id, \
    'target_id', target_id, \
    'relation', relation, \
    'confidence', confidence, \
    'confidence_score', confidence_score, \
    'valid_at', valid_at, \
    'invalid_at', invalid_at, \
    'created_at', created_at, \
    'expired_at', expired_at, \
    'tombstone_reason', tombstone_reason, \
    'source_record_id', source_record_id, \
    'body_hash_hex', hex(body_hash))";

/// Translate a worker-side error into a typed [`StoreError`], preserving
/// any [`StoreError`] previously wrapped via `tokio_rusqlite::Error::Other`.
/// Without this, the blanket `From<tokio_rusqlite::Error>` collapses every
/// inner error into [`StoreError::Worker`], hiding domain variants like
/// [`StoreError::NotFound`] / [`StoreError::Invariant`] that the worker
/// closure deliberately wrapped. Mirrors the idiom in `store/search.rs`.
pub(super) fn unpack_worker_err(err: tokio_rusqlite::Error) -> StoreError {
    match err {
        tokio_rusqlite::Error::Other(boxed) => match boxed.downcast::<StoreError>() {
            Ok(inner) => *inner,
            Err(other) => StoreError::Worker(tokio_rusqlite::Error::Other(other)),
        },
        other => StoreError::from(other),
    }
}
