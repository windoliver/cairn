//! Transactional closure surface: [`SqliteMemoryStore::with_tx`] +
//! [`StoreTx`].
//!
//! Verb layer code that needs atomic multi-statement writes (consolidate,
//! promote, supersede + edge fan-out) calls `store.with_tx(|tx| { … })`
//! instead of stitching together separate async trait calls. The closure
//! runs synchronously on the dedicated `tokio_rusqlite` worker thread
//! inside a single `rusqlite::Transaction`. Returning `Ok` commits;
//! returning `Err` drops the tx without committing → `SQLite` rolls back.
//!
//! `StoreTx` exposes the *write* verbs (`upsert`, `tombstone`, `put_edge`,
//! `remove_edge`). Read verbs are not exposed here in PR-A — read paths
//! that need a snapshot can call the async trait methods, or open their
//! own transaction in a future PR.
//!
//! `with_tx` is **inherent**, not on the [`MemoryStore`] trait, because
//! the trait must stay object-safe (`dyn MemoryStore`). Generic methods
//! break dyn-compatibility. The verb layer reaches into the concrete
//! `SqliteMemoryStore` for transactional work.
//!
//! [`MemoryStore`]: cairn_core::contract::memory_store::MemoryStore

use cairn_core::contract::memory_store::{Edge, EdgeKey, TombstoneReason, UpsertOutcome};
use cairn_core::domain::{MemoryRecord, RecordId};
use rusqlite::{Transaction, params};
use tracing::instrument;

use crate::error::StoreError;
use crate::store::upsert::upsert_in_tx;
use crate::store::{SqliteMemoryStore, current_unix_ms};

/// Transactional handle exposed to closures passed to
/// [`SqliteMemoryStore::with_tx`]. Methods are synchronous because the
/// closure already runs on the DB worker thread; awaiting from inside
/// would deadlock the worker.
pub struct StoreTx<'a> {
    pub(crate) tx: Transaction<'a>,
}

impl StoreTx<'_> {
    /// Synchronous upsert against the open transaction. Delegates to the
    /// same internal `upsert_in_tx` helper that
    /// `SqliteMemoryStore::do_upsert` uses, so this and the async trait
    /// method follow identical idempotency, version-bump, and `record_id`
    /// synthesis rules.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] for SQL failures, or
    /// [`StoreError::Invariant`] if a stored row violates a structural
    /// invariant (corrupt `body_hash`, version overflow, etc.).
    pub fn upsert(&mut self, record: &MemoryRecord) -> Result<UpsertOutcome, StoreError> {
        upsert_in_tx(&mut self.tx, record)
    }

    /// Synchronous tombstone. Marks one specific `record_id` row as
    /// tombstoned with the given reason. Idempotent: re-tombstoning the
    /// same row writes the same flag without producing a new row.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] for SQL failures.
    pub fn tombstone(&self, id: &RecordId, reason: TombstoneReason) -> Result<(), StoreError> {
        let now_ms = current_unix_ms();
        self.tx.execute(
            "UPDATE records \
                SET tombstoned = 1, tombstone_reason = ?1, updated_at = ?2 \
              WHERE record_id = ?3",
            params![reason.as_db_str(), now_ms, id.as_str()],
        )?;
        Ok(())
    }

    /// Synchronous edge upsert. `INSERT OR REPLACE` keyed on
    /// `(src, dst, kind)` — re-putting updates only the `weight`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] for SQL failures, including
    /// `updates`-edge schema-trigger violations.
    pub fn put_edge(&self, edge: &Edge) -> Result<(), StoreError> {
        self.tx.execute(
            "INSERT OR REPLACE INTO edges (src, dst, kind, weight) \
               VALUES (?1, ?2, ?3, ?4)",
            params![
                edge.src.as_str(),
                edge.dst.as_str(),
                edge.kind.as_db_str(),
                edge.weight.map(f64::from),
            ],
        )?;
        Ok(())
    }

    /// Synchronous edge removal. Returns `true` if a row was deleted,
    /// `false` if no row matched the key.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] for SQL failures.
    pub fn remove_edge(&self, key: &EdgeKey) -> Result<bool, StoreError> {
        let n = self.tx.execute(
            "DELETE FROM edges WHERE src = ?1 AND dst = ?2 AND kind = ?3",
            params![key.src.as_str(), key.dst.as_str(), key.kind.as_db_str()],
        )?;
        Ok(n > 0)
    }
}

impl SqliteMemoryStore {
    /// Run `f` inside a single `SQLite` transaction on the dedicated DB
    /// worker thread. `Ok` commits; `Err` rolls back (the tx is dropped
    /// without committing, which `SQLite` treats as ROLLBACK).
    ///
    /// `with_tx` is inherent (not on the [`MemoryStore`] trait) because
    /// the trait must stay `dyn`-compatible — generic methods break
    /// object safety.
    ///
    /// [`MemoryStore`]: cairn_core::contract::memory_store::MemoryStore
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotInitialized`] if the store was constructed
    /// via `Default::default()`. Returns [`StoreError::Worker`] if the
    /// background worker channel fails. Otherwise returns the closure's
    /// own `Err`.
    #[allow(
        clippy::let_and_return,
        reason = "explicit binding ensures `conn` Arc drops before .await? \
                  yields, satisfying Rust 2024 tail-expr-drop-order"
    )]
    #[instrument(skip(self, f), err, fields(verb = "with_tx"))]
    pub async fn with_tx<F, T>(&self, f: F) -> Result<T, StoreError>
    where
        F: FnOnce(&mut StoreTx<'_>) -> Result<T, StoreError> + Send + 'static,
        T: Send + 'static,
    {
        let conn = self.require_conn("with_tx")?.clone();
        let result = conn
            .call(move |c| {
                let tx = c.transaction()?;
                let mut handle = StoreTx { tx };
                match f(&mut handle) {
                    Ok(value) => {
                        handle
                            .tx
                            .commit()
                            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                        Ok::<Result<T, StoreError>, tokio_rusqlite::Error>(Ok(value))
                    }
                    Err(e) => {
                        // Drop without commit → SQLite rolls back.
                        Ok::<Result<T, StoreError>, tokio_rusqlite::Error>(Err(e))
                    }
                }
            })
            .await?;
        result
    }
}
