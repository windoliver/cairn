//! `upsert_entity_edge` — bitemporal edge upsert with body-hash idempotency
//! and contradiction-resolution flow.
//!
//! Three branches in one transaction:
//! 1. **Fresh insert** — no live edge for `(source, target, relation)`.
//!    Insert new edge + write WAL op `graph_upsert_edge` with one step.
//! 2. **Idempotent re-upsert** — live edge has identical `body_hash`.
//!    Commit empty tx, no WAL row, return the existing edge id.
//! 3. **Contradiction** — live edge has different `body_hash`.
//!    UPDATE `old.invalid_at` = `new.valid_at`, INSERT new edge,
//!    write WAL op `graph_contradict` with two steps (invalidate, insert).

use cairn_core::domain::graph::{EntityEdge, EntityEdgeId, EntityEdgeOutcome};
use tracing::instrument;

use crate::entity_graph::wal;
use crate::error::StoreError;
use crate::store::SqliteMemoryStore;

/// Compute the body-hash used to detect substantive edge changes.
///
/// Includes only fields that constitute the *fact*: confidence tier, score,
/// event-time start, and source record. Excludes id, ingestion-time fields,
/// and the conflict key itself (source/target/relation).
pub(super) fn body_hash(edge: &EntityEdge) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(edge.confidence.as_db_str().as_bytes());
    h.update(&edge.confidence_score.to_le_bytes());
    h.update(&edge.valid_at.to_le_bytes());
    if let Some(rid) = &edge.source_record_id {
        h.update(b"|");
        h.update(rid.as_str().as_bytes());
    }
    *h.finalize().as_bytes()
}

/// Insert one `entity_edges` row using a pre-computed `body_hash`.
///
/// Shared between the fresh-insert and contradiction branches; also reused
/// by `resolve_contradiction` (Task 12).
pub(super) fn insert_edge(
    tx: &rusqlite::Transaction<'_>,
    edge: &EntityEdge,
    body_hash: &[u8; 32],
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO entity_edges \
         (id, source_id, target_id, relation, confidence, confidence_score, \
          valid_at, invalid_at, created_at, source_record_id, body_hash) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        rusqlite::params![
            edge.id.as_str(),
            edge.source_id.as_str(),
            edge.target_id.as_str(),
            &edge.relation,
            edge.confidence.as_db_str(),
            edge.confidence_score,
            edge.valid_at,
            edge.invalid_at,
            edge.created_at,
            edge.source_record_id
                .as_ref()
                .map(|r| r.as_str().to_owned()),
            &body_hash[..],
        ],
    )?;
    Ok(())
}

impl SqliteMemoryStore {
    /// Inherent upsert-edge implementation; the trait method
    /// [`MemoryStore::upsert_entity_edge`] guards `self.conn` then delegates here.
    ///
    /// [`MemoryStore::upsert_entity_edge`]: cairn_core::contract::memory_store::MemoryStore::upsert_entity_edge
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Worker`] when the background `tokio_rusqlite`
    /// worker fails (channel closed, panic in worker), and [`StoreError::Sqlite`]
    /// for SQL errors surfaced through the worker.
    #[instrument(
        skip(self, edge),
        err,
        fields(
            verb = "upsert_entity_edge",
            edge_id = %edge.id.as_str(),
            relation = %edge.relation,
        ),
    )]
    pub(crate) async fn do_upsert_entity_edge(
        &self,
        edge: &EntityEdge,
    ) -> Result<EntityEdgeOutcome, StoreError> {
        let conn = self.require_conn("upsert_entity_edge")?.clone();
        let edge = edge.clone();
        let bh = body_hash(&edge);

        let outcome = conn
            .call(
                move |c| -> Result<EntityEdgeOutcome, tokio_rusqlite::Error> {
                    let tx = c.transaction()?;

                    // Probe for live edge with same triple.
                    let existing: Option<(String, Vec<u8>)> = tx
                        .query_row(
                            "SELECT id, body_hash FROM entity_edges \
                         WHERE source_id = ?1 AND target_id = ?2 AND relation = ?3 \
                           AND invalid_at IS NULL AND expired_at IS NULL",
                            rusqlite::params![
                                edge.source_id.as_str(),
                                edge.target_id.as_str(),
                                &edge.relation,
                            ],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .map(Some)
                        .or_else(|e| match e {
                            rusqlite::Error::QueryReturnedNoRows => Ok(None),
                            other => Err(other),
                        })?;

                    let outcome = match existing {
                        None => {
                            // Branch 1: fresh insert.
                            let op_id = wal::issue_op(&tx, "graph_upsert_edge", edge.id.as_str())
                                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                            insert_edge(&tx, &edge, &bh)
                                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                            wal::write_step(&tx, &op_id, 0, "insert_edge", None)
                                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                            wal::commit_op(&tx, &op_id)
                                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                            EntityEdgeOutcome {
                                new_edge_id: edge.id.clone(),
                                invalidated_edge_id: None,
                                body_was_unchanged: false,
                            }
                        }
                        Some((existing_id, existing_hash)) if existing_hash == bh => {
                            // Branch 2: idempotent re-upsert. Empty tx, no WAL row.
                            EntityEdgeOutcome {
                                new_edge_id: EntityEdgeId::from(existing_id),
                                invalidated_edge_id: None,
                                body_was_unchanged: true,
                            }
                        }
                        Some((existing_id, existing_hash)) => {
                            // Branch 3: contradiction. UPDATE old then INSERT new
                            // so at no point are there two rows matching the
                            // partial UNIQUE on the live triple.
                            let op_id = wal::issue_op(&tx, "graph_contradict", edge.id.as_str())
                                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                            tx.execute(
                                "UPDATE entity_edges SET invalid_at = ?1 WHERE id = ?2",
                                rusqlite::params![edge.valid_at, &existing_id],
                            )?;
                            // Pre-image of old edge's body_hash for compensation.
                            wal::write_step(
                                &tx,
                                &op_id,
                                0,
                                "invalidate_edge",
                                Some(&existing_hash),
                            )
                            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                            insert_edge(&tx, &edge, &bh)
                                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                            wal::write_step(&tx, &op_id, 1, "insert_edge", None)
                                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                            wal::commit_op(&tx, &op_id)
                                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                            EntityEdgeOutcome {
                                new_edge_id: edge.id.clone(),
                                invalidated_edge_id: Some(EntityEdgeId::from(existing_id)),
                                body_was_unchanged: false,
                            }
                        }
                    };

                    tx.commit()?;
                    Ok(outcome)
                },
            )
            .await?;

        Ok(outcome)
    }
}
