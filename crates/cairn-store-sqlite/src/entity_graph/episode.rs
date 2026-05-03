//! `link_entity_episode` — idempotent record↔entity join insert.
//!
//! On duplicate hit (a row with `(episode_id, entity_node_id)` already
//! exists), this is a pure-read no-op: we commit the transaction without
//! writing any WAL row and return `Ok(false)`.
//!
//! On a fresh insert we write through `wal_ops` + `wal_steps` using the
//! helpers in [`crate::entity_graph::wal`] inside the same
//! `tokio_rusqlite` transaction as the row insert, then return `Ok(true)`.

use cairn_core::domain::graph::EntityId;
use cairn_core::domain::record::RecordId;
use rusqlite::params;
use tracing::instrument;

use crate::entity_graph::wal;
use crate::error::StoreError;
use crate::store::SqliteMemoryStore;

impl SqliteMemoryStore {
    /// Inherent link-episode implementation; the trait method
    /// [`MemoryStore::link_entity_episode`] guards `self.conn` then delegates here.
    ///
    /// [`MemoryStore::link_entity_episode`]: cairn_core::contract::memory_store::MemoryStore::link_entity_episode
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Worker`] when the background `tokio_rusqlite`
    /// worker fails (channel closed, panic in worker), and [`StoreError::Sqlite`]
    /// for SQL errors surfaced through the worker.
    #[instrument(
        skip(self, entity_id, record_id),
        err,
        fields(
            verb = "link_entity_episode",
            entity_id = %entity_id.as_str(),
            record_id = %record_id.as_str(),
        ),
    )]
    pub(crate) async fn do_link_entity_episode(
        &self,
        entity_id: &EntityId,
        record_id: &RecordId,
    ) -> Result<bool, StoreError> {
        let conn = self.require_conn("link_entity_episode")?.clone();

        // Clone the small payload into the move closure (must be Send + 'static).
        let entity = entity_id.as_str().to_owned();
        let record = record_id.as_str().to_owned();

        let inserted = conn
            .call(move |c| -> Result<bool, tokio_rusqlite::Error> {
                let tx = c.transaction()?;

                // Dedup probe: if this (episode_id, entity_node_id) pair
                // already exists, return false without writing a WAL row.
                let existing: Option<i64> = tx
                    .query_row(
                        "SELECT 1 FROM entity_episodes \
                         WHERE episode_id = ?1 AND entity_node_id = ?2",
                        params![&record, &entity],
                        |r| r.get(0),
                    )
                    .map(Some)
                    .or_else(|e| match e {
                        rusqlite::Error::QueryReturnedNoRows => Ok(None),
                        other => Err(other),
                    })?;

                if existing.is_some() {
                    tx.commit()?;
                    return Ok(false);
                }

                // Fresh insert path: WAL op + row insert + step + commit op.
                let op_id = wal::issue_op(&tx, "graph_link_episode", &entity)
                    .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                let now = chrono::Utc::now().timestamp_millis();
                tx.execute(
                    "INSERT INTO entity_episodes (episode_id, entity_node_id, linked_at) \
                     VALUES (?1, ?2, ?3)",
                    params![&record, &entity, now],
                )?;
                wal::write_step(&tx, &op_id, 0, "insert_episode_link", None)
                    .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                wal::commit_op(&tx, &op_id)
                    .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;

                tx.commit()?;
                Ok(true)
            })
            .await?;

        Ok(inserted)
    }
}
