//! `resolve_contradiction` — caller-driven invalidate-and-insert.
//!
//! Mostly an internal hook used by `SqliteMemoryStore::do_upsert_entity_edge`'s
//! contradiction branch; exposed for callers (e.g. `lint --fix-graph`) that
//! need to invalidate a specific known-bad edge.

use cairn_core::domain::graph::{EntityEdge, EntityEdgeId, EntityEdgeOutcome};
use tracing::instrument;

use crate::entity_graph::{unpack_worker_err, wal};
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
    // The closure body sequences four guards (degenerate-window, NotFound,
    // backdated, overlap) plus the WAL-step apply. Splitting helpers would
    // either require passing an &mut Transaction across them (more wiring,
    // less locality) or make the WAL-step ordering harder to audit in one
    // read.
    #[allow(clippy::too_many_lines)]
    pub(crate) async fn do_resolve_contradiction(
        &self,
        old_id: &EntityEdgeId,
        new_edge: &EntityEdge,
    ) -> Result<EntityEdgeOutcome, StoreError> {
        super::edge::reject_degenerate_or_negative_window(new_edge)?;
        let conn = self.require_conn("resolve_contradiction")?.clone();
        let old_id_owned = old_id.as_str().to_owned();
        let new_edge = new_edge.clone();
        let bh = super::edge::body_hash(&new_edge);

        let outcome = conn
            .call(
                move |c| -> Result<EntityEdgeOutcome, tokio_rusqlite::Error> {
                    let tx = c.transaction()?;

                    // Verify old edge exists and is live; capture pre_image body_hash
                    // and valid_at for the backdated-contradiction guard below.
                    // Map QueryReturnedNoRows to a typed StoreError::NotFound — without
                    // this, await? converts the bare rusqlite error into the generic
                    // StoreError::Worker variant, hiding the deterministic stale/missing
                    // input from retry/recovery callers.
                    let (pre_image, old_valid_at): (Vec<u8>, i64) = tx
                        .query_row(
                            "SELECT body_hash, valid_at FROM entity_edges \
                             WHERE id = ?1 AND invalid_at IS NULL AND expired_at IS NULL",
                            [&old_id_owned],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .map_err(|e| match e {
                            rusqlite::Error::QueryReturnedNoRows => {
                                tokio_rusqlite::Error::Other(Box::new(StoreError::NotFound {
                                    id: old_id_owned.clone(),
                                }))
                            }
                            other => tokio_rusqlite::Error::from(other),
                        })?;

                    // Backdated guard: contradiction sets `old.invalid_at = new.valid_at`.
                    // The CHECK on entity_edges (invalid_at >= valid_at) would otherwise
                    // abort with a generic SQL error. Reject here with a typed domain
                    // error so callers can distinguish "retroactive correction not
                    // supported" from infrastructure failure.
                    if new_edge.valid_at < old_valid_at {
                        return Err(tokio_rusqlite::Error::Other(Box::new(
                            StoreError::Invariant {
                                what: format!(
                                    "backdated contradiction not supported: \
                                     new.valid_at={} < old.valid_at={} for edge id={}",
                                    new_edge.valid_at,
                                    old_valid_at,
                                    old_id_owned,
                                ),
                            },
                        )));
                    }

                    // Overlap guard: another non-expired bounded row for
                    // this triple (excluding the one we're about to
                    // invalidate) whose window overlaps the new edge's
                    // window would create duplicate facts at any as-of
                    // read inside the overlap. Same NULL-aware predicate
                    // as upsert_entity_edge::Probe B. Pre-fix this method
                    // verified only old_id's liveness, leaving lint /
                    // repair callers free to introduce overlap.
                    let triple: (String, String, String) = tx.query_row(
                        "SELECT source_id, target_id, relation \
                         FROM entity_edges WHERE id = ?1",
                        [&old_id_owned],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )?;
                    let conflicting: Option<(String, i64, Option<i64>)> = tx
                        .query_row(
                            "SELECT id, valid_at, invalid_at FROM entity_edges \
                             WHERE source_id = ?1 AND target_id = ?2 AND relation = ?3 \
                               AND id != ?4 \
                               AND expired_at IS NULL \
                               AND (?6 IS NULL OR valid_at < ?6) \
                               AND (invalid_at IS NULL OR invalid_at > ?5) \
                             LIMIT 1",
                            rusqlite::params![
                                triple.0,
                                triple.1,
                                triple.2,
                                &old_id_owned,
                                new_edge.valid_at,
                                new_edge.invalid_at,
                            ],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                        )
                        .map(Some)
                        .or_else(|e| match e {
                            rusqlite::Error::QueryReturnedNoRows => Ok(None),
                            other => Err(other),
                        })?;
                    if let Some((cid, cv, ci)) = conflicting {
                        return Err(tokio_rusqlite::Error::Other(Box::new(
                            StoreError::Invariant {
                                what: format!(
                                    "resolve_contradiction would introduce overlap: \
                                     existing id={} window=[{},{:?}] overlaps new \
                                     window=[{},{:?}]",
                                    cid, cv, ci, new_edge.valid_at, new_edge.invalid_at,
                                ),
                            },
                        )));
                    }

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
            .await
            .map_err(unpack_worker_err)?;

        Ok(outcome)
    }
}
