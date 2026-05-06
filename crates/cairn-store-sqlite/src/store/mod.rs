//! `SqliteMemoryStore` impl modules.

pub(crate) mod edges;
pub(crate) mod hybrid;
pub(crate) mod projection;
pub(crate) mod read;
pub(crate) mod reindex;
pub(crate) mod reindex_from_db;
pub(crate) mod search;
pub mod sessions;
pub(crate) mod tombstone;
pub(crate) mod trait_impl;
pub(crate) mod tx;
pub(crate) mod upsert;

use std::sync::Arc;

use cairn_core::contract::memory_store::MemoryStoreCapabilities;
use cairn_embeddings_local::EmbeddingModel;
use tokio_rusqlite::Connection as AsyncConn;
use tokio_util::sync::CancellationToken;

use crate::error::StoreError;

/// Async-fronted `SQLite` memory store.
///
/// Two construction paths:
///
/// - [`SqliteMemoryStore::default`] — unconnected registry stub used by
///   the `register_plugin!` macro for identity/capability advertisement.
///   Trait verb methods return a "not initialized" error.
/// - [`crate::open()`] / [`crate::open_in_memory()`] / [`crate::open_with_embedder()`] —
///   connected store with pragmas + migrations applied. Trait verb methods
///   route through the wrapped `tokio_rusqlite::Connection` on a dedicated
///   DB thread. When an embedder is supplied, the `vector` capability is
///   enabled and a background drain loop is spawned.
///
/// Construction is side-effect free per brief §4.1; the `open` path is
/// the only side-effecting one.
#[derive(Clone)]
pub struct SqliteMemoryStore {
    pub(crate) conn: Option<Arc<AsyncConn>>,
    /// Optional local embedding model. Presence enables `caps.vector`.
    /// Used by `do_search_semantic` (Task 7) and embed-on-write in `do_upsert` (Task 8).
    pub(crate) embedder: Option<Arc<dyn EmbeddingModel>>,
    /// Dynamic capability advertisement. Set at open time based on whether
    /// an embedder was supplied.
    pub(crate) caps: MemoryStoreCapabilities,
    /// Cancellation token for the background reindex drain loop. Dropped when
    /// the store is dropped, signalling the drain task to exit gracefully.
    pub(crate) _cancel: Option<CancellationToken>,
    /// Per-column BM25 weights for `records_fts` `(kind, class, scope, body)`.
    /// Threaded into the `bm25(records_fts, w0, w1, w2, w3)` SQL call inside
    /// `do_search_keyword`. Defaults to `[10.0, 10.0, 5.0, 1.0]` — kind/class
    /// dominate, scope is mid-tier, body anchors the long-prose match.
    /// Set at open time via `open_with_embedder_and_config`; the registry
    /// stub built by `Default::default` carries the defaults.
    pub(crate) fts_column_weights: [f64; 4],
}

impl Default for SqliteMemoryStore {
    fn default() -> Self {
        Self {
            conn: None,
            embedder: None,
            caps: MemoryStoreCapabilities {
                fts: true,
                vector: false,
                graph_edges: true,
                transactions: true,
                per_record_consent_model: true,

                graph_search: false,
            },
            _cancel: None,
            fts_column_weights: [10.0, 10.0, 5.0, 1.0],
        }
    }
}

impl SqliteMemoryStore {
    /// Borrow the underlying `tokio_rusqlite` handle, returning a typed
    /// `not initialized` error when the store was constructed via
    /// [`Default::default`] (registry stub).
    ///
    /// The trait-level `MemoryStore` impl performs an early `is_none` guard
    /// before dispatching into the per-method `do_*` inherent methods; those
    /// inherent methods route through this helper so the guard message stays
    /// in one place and shared between the trait surface and any internal
    /// caller that might bypass the trait.
    pub(crate) fn require_conn(&self, method: &'static str) -> Result<&Arc<AsyncConn>, StoreError> {
        self.conn
            .as_ref()
            .ok_or(StoreError::NotInitialized { method })
    }
}

impl SqliteMemoryStore {
    /// Expose the underlying async connection for integration tests that need
    /// to insert raw rows (e.g., to seed `record_vectors` before Task 8 lands
    /// embed-on-write). Gated behind `test-helpers` so this surface never
    /// appears in production builds.
    #[cfg(any(test, feature = "test-helpers"))]
    #[must_use]
    pub fn raw_conn(&self) -> Option<&Arc<AsyncConn>> {
        self.conn.as_ref()
    }

    /// Borrow the underlying async connection for admin verbs (e.g.
    /// `cairn admin reindex`) that need to call `drain_once` directly.
    ///
    /// Returns `None` when the store was constructed via `Default::default`
    /// (registry stub — no connection). Production callers should treat
    /// `None` as an internal error.
    #[must_use]
    pub fn raw_conn_for_admin(&self) -> Option<&Arc<AsyncConn>> {
        self.conn.as_ref()
    }
}

impl std::fmt::Debug for SqliteMemoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteMemoryStore")
            .field("connected", &self.conn.is_some())
            .finish_non_exhaustive()
    }
}

/// Wall-clock epoch-milliseconds, shared by every mutation path that needs
/// to stamp a row's `created_at` / `updated_at`.
///
/// `SystemTime::now() < UNIX_EPOCH` cannot happen on real hardware (the
/// platform clock would have to predate 1970); we still return `0` rather
/// than panic so a misconfigured VM clock cannot crash the store.
/// Saturating the millis cast at `i64::MAX` is a similar belt-and-braces
/// guard for the y292000 problem.
pub(crate) fn current_unix_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}
