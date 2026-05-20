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

/// Old-row capture for `do_resolve_contradiction`: every field needed by
/// the cross-triple guard, the backdated/bounded-target guards, the
/// overlap probe, the `body_hash` recomputation, and the WAL `pre_image`.
type OldRow = (
    i64,
    Option<i64>,
    String,
    String,
    String,
    String,
    f32,
    i64,
    Option<String>,
    String,
);

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
    /// [`StoreError::NotFound`] when `old_id` does not name a non-expired
    /// edge (live or bounded).
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

                    // Lookup old edge: any non-expired row (live OR bounded).
                    // Pre-round-8 this clause was `invalid_at IS NULL` —
                    // `upsert_entity_edge` rejects overlap with bounded rows
                    // and points callers at this method, so refusing bounded
                    // targets here makes those overlaps unrepairable through
                    // the public API. Capture source/target/relation for the
                    // cross-triple guard below; capture the existing
                    // `invalid_at` so we can enforce that a bounded target's
                    // window is being shrunk (not extended or zeroed) by the
                    // contradiction.
                    let lookup_sql = format!(
                        "SELECT valid_at, invalid_at, \
                                source_id, target_id, relation, \
                                confidence, confidence_score, created_at, \
                                source_record_id, {pre_image} \
                         FROM entity_edges \
                         WHERE id = ?1 AND expired_at IS NULL",
                        pre_image = super::ENTITY_EDGE_PRE_IMAGE_JSON,
                    );
                    let (
                        old_valid_at,
                        old_invalid_at,
                        old_src,
                        old_tgt,
                        old_rel,
                        old_confidence_db,
                        old_confidence_score,
                        old_created_at,
                        old_source_record_id,
                        pre_image_json,
                    ): OldRow = tx
                        .query_row(&lookup_sql, [&old_id_owned], |r| {
                            Ok((
                                r.get(0)?,
                                r.get(1)?,
                                r.get(2)?,
                                r.get(3)?,
                                r.get(4)?,
                                r.get(5)?,
                                r.get(6)?,
                                r.get(7)?,
                                r.get(8)?,
                                r.get(9)?,
                            ))
                        })
                        .map_err(|e| match e {
                            rusqlite::Error::QueryReturnedNoRows => {
                                tokio_rusqlite::Error::Other(Box::new(StoreError::NotFound {
                                    id: old_id_owned.clone(),
                                }))
                            }
                            other => tokio_rusqlite::Error::from(other),
                        })?;

                    // Cross-triple guard: a stale or buggy repair caller
                    // could pair `old_id` with a `new_edge` belonging to
                    // a *different* triple. Pre-round-8 the body of the
                    // op then closed the unrelated live fact and inserted
                    // an unrelated new one in the same WAL op, and the
                    // overlap probe ran against the old triple, leaving
                    // the new triple unchecked. Reject up front.
                    if new_edge.source_id.as_str() != old_src
                        || new_edge.target_id.as_str() != old_tgt
                        || new_edge.relation != old_rel
                    {
                        return Err(tokio_rusqlite::Error::Other(Box::new(
                            StoreError::Invariant {
                                what: format!(
                                    "resolve_contradiction triple mismatch: \
                                     old=({},{},{}) new=({},{},{}) for edge id={}",
                                    old_src,
                                    old_tgt,
                                    old_rel,
                                    new_edge.source_id.as_str(),
                                    new_edge.target_id.as_str(),
                                    new_edge.relation,
                                    old_id_owned,
                                ),
                            },
                        )));
                    }

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
                                    new_edge.valid_at, old_valid_at, old_id_owned,
                                ),
                            },
                        )));
                    }

                    // Bounded-target guards: when the old row is
                    // already bounded `[old.valid_at, old.invalid_at)`,
                    // the contradiction must (a) place `new.valid_at`
                    // strictly inside the existing window so we are
                    // actually shrinking the tail, and (b) preserve
                    // `old.invalid_at` as the replacement's `invalid_at`
                    // so we do not silently extend the fact's lifetime
                    // past the original endpoint. A live (NULL) or
                    // later-bounded `new.invalid_at` would mutate
                    // history outside the selected target — that
                    // belongs to a separate split/merge policy, not the
                    // contradiction primitive.
                    if let Some(old_inv) = old_invalid_at {
                        if new_edge.valid_at >= old_inv {
                            return Err(tokio_rusqlite::Error::Other(Box::new(
                                StoreError::Invariant {
                                    what: format!(
                                        "resolve_contradiction would not shrink bounded \
                                         window: new.valid_at={} >= old.invalid_at={} for \
                                         edge id={}",
                                        new_edge.valid_at, old_inv, old_id_owned,
                                    ),
                                },
                            )));
                        }
                        if new_edge.invalid_at != Some(old_inv) {
                            return Err(tokio_rusqlite::Error::Other(Box::new(
                                StoreError::Invariant {
                                    what: format!(
                                        "resolve_contradiction must preserve bounded \
                                         endpoint: new.invalid_at={:?} != old.invalid_at={} \
                                         for edge id={}",
                                        new_edge.invalid_at, old_inv, old_id_owned,
                                    ),
                                },
                            )));
                        }
                    }

                    // Overlap guard: another non-expired row for this
                    // triple (excluding the one we're about to
                    // invalidate) whose window overlaps the *new* edge's
                    // window would create duplicate facts at any as-of
                    // read inside the overlap. Same NULL-aware predicate
                    // as upsert_entity_edge::Probe B. Pre-fix this method
                    // verified only old_id's liveness, leaving lint /
                    // repair callers free to introduce overlap.
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
                                old_src,
                                old_tgt,
                                old_rel,
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

                    // Recompute the OLD row's body_hash with its new
                    // (post-contradiction) `invalid_at`. body_hash
                    // includes invalid_at in its domain, so leaving the
                    // stored hash as the pre-mutation value would let
                    // a stale re-upsert of the original fact match
                    // Probe A and return body_was_unchanged=true even
                    // though that fact is no longer present. Same fix
                    // as in the upsert contradiction branch.
                    let new_old_hash = super::edge::body_hash_components(
                        &old_confidence_db,
                        old_confidence_score,
                        old_valid_at,
                        Some(new_edge.valid_at),
                        old_created_at,
                        old_source_record_id.as_deref(),
                    );
                    let op_id = wal::issue_op(&tx, "graph_contradict", new_edge.id.as_str())
                        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                    tx.execute(
                        "UPDATE entity_edges SET invalid_at = ?1, body_hash = ?2 \
                         WHERE id = ?3",
                        rusqlite::params![new_edge.valid_at, &new_old_hash[..], &old_id_owned],
                    )?;
                    wal::write_step(
                        &tx,
                        &op_id,
                        0,
                        "invalidate_edge",
                        Some(pre_image_json.as_bytes()),
                    )
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
