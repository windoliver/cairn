//! `SqliteMemoryStore` impl modules.

pub(crate) mod edges;
pub(crate) mod expire;
pub mod forget;
pub(crate) mod graph_search;
pub(crate) mod hybrid;
pub(crate) mod projection;
pub(crate) mod projection_ledger;
pub(crate) mod read;
pub(crate) mod reindex;
pub(crate) mod reindex_from_db;
pub(crate) mod salience;
pub(crate) mod scope_predicate;
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
    /// Per-process daemon incarnation id, minted by `locks::init_incarnation`
    /// during `open` (issue #56, brief §5.6). Threaded into `locks::acquire`
    /// so freshly-acquired lock rows carry the current process's ULID.
    /// `None` for the registry stub built via `Default::default` (no
    /// connection, so no incarnation row exists).
    pub(crate) incarnation: Option<Arc<str>>,
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
            incarnation: None,
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
    /// Read-path connection accessor. Clones the underlying
    /// `Arc<AsyncConn>` and maps the absent-store case to
    /// `StoreError`. Returning the cloned `Arc` (not a reference)
    /// lets `GraphQueries` move the handle into the
    /// `tokio_rusqlite::Connection::call(move |c| { … })` closure
    /// without borrowing `&self`.
    pub fn read_conn(&self) -> Result<Arc<AsyncConn>, StoreError> {
        self.require_conn("read_conn").map(Arc::clone)
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
    /// Blocking open helper retained for integration tests that drive the
    /// async store from synchronous `Command`-based fixtures.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the async open path fails or the helper
    /// runtime cannot be created.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn open(path: &std::path::Path) -> Result<Self, StoreError> {
        let path = path.to_path_buf();
        block_on_store(async move { crate::open(path).await })
    }

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

    /// Returns the daemon incarnation id used by `locks::acquire`.
    /// `None` for the registry stub (unconnected) Store.
    #[must_use]
    pub fn incarnation(&self) -> Option<&Arc<str>> {
        self.incarnation.as_ref()
    }

    /// Insert or replace a deterministic active record for projection tests.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the record id is invalid, the store cannot
    /// be opened, or SQLite rejects the fixture row.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn insert_test_record(
        &self,
        record_id: &str,
        body: &str,
        wal_sequence: u64,
        record_hash: &str,
    ) -> Result<(), StoreError> {
        self.insert_test_record_inner(record_id, body, wal_sequence, record_hash, None)
    }

    /// Insert or replace a deterministic active record with source metadata
    /// for projection parser tests.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the record id is invalid, the store cannot
    /// be opened, or SQLite rejects the fixture row.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn insert_test_record_with_source(
        &self,
        record_id: &str,
        body: &str,
        wal_sequence: u64,
        record_hash: &str,
        source_path: &str,
        source_hash: &str,
    ) -> Result<(), StoreError> {
        self.insert_test_record_inner(
            record_id,
            body,
            wal_sequence,
            record_hash,
            Some((source_path, source_hash)),
        )
    }

    #[cfg(any(test, feature = "test-helpers"))]
    fn insert_test_record_inner(
        &self,
        record_id: &str,
        body: &str,
        wal_sequence: u64,
        record_hash: &str,
        source: Option<(&str, &str)>,
    ) -> Result<(), StoreError> {
        use cairn_core::domain::{RecordId, SourceRef, TargetId};

        let id = RecordId::parse(record_id.to_owned()).map_err(|err| StoreError::Invariant {
            what: format!("invalid test record id `{record_id}`: {err}"),
        })?;
        let target =
            TargetId::parse(record_id.to_owned()).map_err(|err| StoreError::Invariant {
                what: format!("invalid test target id `{record_id}`: {err}"),
            })?;
        let mut upsert_record = cairn_core::domain::record::tests_export::sample_record();
        upsert_record.id = id.clone();
        upsert_record.target_id = target;
        upsert_record.body = body.to_owned();

        let mut stored_record = upsert_record.clone();
        if let Some((source_path, source_hash)) = source {
            stored_record.provenance.source_hash = source_hash.to_owned();
            stored_record.provenance.source_refs = vec![SourceRef {
                id: source_path.to_owned(),
                hash: source_hash.to_owned(),
            }];
        }
        let record_json = serde_json::to_string(&stored_record)?;
        let store = self.clone();
        let record_id = id.as_str().to_owned();
        let body = body.to_owned();
        let record_hash = record_hash.to_owned();

        block_on_store(async move {
            if store.do_get(&id).await?.is_none() {
                let _ = store.do_upsert(&upsert_record).await?;
            }
            let conn = store.require_conn("insert_test_record")?.clone();
            conn.call(move |c| {
                let wal_sequence = i64::try_from(wal_sequence).map_err(|_| {
                    tokio_rusqlite::Error::Other(Box::new(StoreError::Invariant {
                        what: "test wal_sequence overflow".to_owned(),
                    }))
                })?;
                let changed = c.execute(
                    "UPDATE records
                     SET version = ?2,
                         body = ?3,
                         body_hash = ?4,
                         record_json = ?5,
                         active = 1,
                         tombstoned = 0,
                         cow_staged = 0
                     WHERE record_id = ?1",
                    rusqlite::params![record_id, wal_sequence, body, record_hash, record_json],
                )?;
                if changed == 0 {
                    return Err(tokio_rusqlite::Error::Other(Box::new(
                        StoreError::Invariant {
                            what: "test record upsert did not create a row".to_owned(),
                        },
                    )));
                }
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await?;
            Ok(())
        })
    }
}

#[cfg(any(test, feature = "test-helpers"))]
fn block_on_store<T, F>(future: F) -> Result<T, StoreError>
where
    T: Send + 'static,
    F: std::future::Future<Output = Result<T, StoreError>> + Send + 'static,
{
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| StoreError::Invariant {
                what: format!("failed to build test runtime: {err}"),
            })?;
        runtime.block_on(future)
    })
    .join()
    .map_err(|_| StoreError::Invariant {
        what: "test runtime thread panicked".to_owned(),
    })?
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
