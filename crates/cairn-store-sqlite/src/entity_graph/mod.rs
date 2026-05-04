//! Bitemporal knowledge-graph storage layer (issue #186).
//!
//! All entity-graph mutations write through the generic `wal_ops` +
//! `wal_steps` schema (migration 0002, widened in 0031) using the helpers
//! in [`wal`]. Spec: `docs/superpowers/specs/2026-05-02-issue-186-bitemporal-kg-schema-design.md`.

pub mod wal;

mod edge;
mod episode;
mod node;
mod query;
mod resolve;

use crate::error::StoreError;

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
