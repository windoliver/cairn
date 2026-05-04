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
/// Includes every field that constitutes the *fact* and is persisted on
/// the row: confidence tier, score, the full event-time window
/// (`valid_at` and `invalid_at`), the ingestion-time start
/// (`created_at`), and source record. Excludes id (PK), `expired_at`
/// (set on tombstone, not asserted by the caller), and the conflict key
/// itself (source/target/relation, covered by the SELECT).
///
/// `invalid_at` MUST be in the domain: a re-upsert with the same triple
/// and otherwise-identical fields but a different `invalid_at` (e.g.
/// live → bounded) would otherwise hash equal to the live row, return
/// `body_was_unchanged = true`, and silently leave the row queryable
/// past the requested close-time.
///
/// `created_at` is also in the domain: `as_of_ingest_time` slicing
/// reads it directly. Excluding it would let a re-upsert with a
/// corrected ingestion-time start hit the unchanged path and keep the
/// stale value without any WAL signal.
pub(super) fn body_hash(edge: &EntityEdge) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(edge.confidence.as_db_str().as_bytes());
    h.update(&edge.confidence_score.to_le_bytes());
    h.update(&edge.valid_at.to_le_bytes());
    // invalid_at: tagged Option encoding so None ≠ Some(0).
    if let Some(t) = edge.invalid_at {
        h.update(&[1u8]);
        h.update(&t.to_le_bytes());
    } else {
        h.update(&[0u8]);
    }
    h.update(&edge.created_at.to_le_bytes());
    if let Some(rid) = &edge.source_record_id {
        h.update(b"|");
        h.update(rid.as_str().as_bytes());
    }
    *h.finalize().as_bytes()
}

