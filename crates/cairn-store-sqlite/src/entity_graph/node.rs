//! `upsert_entity` — entity-node upsert with `name_norm` dedup.
//!
//! On dedup hit (a row with the same `name_norm` already exists), this is a
//! pure-read no-op: we return the existing row's id and write nothing —
//! including no WAL row, mirroring `SqliteMemoryStore::do_upsert`'s
//! idempotent branch (brief §5.2).
//!
//! On fresh insert, we write through the generic `wal_ops` + `wal_steps`
//! tables using the helpers in [`crate::entity_graph::wal`] inside the same
//! `tokio_rusqlite` transaction as the row insert.

use cairn_core::domain::graph::{EntityId, EntityNode};
use rusqlite::params;
use tracing::instrument;

use crate::entity_graph::wal;
use crate::error::StoreError;
use crate::store::SqliteMemoryStore;

impl SqliteMemoryStore {
    /// Inherent upsert-entity implementation; the trait method
    /// [`MemoryStore::upsert_entity`] guards `self.conn` then delegates here.
    ///
    /// [`MemoryStore::upsert_entity`]: cairn_core::contract::memory_store::MemoryStore::upsert_entity
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Worker`] when the background `tokio_rusqlite`
    /// worker fails (channel closed, panic in worker), and [`StoreError::Sqlite`]
    /// for SQL errors surfaced through the worker.
    #[instrument(
        skip(self, node),
        err,
        fields(
            verb = "upsert_entity",
            entity_id = %node.id.as_str(),
            name_norm = %node.name_norm,
        ),
    )]
    pub(crate) async fn do_upsert_entity(&self, node: &EntityNode) -> Result<EntityId, StoreError> {
        // The storage boundary owns the canonical `name_norm` invariant:
        // `name_norm` is ALWAYS derived here from `node.name` via the
        // shared `normalize_entity_name` helper, regardless of what the
        // caller supplied. This keeps write-time dedup and read-time
        // ByName lookup (`get_entity_by_name`, which uses the same helper)
        // on a single canonical contract — a caller that bypassed the
        // helper and constructed a non-canonical `name_norm` cannot break
        // dedup or hide an existing row from later lookups.
        //
        // Punctuation/whitespace-only `node.name` collapses to `None` and
        // is rejected — distinct entities must not share the empty key.
        let Some(canonical_name_norm) =
            cairn_core::domain::graph::normalize_entity_name(&node.name)
        else {
            return Err(StoreError::SchemaDrift(format!(
                "entity_nodes.name {:?} normalizes to an empty key — \
                 entity name must contain at least one alphanumeric character",
                node.name,
            )));
        };

        let conn = self.require_conn("upsert_entity")?.clone();

        // Clone the small payload into the move closure.
        let id_str = node.id.as_str().to_owned();
        let name = node.name.clone();
        let name_norm = canonical_name_norm;
        let summary = node.summary.clone();
        let created_at = node.created_at;
        let embedding_id = node.embedding_id.clone();

        let result_id = conn
            .call(move |c| {
                let tx = c.transaction()?;

                // Dedup probe: if a row with this `name_norm` already
                // exists, return its id. No mutation, no WAL row.
                let existing: Option<String> = tx
                    .query_row(
                        "SELECT id FROM entity_nodes WHERE name_norm = ?1",
                        params![&name_norm],
                        |r| r.get(0),
                    )
                    .map(Some)
                    .or_else(|e| match e {
                        rusqlite::Error::QueryReturnedNoRows => Ok(None),
                        other => Err(other),
                    })?;

                if let Some(found) = existing {
                    tx.commit()?;
                    return Ok(found);
                }

                // Fresh insert path: WAL op + row insert + step + commit op.
                let op_id = wal::issue_op(&tx, "graph_upsert_entity", &id_str)
                    .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                tx.execute(
                    "INSERT INTO entity_nodes \
                     (id, name, name_norm, summary, created_at, embedding_id) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        &id_str,
                        &name,
                        &name_norm,
                        &summary,
                        created_at,
                        &embedding_id
                    ],
                )?;
                wal::write_step(&tx, &op_id, 0, "insert_entity", None)
                    .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                wal::commit_op(&tx, &op_id)
                    .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;

                tx.commit()?;
                Ok(id_str)
            })
            .await?;

        Ok(EntityId::from(result_id))
    }
}
