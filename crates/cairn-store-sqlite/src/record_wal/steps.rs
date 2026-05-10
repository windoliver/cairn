//! Record WAL step bodies.

use cairn_core::wal::{OperationId, StepDef};
use rusqlite::{Transaction, params};

use crate::record_wal::locks::RecordLocks;
use crate::record_wal::payload::{ExpirePayload, ForgetPayload, StoredEmbedOutcome, UpsertPayload};
use crate::wal::runner::{StepBody, StepBodyError};

pub(crate) enum RecordStepPayload {
    Upsert(Box<UpsertPayload>),
    Expire(Box<ExpirePayload>),
    Forget(Box<ForgetPayload>),
}

pub(crate) struct RecordStepBody {
    payload: RecordStepPayload,
    locks: RecordLocks,
}

impl RecordStepBody {
    #[must_use]
    pub(crate) fn new_upsert(payload: UpsertPayload, locks: RecordLocks) -> Self {
        Self {
            payload: RecordStepPayload::Upsert(Box::new(payload)),
            locks,
        }
    }

    #[must_use]
    pub(crate) fn new_expire(payload: ExpirePayload, locks: RecordLocks) -> Self {
        Self {
            payload: RecordStepPayload::Expire(Box::new(payload)),
            locks,
        }
    }

    #[must_use]
    pub(crate) fn new_forget(payload: ForgetPayload, locks: RecordLocks) -> Self {
        Self {
            payload: RecordStepPayload::Forget(Box::new(payload)),
            locks,
        }
    }
}

impl StepBody for RecordStepBody {
    fn run(
        &self,
        tx: &mut Transaction<'_>,
        op_id: &OperationId,
        step: &StepDef,
    ) -> Result<(), StepBodyError> {
        self.locks
            .assert_live_in_tx(tx)
            .map_err(|e| StepBodyError::Failed(format!("fenced: {e}")))?;

        match (&self.payload, step.name) {
            (RecordStepPayload::Upsert(payload), "snapshot.stage") => {
                stage_snapshot(tx, op_id, step, &payload.planned.target_id)
            }
            (RecordStepPayload::Upsert(payload), "primary.upsert_cow") => {
                let plan = payload
                    .planned
                    .to_store_plan()
                    .map_err(|e| StepBodyError::Failed(e.to_string()))?;
                crate::store::upsert::stage_upsert_cow_in_tx(tx, &payload.record, &plan)
                    .map_err(|e| StepBodyError::Failed(e.to_string()))
            }
            (RecordStepPayload::Upsert(payload), "vector.upsert") => {
                upsert_vector(tx, &payload.planned.outcome_record_id, &payload.embed)
            }
            (RecordStepPayload::Upsert(payload), "fts.upsert") => {
                upsert_fts(tx, &payload.planned.outcome_record_id)
            }
            (RecordStepPayload::Upsert(payload), "edges.upsert") => {
                upsert_edges(tx, &payload.planned.outcome_record_id);
                Ok(())
            }
            (RecordStepPayload::Upsert(payload), "primary.activate") => {
                let plan = payload
                    .planned
                    .to_store_plan()
                    .map_err(|e| StepBodyError::Failed(e.to_string()))?;
                crate::store::upsert::activate_upsert_in_tx(tx, &plan)
                    .map_err(|e| StepBodyError::Failed(e.to_string()))
            }
            (RecordStepPayload::Expire(payload), "snapshot.stage") => {
                stage_snapshot(tx, op_id, step, payload.target_id.as_str())
            }
            (RecordStepPayload::Expire(payload), "primary.mark_expired") => {
                mark_expired(tx, payload)
            }
            (RecordStepPayload::Expire(payload), "vector.drain") => {
                drain_vectors_for_target(tx, payload.target_id.as_str())
            }
            (RecordStepPayload::Expire(payload), "fts.drain") => {
                drain_fts_for_target(tx, payload.target_id.as_str())
            }
            (RecordStepPayload::Expire(payload), "edges.drain") => {
                drain_edges_for_target(tx, payload.target_id.as_str())
            }
            (RecordStepPayload::Forget(payload), "primary.mark_tombstone") => {
                mark_tombstone_and_emit_receipt(tx, op_id, payload)
            }
            (RecordStepPayload::Forget(payload), "vector.drain") => {
                drain_vectors_for_target(tx, payload.target_id.as_str())
            }
            (RecordStepPayload::Forget(payload), "fts.drain") => {
                drain_fts_for_target(tx, payload.target_id.as_str())
            }
            (RecordStepPayload::Forget(payload), "edges.drain") => {
                drain_edges_for_target(tx, payload.target_id.as_str())
            }
            (RecordStepPayload::Forget(payload), "primary.purge") => {
                primary_purge_for_target(tx, payload.target_id.as_str())
            }
            (RecordStepPayload::Forget(payload), "wal.purge_pre_images") => {
                purge_wal_pre_images_for_target(tx, op_id, payload)
            }
            (RecordStepPayload::Forget(_), "snapshot.purge") => {
                // P0: no `.cairn/snapshots/` or `nexus-data/` mirror exists
                // (issue #109 lands the cold-storage layer). The bundle
                // rewrite is a no-op until the snapshot registry exists.
                Ok(())
            }
            (
                RecordStepPayload::Upsert(_)
                | RecordStepPayload::Expire(_)
                | RecordStepPayload::Forget(_),
                _,
            ) => Ok(()),
        }
    }
}