/// Reject caller-asserted edges whose declared event-time window is
/// degenerate (`invalid_at == valid_at`) or strictly negative
/// (`invalid_at < valid_at`). The schema CHECK allows the degenerate
/// case so the contradiction branch can produce instantaneous-replace
/// rows internally; callers asserting empty windows is almost always a
/// mistake (the row would be inert in every as-of query) and was used
/// in the round-7 finding to bypass overlap routing — a `[T, T)` upsert
/// against a live `[100, NULL)` row matched the overlap probe and
/// silently set the live row's `invalid_at = T`, killing it.
pub(super) fn reject_degenerate_or_negative_window(edge: &EntityEdge) -> Result<(), StoreError> {
    if let Some(invalid_at) = edge.invalid_at
        && invalid_at <= edge.valid_at
    {
        return Err(StoreError::Invariant {
            what: format!(
                "caller-asserted empty or negative edge window: \
                 valid_at={} invalid_at={} for edge id={}",
                edge.valid_at,
                invalid_at,
                edge.id.as_str(),
            ),
        });
    }
    Ok(())
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
    // Body: probe A (body-hash idempotency) → probe B (live triple) →
    // fresh-insert / backdated-guard / contradiction. Splitting the
    // branches into helpers would require threading `tx` through async
    // closures and surface no real reuse, so the workspace
    // too_many_lines cap is suppressed here with a reason.
    #[allow(clippy::too_many_lines)]
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
        reject_degenerate_or_negative_window(edge)?;
        let conn = self.require_conn("upsert_entity_edge")?.clone();
        let edge = edge.clone();
        let bh = body_hash(&edge);

        let outcome = conn
            .call(
                move |c| -> Result<EntityEdgeOutcome, tokio_rusqlite::Error> {
                    let tx = c.transaction()?;

                    // Probe A — content idempotency. Any non-purged
                    // (expired_at IS NULL) row with this exact body hash
                    // for this triple is the SAME fact, regardless of
                    // whether it has been event-time invalidated. Without
                    // this, a retry of an `invalid_at: Some(...)` upsert
                    // would skip past the live-only probe below, take the
                    // fresh-insert path, and either trip a PK conflict
                    // (same id) or duplicate the historical row (different
                    // id) — violating the upsert contract.
                    let dup_id: Option<String> = tx
                        .query_row(
                            "SELECT id FROM entity_edges \
                             WHERE source_id = ?1 AND target_id = ?2 AND relation = ?3 \
                               AND body_hash = ?4 AND expired_at IS NULL \
                             LIMIT 1",
                            rusqlite::params![
                                edge.source_id.as_str(),
                                edge.target_id.as_str(),
                                &edge.relation,
                                &bh[..],
                            ],
                            |r| r.get(0),
                        )
                        .map(Some)
                        .or_else(|e| match e {
                            rusqlite::Error::QueryReturnedNoRows => Ok(None),
                            other => Err(other),
                        })?;
                    if let Some(found) = dup_id {
                        tx.commit()?;
                        return Ok(EntityEdgeOutcome {
                            new_edge_id: EntityEdgeId::from(found),
                            invalidated_edge_id: None,
                            body_was_unchanged: true,
                        });
                    }

                    // Probe B — any non-expired row whose event-time
                    // window overlaps the new edge's window. Subsumes the
                    // narrower live-triple lookup. Open-interval overlap
                    // for `[v, i)` semantics, NULL-aware (no sentinel —
                    // a sentinel like 9223372036854775807 would collide
                    // with a legitimate live edge anchored at i64::MAX).
                    //
                    // Old window `[old.valid_at, old.invalid_at)` and
                    // new window `[new.valid_at, new.invalid_at)` overlap
                    // iff
                    //   (new.invalid_at IS NULL OR old.valid_at < new.invalid_at)
                    //   AND (old.invalid_at IS NULL OR old.invalid_at > new.valid_at).
                    //
                    // The reject_degenerate_or_negative_window guard above
                    // ensures the new window has positive length, so we
                    // do not need to filter degenerate stored rows here
                    // (a stored `[T, T)` window matches no overlap with a
                    // strictly-positive new window).
                    //
                    // Pre-fix Probe B only matched live (invalid_at IS NULL)
                    // rows, so two bounded upserts for the same triple
                    // with overlapping windows (e.g. [100,200) then
                    // [150,250)) both took the fresh-insert path and a
                    // single as-of read at t=175 returned duplicates,
                    // breaking the bitemporal invariant.
                    let overlap: Option<(String, Vec<u8>, i64, Option<i64>)> = tx
                        .query_row(
                            "SELECT id, body_hash, valid_at, invalid_at FROM entity_edges \
                             WHERE source_id = ?1 AND target_id = ?2 AND relation = ?3 \
                               AND expired_at IS NULL \
                               AND (?5 IS NULL OR valid_at < ?5) \
                               AND (invalid_at IS NULL OR invalid_at > ?4) \
                             LIMIT 1",
                            rusqlite::params![
                                edge.source_id.as_str(),
                                edge.target_id.as_str(),
                                &edge.relation,
                                edge.valid_at,
                                edge.invalid_at,
                            ],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                        )
                        .map(Some)
                        .or_else(|e| match e {
                            rusqlite::Error::QueryReturnedNoRows => Ok(None),
                            other => Err(other),
                        })?;

                    let outcome = match overlap {
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
                        Some((
                            existing_id,
                            existing_hash,
                            existing_valid_at,
                            existing_invalid_at,
                        )) => {
                            // Branch 2: an overlapping non-expired row
                            // exists. Routing depends on whether it's the
                            // single live row (invalid_at IS NULL — the
                            // contradiction case) or a bounded historical
                            // row (invalid_at IS NOT NULL — caller must
                            // resolve explicitly).
                            //
                            // Bounded-overlap reject: silently splitting
                            // a bounded historical window via a fresh
                            // upsert would violate the bitemporal "at
                            // most one fact per (triple, event-time T)"
                            // invariant. Force the caller to use
                            // resolve_contradiction with explicit
                            // timestamps so the intent is auditable.
                            if existing_invalid_at.is_some() {
                                return Err(tokio_rusqlite::Error::Other(Box::new(
                                    StoreError::Invariant {
                                        what: format!(
                                            "overlapping bounded edge for triple {}→{}/{}: \
                                             existing id={} window=[{},{}] overlaps new \
                                             window=[{},{:?}]; use resolve_contradiction",
                                            edge.source_id.as_str(),
                                            edge.target_id.as_str(),
                                            edge.relation,
                                            existing_id,
                                            existing_valid_at,
                                            existing_invalid_at.unwrap_or(i64::MAX),
                                            edge.valid_at,
                                            edge.invalid_at,
                                        ),
                                    },
                                )));
                            }
                            // From here: existing_invalid_at IS NULL,
                            // i.e. live triple. Standard contradiction:
                            // UPDATE old then INSERT new so the partial
                            // UNIQUE on the live triple is never
                            // violated mid-tx.
                            //
                            // Backdated guard: contradiction sets
                            // `old.invalid_at = new.valid_at`. If
                            // new.valid_at < old.valid_at, the schema CHECK
                            // would abort the transaction with a generic
                            // SQL error. Reject up front with a typed
                            // domain error so callers can distinguish
                            // "retroactive correction not supported" from
                            // an infrastructure failure.
                            if edge.valid_at < existing_valid_at {
                                return Err(tokio_rusqlite::Error::Other(Box::new(
                                    StoreError::Invariant {
                                        what: format!(
                                            "backdated contradiction not supported: \
                                             new.valid_at={} < old.valid_at={} for edge id={}",
                                            edge.valid_at, existing_valid_at, existing_id,
                                        ),
                                    },
                                )));
                            }
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
            .await
            .map_err(super::unpack_worker_err)?;

        Ok(outcome)
    }
}
