//! `MemoryStore::{put_edge, remove_edge, neighbours}` impls.
//!
//! Edges are persisted in the `edges` table keyed by `(src, dst, kind)`.
//! `put_edge` is idempotent on that triple via `INSERT OR REPLACE`; the
//! caller's optional `weight` is the only field a re-put updates.
//!
//! `updates`-edge invariants (distinct `target_id`s, non-tombstoned
//! endpoints, post-insert immutability of identity columns) are enforced
//! by the schema triggers in migration `0001_records`. The store surfaces
//! their `RAISE(ABORT, ...)` messages as [`StoreError::Sqlite`] without
//! parsing them.
//!
//! `neighbours` filters endpoints through the `records_latest` view so
//! superseded or tombstoned rows never escape the graph traversal — even
//! if the underlying `edges` row still exists.

use cairn_core::contract::memory_store::{Edge, EdgeDir, EdgeKey, EdgeKind};
use cairn_core::domain::RecordId;
use rusqlite::params;
use tracing::instrument;

use crate::error::StoreError;
use crate::store::SqliteMemoryStore;
use crate::store::projection::record_id_from_str;

impl SqliteMemoryStore {
    /// Inherent `put_edge` implementation; the trait method
    /// [`MemoryStore::put_edge`] guards `self.conn` then delegates here.
    ///
    /// `INSERT OR REPLACE` makes the call idempotent on `(src, dst, kind)`:
    /// re-putting the same edge updates `weight` only. The schema triggers
    /// `edges_updates_supersede_insert` (BEFORE INSERT) and
    /// `edges_updates_immutable_after_insert` (BEFORE UPDATE) enforce the
    /// `updates`-edge invariants; their `RAISE(ABORT, ...)` surfaces here
    /// as [`StoreError::Sqlite`].
    ///
    /// [`MemoryStore::put_edge`]: cairn_core::contract::memory_store::MemoryStore::put_edge
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Worker`] when the background `tokio_rusqlite`
    /// worker fails and [`StoreError::Sqlite`] for SQL errors surfaced
    /// through the worker (including trigger violations).
    #[instrument(
        skip(self),
        err,
        fields(
            verb = "put_edge",
            src = %edge.src.as_str(),
            dst = %edge.dst.as_str(),
            kind = ?edge.kind,
        ),
    )]
    pub(crate) async fn do_put_edge(&self, edge: &Edge) -> Result<(), StoreError> {
        let conn = self.require_conn("put_edge")?.clone();
        let src = edge.src.as_str().to_owned();
        let dst = edge.dst.as_str().to_owned();
        let kind = edge.kind.as_db_str();
        let weight = edge.weight.map(f64::from);

        conn.call(move |c| {
            c.execute(
                "INSERT OR REPLACE INTO edges (src, dst, kind, weight) \
                       VALUES (?1, ?2, ?3, ?4)",
                params![src, dst, kind, weight],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await?;
        Ok(())
    }

    /// Inherent `remove_edge` implementation; the trait method
    /// [`MemoryStore::remove_edge`] guards `self.conn` then delegates here.
    ///
    /// Returns `true` when a row was deleted, `false` when no row matched.
    /// The schema's `updates`-edge immutability trigger is on UPDATE only,
    /// so DELETE of an `updates` edge succeeds today; the integration test
    /// `updates_edge_immutable_via_remove_returns_error` pins this behaviour.
    ///
    /// [`MemoryStore::remove_edge`]: cairn_core::contract::memory_store::MemoryStore::remove_edge
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Worker`] when the background `tokio_rusqlite`
    /// worker fails and [`StoreError::Sqlite`] for SQL errors surfaced
    /// through the worker.
    #[instrument(
        skip(self),
        err,
        fields(
            verb = "remove_edge",
            src = %key.src.as_str(),
            dst = %key.dst.as_str(),
            kind = ?key.kind,
        ),
    )]
    pub(crate) async fn do_remove_edge(&self, key: &EdgeKey) -> Result<bool, StoreError> {
        let conn = self.require_conn("remove_edge")?.clone();
        let src = key.src.as_str().to_owned();
        let dst = key.dst.as_str().to_owned();
        let kind = key.kind.as_db_str();

        let removed = conn
            .call(move |c| {
                let n = c.execute(
                    "DELETE FROM edges WHERE src = ?1 AND dst = ?2 AND kind = ?3",
                    params![src, dst, kind],
                )?;
                Ok::<_, tokio_rusqlite::Error>(n > 0)
            })
            .await?;
        Ok(removed)
    }

    /// Inherent `neighbours` implementation; the trait method
    /// [`MemoryStore::neighbours`] guards `self.conn` then delegates here.
    ///
    /// Filters endpoints through `records_latest` so superseded or
    /// tombstoned records are dropped from the result. The pivot itself is
    /// not required to be live — callers can ask for the neighbourhood of a
    /// retired record.
    ///
    /// [`MemoryStore::neighbours`]: cairn_core::contract::memory_store::MemoryStore::neighbours
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Worker`] / [`StoreError::Sqlite`] for SQL
    /// failures and [`StoreError::Invariant`] when a stored `record_id` /
    /// `kind` value cannot be parsed back into the typed newtype / enum
    /// (corruption / schema-drift signal).
    #[instrument(
        skip(self),
        err,
        fields(verb = "neighbours", record_id = %id.as_str(), dir = ?dir),
    )]
    pub(crate) async fn do_neighbours(
        &self,
        id: &RecordId,
        dir: EdgeDir,
    ) -> Result<Vec<Edge>, StoreError> {
        let conn = self.require_conn("neighbours")?.clone();
        let key = id.as_str().to_owned();

        let edges = conn
            .call(move |c| {
                let sql = match dir {
                    EdgeDir::Out => {
                        "SELECT src, dst, kind, weight FROM edges \
                           WHERE src = ?1 \
                             AND dst IN (SELECT record_id FROM records_latest)"
                    }
                    EdgeDir::In => {
                        "SELECT src, dst, kind, weight FROM edges \
                           WHERE dst = ?1 \
                             AND src IN (SELECT record_id FROM records_latest)"
                    }
                    EdgeDir::Both => {
                        "SELECT src, dst, kind, weight FROM edges \
                           WHERE (src = ?1 AND dst IN (SELECT record_id FROM records_latest)) \
                              OR (dst = ?1 AND src IN (SELECT record_id FROM records_latest))"
                    }
                };
                let mut stmt = c.prepare(sql)?;
                let rows = stmt
                    .query_map(params![key], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, Option<f64>>(3)?,
                        ))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                let mut out = Vec::with_capacity(rows.len());
                for (src, dst, kind, weight) in rows {
                    let src_id = record_id_from_str(&src)
                        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                    let dst_id = record_id_from_str(&dst)
                        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                    let kind_enum = EdgeKind::parse(&kind).ok_or_else(|| {
                        tokio_rusqlite::Error::Other(Box::new(StoreError::Invariant {
                            what: format!("unknown edge kind `{kind}`"),
                        }))
                    })?;
                    out.push(Edge {
                        src: src_id,
                        dst: dst_id,
                        kind: kind_enum,
                        // Weights are stored as REAL (f64) but the contract
                        // exposes them as f32; precision loss is acceptable
                        // here — the column is bounded to `[0.0, 1.0]`.
                        #[allow(clippy::cast_possible_truncation)]
                        weight: weight.map(|w| w as f32),
                    });
                }
                Ok::<_, tokio_rusqlite::Error>(out)
            })
            .await?;
        Ok(edges)
    }
}