fn stage_snapshot(
    tx: &rusqlite::Transaction<'_>,
    op_id: &OperationId,
    step: &StepDef,
    target_id: &str,
) -> Result<(), StepBodyError> {
    let rows = {
        let mut stmt = tx
            .prepare(
                "SELECT record_id, version, active, tombstoned, tombstone_reason, body_hash \
                 FROM records WHERE target_id = ?1 ORDER BY version",
            )
            .map_err(StepBodyError::Storage)?;
        stmt.query_map(rusqlite::params![target_id], |row| {
            Ok(serde_json::json!({
                "record_id": row.get::<_, String>(0)?,
                "version": row.get::<_, i64>(1)?,
                "active": row.get::<_, i64>(2)?,
                "tombstoned": row.get::<_, i64>(3)?,
                "tombstone_reason": row.get::<_, Option<String>>(4)?,
                "body_hash": row.get::<_, String>(5)?,
            }))
        })
        .map_err(StepBodyError::Storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(StepBodyError::Storage)?
    };
    let bytes = serde_json::to_vec(&rows)
        .map_err(|e| StepBodyError::Failed(format!("snapshot json: {e}")))?;
    tx.execute(
        "UPDATE wal_steps SET pre_image = ?1 \
         WHERE operation_id = ?2 AND step_ord = ?3",
        rusqlite::params![bytes, op_id.as_str(), step.ord],
    )
    .map_err(StepBodyError::Storage)?;
    Ok(())
}

fn upsert_fts(tx: &Transaction<'_>, record_id: &str) -> Result<(), StepBodyError> {
    let row: (i64, String, String, String, String) = tx
        .query_row(
            "SELECT rowid, kind, class, scope, body FROM records WHERE record_id = ?1",
            params![record_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .map_err(StepBodyError::Storage)?;
    tx.execute("DELETE FROM records_fts WHERE rowid = ?1", params![row.0])
        .map_err(StepBodyError::Storage)?;
    tx.execute(
        "INSERT INTO records_fts(rowid, kind, class, scope, body) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![row.0, row.1, row.2, row.3, row.4],
    )
    .map_err(StepBodyError::Storage)?;
    Ok(())
}

fn upsert_vector(
    tx: &Transaction<'_>,
    record_id: &str,
    embed: &StoredEmbedOutcome,
) -> Result<(), StepBodyError> {
    match embed {
        StoredEmbedOutcome::Succeeded {
            vector,
            model_label,
        } => {
            // sqlite-vec vec0 virtual tables do not support UPSERT syntax.
            // DELETE + INSERT keeps replacement atomic with the record row.
            tx.execute(
                "DELETE FROM record_vectors WHERE record_id = ?",
                params![record_id],
            )
            .map_err(StepBodyError::Storage)?;
            tx.execute(
                "INSERT INTO record_vectors(record_id, embedding, model) \
                   VALUES (?, ?, ?)",
                params![record_id, vector, model_label],
            )
            .map_err(StepBodyError::Storage)?;
            tx.execute(
                "DELETE FROM pending_embeddings WHERE record_id = ?",
                params![record_id],
            )
            .map_err(StepBodyError::Storage)?;
        }
        StoredEmbedOutcome::Failed { error } => {
            let now_secs = now_secs();
            tx.execute(
                "INSERT INTO pending_embeddings \
                     (record_id, reason, attempt_count, last_error, enqueued_at) \
                   VALUES (?, 'embed_failed', 0, ?, ?) \
                   ON CONFLICT(record_id) DO UPDATE \
                     SET last_error  = excluded.last_error, \
                         enqueued_at = COALESCE(pending_embeddings.enqueued_at, excluded.enqueued_at)",
                params![record_id, error, now_secs],
            )
            .map_err(StepBodyError::Storage)?;
        }
        StoredEmbedOutcome::Skipped => {}
    }
    Ok(())
}

fn upsert_edges(_tx: &Transaction<'_>, _record_id: &str) {}

fn mark_expired(tx: &Transaction<'_>, payload: &ExpirePayload) -> Result<(), StepBodyError> {
    tx.execute(
        "UPDATE records \
            SET active = 0, \
                tombstoned = 1, \
                tombstone_reason = COALESCE(tombstone_reason, 'expire'), \
                updated_at = ?1 \
          WHERE target_id = ?2 \
            AND NOT (active = 0 AND tombstoned = 1 AND tombstone_reason IS NOT NULL)",
        params![crate::store::current_unix_ms(), payload.target_id.as_str()],
    )
    .map_err(StepBodyError::Storage)?;
    Ok(())
}

fn drain_vectors_for_target(tx: &Transaction<'_>, target: &str) -> Result<(), StepBodyError> {
    tx.execute(
        "DELETE FROM record_vectors \
          WHERE record_id IN (SELECT record_id FROM records WHERE target_id = ?1)",
        params![target],
    )
    .map_err(StepBodyError::Storage)?;
    tx.execute(
        "DELETE FROM pending_embeddings \
          WHERE record_id IN (SELECT record_id FROM records WHERE target_id = ?1)",
        params![target],
    )
    .map_err(StepBodyError::Storage)?;
    Ok(())
}

fn drain_fts_for_target(tx: &Transaction<'_>, target: &str) -> Result<(), StepBodyError> {
    let rowids = {
        let mut stmt = tx
            .prepare("SELECT rowid FROM records WHERE target_id = ?1")
            .map_err(StepBodyError::Storage)?;
        stmt.query_map(params![target], |row| row.get::<_, i64>(0))
            .map_err(StepBodyError::Storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StepBodyError::Storage)?
    };
    for rowid in rowids {
        tx.execute("DELETE FROM records_fts WHERE rowid = ?1", params![rowid])
            .map_err(StepBodyError::Storage)?;
    }
    Ok(())
}

fn drain_edges_for_target(tx: &Transaction<'_>, target: &str) -> Result<(), StepBodyError> {
    tx.execute(
        "DELETE FROM edges \
          WHERE src IN (SELECT record_id FROM records WHERE target_id = ?1) \
             OR dst IN (SELECT record_id FROM records WHERE target_id = ?1)",
        params![target],
    )
    .map_err(StepBodyError::Storage)?;
    Ok(())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Phase A of `forget_record` (brief §5.6 row 2). Tombstones every version
/// of the target with `reason='forget'` and emits a body-free
/// `ForgetIntent` consent receipt in the same transaction. The fusion is
/// what makes the receipt atomic with the tombstone — neither half can be
/// observed without the other.
///
/// Phase A is atomic: the runner wraps this body in one `SQLite` txn, so
/// the tombstone `UPDATE` and the `consent_journal` append commit
/// together or not at all. A crashed Phase A leaves neither side effect;
/// replay produces exactly one receipt per successful Phase-A commit.
/// The `UPDATE` itself is idempotent — re-running over already-tombstoned
/// rows is a no-op.
fn mark_tombstone_and_emit_receipt(
    tx: &Transaction<'_>,
    op_id: &OperationId,
    payload: &ForgetPayload,
) -> Result<(), StepBodyError> {
    use cairn_core::domain::{ConsentEvent, ConsentKind, ConsentPayload};
    use sha2::{Digest, Sha256};

    let now_ms = crate::store::current_unix_ms();

    // Brief §5.6 Phase A: tombstone every version of the target with
    // reason='forget' and active=0.
    tx.execute(
        "UPDATE records \
            SET active = 0, \
                tombstoned = 1, \
                tombstone_reason = 'forget', \
                updated_at = ?1 \
          WHERE target_id = ?2",
        params![now_ms, payload.target_id.as_str()],
    )
    .map_err(StepBodyError::Storage)?;

    // sha256 of the raw target id. P1 follow-up (brief §14) introduces a
    // per-user salt; the receipt schema does not change.
    let mut hasher = Sha256::new();
    hasher.update(payload.target_id.as_str().as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    let target_id_hash = format!("sha256:{hex}");
    let subject_hash = format!("sha256:{hex}");

    let event = ConsentEvent {
        consent_id: format!("CNS{}", ulid::Ulid::new()),
        kind: ConsentKind::ForgetIntent,
        actor: payload.actor.clone(),
        subject: subject_hash,
        scope: scope_canonical_wire(&payload.scope, payload.scope_tier),
        op_id: Some(op_id.as_str().to_owned()),
        sensor_id: None,
        payload: ConsentPayload::IntentReceipt {
            target_id_hash,
            scope_tier: payload.scope_tier,
            reason_code: payload.reason_code.clone(),
        },
        decided_at: now_rfc3339_from_ms(now_ms)?,
        expires_at: None,
    };

    crate::consent::append(tx, &event)
        .map_err(|e| StepBodyError::Failed(format!("consent append: {e}")))?;
    Ok(())
}

/// Render a `ScopeTuple` into the consent journal `scope` slot. Defaults
/// (no IDL-addressable dimension set) fall back to the visibility tier
/// wire form so the slot is non-empty — `validate_scope` rejects empty.
fn scope_canonical_wire(
    scope: &cairn_core::domain::ScopeTuple,
    tier: cairn_core::domain::MemoryVisibility,
) -> String {
    let wire = scope.canonical_wire();
    if wire.is_empty() {
        tier.as_str().to_owned()
    } else {
        wire
    }
}

/// Build a UTC `Rfc3339Timestamp` from a Unix-millis instant. Matches
/// the `from_unix_secs` constructor by truncating sub-second precision —
/// `decided_at_iso` writers in this crate use second precision.
///
/// `current_unix_ms` saturates negative values to `0`, but we still clamp
/// here (`max(0)`) so a future caller passing a degraded clock value
/// cannot push us into the `from_unix_secs` error path. Errors are
/// surfaced as [`StepBodyError::Failed`] so the WAL marks the step
/// failed rather than the helper crashing.
fn now_rfc3339_from_ms(
    unix_ms: i64,
) -> Result<cairn_core::domain::Rfc3339Timestamp, StepBodyError> {
    let secs = unix_ms.div_euclid(1_000).max(0);
    cairn_core::domain::Rfc3339Timestamp::from_unix_secs(secs)
        .map_err(|e| StepBodyError::Failed(format!("rfc3339 from_unix_secs: {e}")))
}

/// Phase B step 4: collapse all body-bearing rows for the target. Re-runs
/// the index drains defensively — vector.drain / fts.drain / edges.drain
/// may have completed in a prior crash window, but the `idempotent: false`
/// `primary.purge` step is the audit-invariant boundary so we make sure
/// nothing survives.
fn primary_purge_for_target(tx: &Transaction<'_>, target: &str) -> Result<(), StepBodyError> {
    tx.execute(
        "DELETE FROM record_vectors \
          WHERE record_id IN (SELECT record_id FROM records WHERE target_id = ?1)",
        params![target],
    )
    .map_err(StepBodyError::Storage)?;
    tx.execute(
        "DELETE FROM pending_embeddings \
          WHERE record_id IN (SELECT record_id FROM records WHERE target_id = ?1)",
        params![target],
    )
    .map_err(StepBodyError::Storage)?;
    tx.execute("DELETE FROM records WHERE target_id = ?1", params![target])
        .map_err(StepBodyError::Storage)?;
    Ok(())
}

/// Phase B step 5: scrub WAL pre-image blobs that mention the forgotten
/// target id, replacing them with a `{purged, target_id_hash, op_id,
/// purged_at}` stub. The audit invariant (brief §5.6) is that no
/// post-forget reader can recover the original record body from a WAL
/// snapshot.
///
/// LIKE prefilter: target ids are ULIDs (`[0-9A-Z]{26}`) so they cannot
/// contain `%` or `_` — no escaping needed. Any `wal_steps.pre_image`
/// whose CAST-to-TEXT contains the literal target id substring is
/// rewritten in place.
///
/// **Coverage in P0 is forward-defense.** `stage_snapshot` (the only
/// step that currently writes `pre_image` blobs) projects
/// `{record_id, version, active, tombstoned, tombstone_reason,
/// body_hash}` — none of which carry body bytes and none of which
/// embed the target id (`RecordId` and `TargetId` are independent
/// ULIDs per
/// `cairn_core::domain::record::tests::target_id_independent_of_id`).
/// So the body-leakage invariant is already satisfied by construction
/// for current snapshot `pre_image`s, and the LIKE prefilter on the raw
/// target-id substring will not match any of them in practice.
///
/// This step body still runs because:
/// 1. Future step bodies that stage `target_id`-bearing `pre_image`s
///    will be caught.
/// 2. It documents intent — the audit-invariant test (#58 Task 10)
///    can grep for the target id substring with confidence that any
///    matches would have been stubbed.
///
/// A follow-up (filed when issue #58 closes) may extend this to scrub
/// `pre_image`s that reference any of the target's purged `record_id`s,
/// but that requires capturing the record-id list at Phase A before
/// `primary.purge` deletes the records — out of scope here.
fn purge_wal_pre_images_for_target(
    tx: &Transaction<'_>,
    self_op_id: &OperationId,
    payload: &ForgetPayload,
) -> Result<(), StepBodyError> {
    use sha2::{Digest, Sha256};

    let target = payload.target_id.as_str();
    let needle = format!("%{target}%");
    let rows: Vec<(String, u32)> = {
        let mut stmt = tx
            .prepare(
                "SELECT operation_id, step_ord \
                   FROM wal_steps \
                  WHERE pre_image IS NOT NULL \
                    AND CAST(pre_image AS TEXT) LIKE ?1",
            )
            .map_err(StepBodyError::Storage)?;
        stmt.query_map(params![needle], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
        })
        .map_err(StepBodyError::Storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(StepBodyError::Storage)?
    };

    let now_ms = crate::store::current_unix_ms();
    let mut hasher = Sha256::new();
    hasher.update(target.as_bytes());
    let target_id_hash = format!("sha256:{:x}", hasher.finalize());

    for (op_id, step_ord) in rows {
        let stub = serde_json::json!({
            "purged": true,
            "target_id_hash": target_id_hash,
            "op_id": self_op_id.as_str(),
            "purged_at": now_ms,
        });
        let bytes = serde_json::to_vec(&stub)
            .map_err(|e| StepBodyError::Failed(format!("stub json: {e}")))?;
        tx.execute(
            "UPDATE wal_steps \
                SET pre_image = ?1 \
              WHERE operation_id = ?2 AND step_ord = ?3",
            params![bytes, op_id, step_ord],
        )
        .map_err(StepBodyError::Storage)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_vector_upsert_replay_does_not_increment_attempt_count() {
        let mut conn = crate::open::open_in_memory_sync().expect("open");
        let tx = conn.transaction().expect("tx");
        let record = cairn_core::domain::record::tests_export::sample_record();
        let plan = crate::store::upsert::plan_upsert_in_tx(&tx, &record).expect("plan");
        crate::store::upsert::stage_upsert_cow_in_tx(&tx, &record, &plan).expect("stage");

        let failed = StoredEmbedOutcome::Failed {
            error: "embed unavailable".to_owned(),
        };
        upsert_vector(&tx, plan.outcome_record_id.as_str(), &failed).expect("first vector upsert");
        upsert_vector(&tx, plan.outcome_record_id.as_str(), &failed)
            .expect("replayed vector upsert");

        let row: (i64, String, Option<i64>) = tx
            .query_row(
                "SELECT attempt_count, last_error, last_attempt_at \
                 FROM pending_embeddings WHERE record_id = ?1",
                params![plan.outcome_record_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("pending row");

        assert_eq!(row.0, 0);
        assert_eq!(row.1, "embed unavailable");
        assert_eq!(row.2, None);
    }
}
