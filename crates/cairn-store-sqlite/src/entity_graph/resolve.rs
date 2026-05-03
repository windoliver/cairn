//! `resolve_contradiction` — caller-driven invalidate-and-insert.
//!
//! Mostly an internal hook used by `SqliteMemoryStore::do_upsert_entity_edge`'s
//! contradiction branch; exposed for callers (e.g. `lint --fix-graph`) that
//! need to invalidate a specific known-bad edge.

use cairn_core::domain::graph::{EntityEdge, EntityEdgeId, EntityEdgeOutcome};
use tracing::instrument;

use crate::entity_graph::wal;
use crate::error::StoreError;
use crate::store::SqliteMemoryStore;

impl SqliteMemoryStore {
    /// Inherent resolve-contradiction implementation; the trait method
    /// [`MemoryStore::resolve_contradiction`] guards `self.conn` then delegates here.
    ///
    /// [`MemoryStore::resolve_contradiction`]: cairn_core::contract::memory_store::MemoryStore::resolve_contradiction
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Worker`] when the background `tokio_rusqlite`
    /// worker fails, [`StoreError::Sqlite`] for SQL errors, or
    /// [`StoreError::NotFound`] when `old_id` does not name a live edge.
    #[instrument(
        skip(self, old_id, new_edge),
        err,
        fields(
            verb = "resolve_contradiction",
            new_edge_id = %new_edge.id.as_str(),
        ),
    )]
    pub(crate) async fn do_resolve_contradiction(
        &self,
        old_id: &EntityEdgeId,
        new_edge: &EntityEdge,
    ) -> Result<EntityEdgeOutcome, StoreError> {
        let conn = self.require_conn("resolve_contradiction")?.clone();
        let old_id_owned = old_id.as_str().to_owned();
        let new_edge = new_edge.clone();
        let bh = super::edge::body_hash(&new_edge);

        let outcome = conn
            .call(
                move |c| -> Result<EntityEdgeOutcome, tokio_rusqlite::Error> {
                    let tx = c.transaction()?;

                    // Verify old edge exists and is live; capture pre_image body_hash.
                    let pre_image: Vec<u8> = tx.query_row(
                        "SELECT body_hash FROM entity_edges \
                         WHERE id = ?1 AND invalid_at IS NULL AND expired_at IS NULL",
                        [&old_id_owned],
                        |r| r.get(0),
                    )?;

                    let op_id = wal::issue_op(&tx, "graph_contradict", new_edge.id.as_str())
                        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                    tx.execute(
                        "UPDATE entity_edges SET invalid_at = ?1 WHERE id = ?2",
                        rusqlite::params![new_edge.valid_at, &old_id_owned],
                    )?;
                    wal::write_step(&tx, &op_id, 0, "invalidate_edge", Some(&pre_image))
                        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                    super::edge::insert_edge(&tx, &new_edge, &bh)
                        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                    wal::write_step(&tx, &op_id, 1, "insert_edge", None)
                        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                    wal::commit_op(&tx, &op_id)
                        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                    tx.commit()?;

                    Ok(EntityEdgeOutcome {
                        new_edge_id: new_edge.id.clone(),
                        invalidated_edge_id: Some(EntityEdgeId::from(old_id_owned)),
                        body_was_unchanged: false,
                    })
                },
            )
            .await?;

        Ok(outcome)
    }
}
