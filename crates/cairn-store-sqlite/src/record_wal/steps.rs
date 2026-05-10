//! Record WAL step bodies.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

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
    /// Live-version count captured inside the Phase A transaction by the
    /// `primary.mark_tombstone` step body for `forget_record`. `0` for
    /// other op kinds. Read by `apply_forget_record` after the runner
    /// returns to populate `ForgetReceipt::deleted_count` with a value
    /// that reflects what THIS op actually tombstoned, not a pre-lock
    /// snapshot that two concurrent forgets could both read as `1`.
    deleted_count: Arc<AtomicU64>,
}

impl RecordStepBody {
    #[must_use]
    pub(crate) fn new_upsert(payload: UpsertPayload, locks: RecordLocks) -> Self {
        Self {
            payload: RecordStepPayload::Upsert(Box::new(payload)),
            locks,
            deleted_count: Arc::new(AtomicU64::new(0)),
        }
    }

    #[must_use]
    pub(crate) fn new_expire(payload: ExpirePayload, locks: RecordLocks) -> Self {
        Self {
            payload: RecordStepPayload::Expire(Box::new(payload)),
            locks,
            deleted_count: Arc::new(AtomicU64::new(0)),
        }
    }

    #[must_use]
    pub(crate) fn new_forget(payload: ForgetPayload, locks: RecordLocks) -> Self {
        Self {
            payload: RecordStepPayload::Forget(Box::new(payload)),
            locks,
            deleted_count: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Handle that the caller stashes before `runner::run_from` so it can
    /// observe the in-txn count after the runner returns.
    pub(crate) fn deleted_count_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.deleted_count)
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
                mark_tombstone_and_emit_receipt(tx, op_id, payload, &self.deleted_count)
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
///
/// **Truly-purged forgets do not append a consent receipt.** When the
/// in-txn `SELECT COUNT(*)` observes zero rows for the target (records
/// table holds nothing), the helper records `deleted_count = 0` and
/// returns without writing to `consent_journal`. The authoritative
/// receipt is the one the original forget wrote with the real
/// scope/tier; appending another receipt with the post-purge
/// `ScopeTuple::default()` + `MemoryVisibility::Private` defaults
/// would dilute the audit trail. Round-5 review (Codex).
///
/// **Already-expired targets DO get a receipt.** A target whose rows
/// exist but are `active = 0` (e.g. previously expired by the
/// expiration workflow) still has body-bearing data on disk. Phase B
/// will purge those rows; the receipt records WHO authorized the
/// destructive op. The scope/tier comes from `load_scope_and_tier`'s
/// `ORDER BY version DESC` which reads the latest row regardless of
/// active status, so the receipt preserves the original tier even
/// when no live row exists. Round-6 review (Codex).
fn mark_tombstone_and_emit_receipt(
    tx: &Transaction<'_>,
    op_id: &OperationId,
    payload: &ForgetPayload,
    deleted_count: &AtomicU64,
) -> Result<(), StepBodyError> {
    use cairn_core::domain::{ConsentEvent, ConsentKind, ConsentPayload};
    use sha2::{Digest, Sha256};

    let now_ms = crate::store::current_unix_ms();

    // In-txn counts under the record-WAL lock: `live_count` populates
    // `ForgetReceipt::deleted_count` (the count THIS op tombstoned);
    // `total_rows` discriminates "truly already purged" (no receipt)
    // from "rows present but inactive" (receipt with original tier).
    let live_count: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM records WHERE target_id = ?1 AND active = 1",
            params![payload.target_id.as_str()],
            |row| row.get(0),
        )
        .map_err(StepBodyError::Storage)?;
    // Snapshot scope + visibility from the latest record version
    // INSIDE the Phase A transaction. The pre-lock read in
    // `apply_forget_record` populates the payload's `scope` /
    // `scope_tier` for lock-acquisition purposes (entity + session
    // legs), but a same-target upsert that committed between that
    // pre-lock read and the record-WAL lock acquisition could have
    // moved the record to a different scope/tier. Brief §14 audit
    // invariant: the consent receipt must record the scope/tier of
    // the rows actually destroyed by THIS op.
    //
    // `total_rows` discriminates "truly already purged" (no
    // receipt) from "rows present" (receipt with in-txn tier).
    // Single-row scalar SELECT: always returns one row even when the
    // subqueries are NULL (COALESCE -> '').
    let (total_rows, in_txn_scope_json, in_txn_visibility): (i64, String, String) = tx
        .query_row(
            "SELECT \
                (SELECT COUNT(*) FROM records WHERE target_id = ?1), \
                COALESCE( \
                    (SELECT scope FROM records WHERE target_id = ?1 \
                       ORDER BY version DESC LIMIT 1), \
                    '' \
                ), \
                COALESCE( \
                    (SELECT visibility FROM records WHERE target_id = ?1 \
                       ORDER BY version DESC LIMIT 1), \
                    '' \
                )",
            params![payload.target_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(StepBodyError::Storage)?;
    let live_count_u64 = u64::try_from(live_count).unwrap_or(0);
    deleted_count.store(live_count_u64, Ordering::SeqCst);

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

    // Truly already-purged target: no rows exist. Skip the receipt
    // (the original forget wrote the authoritative one). See doc
    // comment above for the audit-invariant rationale.
    if total_rows == 0 {
        return Ok(());
    }

    // Round-8 review (Codex): for `total_rows > 0` we MUST resolve the
    // in-txn scope/tier from the records table — falling back to
    // `payload.scope` / `payload.scope_tier` would let stale pre-lock
    // values leak into the audit receipt. Treat any parse failure or
    // empty-string snapshot as a storage invariant break and abort
    // Phase A; the WAL marks the step Failed and recovery surfaces it.
    if in_txn_scope_json.is_empty() {
        return Err(StepBodyError::Failed(format!(
            "records.scope is NULL/empty for target {} despite total_rows > 0",
            payload.target_id.as_str()
        )));
    }
    if in_txn_visibility.is_empty() {
        return Err(StepBodyError::Failed(format!(
            "records.visibility is NULL/empty for target {} despite total_rows > 0",
            payload.target_id.as_str()
        )));
    }
    let in_txn_scope: cairn_core::domain::ScopeTuple = serde_json::from_str(&in_txn_scope_json)
        .map_err(|e| {
            StepBodyError::Failed(format!(
                "records.scope is not valid JSON for target {}: {e}",
                payload.target_id.as_str()
            ))
        })?;
    // Round-9 review (Codex): semantic validation. JSON-syntactic
    // success is not enough — `{}` deserializes fine but is an invalid
    // domain value (no IDL-addressable dimension). Reject corrupt
    // domain shapes too; otherwise schema drift could let an
    // irreversible forget commit a receipt with the original
    // tenant/session/entity erased.
    in_txn_scope.validate().map_err(|e| {
        StepBodyError::Failed(format!(
            "records.scope failed domain validation for target {}: {e}",
            payload.target_id.as_str()
        ))
    })?;
    let in_txn_tier =
        cairn_core::domain::MemoryVisibility::parse(&in_txn_visibility).map_err(|e| {
            StepBodyError::Failed(format!(
                "records.visibility {in_txn_visibility:?} not parseable for target {}: {e}",
                payload.target_id.as_str()
            ))
        })?;

    // Known limitation (Round-8 review): the receipt records the scope
    // and tier of the LATEST version only. A target lineage can in
    // principle hold versions at different visibility tiers (an upsert
    // that promoted Private → Public), and Phase B purges every
    // version's body bytes. The receipt's single tier is therefore an
    // incomplete summary of what was destroyed when the lineage spans
    // tiers. Brief §14 forget-receipt allowlist does not yet model a
    // multi-tier list; a follow-up should either extend the receipt
    // schema or enforce per-target tier immutability at upsert time.
    // For now we record the latest tier — better than the original
    // pre-lock payload value and matches the most-recent reader-visible
    // classification.

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
        scope: scope_canonical_wire(&in_txn_scope, in_txn_tier),
        op_id: Some(op_id.as_str().to_owned()),
        sensor_id: None,
        payload: ConsentPayload::IntentReceipt {
            target_id_hash,
            scope_tier: in_txn_tier,
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

    /// Round-3 review (Codex): integration tests cannot distinguish a
    /// pre-lock SELECT from the in-txn SELECT because the record-WAL
    /// locks are fail-fast — the contender never reaches Phase A. Pin
    /// the in-txn semantic at the unit-test layer instead: call
    /// `mark_tombstone_and_emit_receipt` directly against a transaction
    /// where rows have been mutated AFTER the helper would otherwise
    /// have captured a count, and assert the `AtomicU64` reflects the
    /// post-mutation state. A regression to a pre-lock SELECT (count
    /// captured outside this function) would not write through this
    /// `AtomicU64` at all and the assertion would fail closed.
    #[test]
    fn mark_tombstone_count_is_captured_inside_transaction() {
        use std::sync::atomic::{AtomicU64, Ordering};

        use cairn_core::domain::Identity;
        use cairn_core::domain::taxonomy::MemoryVisibility;
        use cairn_core::wal::{OperationId, WalKind};

        use crate::record_wal::ops::new_operation_id;
        use crate::record_wal::payload::ForgetPayload;

        let mut conn = crate::open::open_in_memory_sync().expect("open");
        let record = cairn_core::domain::record::tests_export::sample_record();

        // Stage one active record + one already-tombstoned superseded
        // version of the same target. The helper must count only the
        // single live row.
        let tx = conn.transaction().expect("tx");
        let plan = crate::store::upsert::plan_upsert_in_tx(&tx, &record).expect("plan");
        crate::store::upsert::stage_upsert_cow_in_tx(&tx, &record, &plan).expect("stage");
        crate::store::upsert::activate_upsert_in_tx(&tx, &plan).expect("activate");
        tx.commit().expect("commit upsert");

        let payload = ForgetPayload {
            target_id: record.target_id.clone(),
            scope: record.scope.clone(),
            reason_code: "user_command".to_owned(),
            actor: Identity::parse("hmn:test:v1").expect("identity"),
            scope_tier: MemoryVisibility::Private,
        };
        let op_id: OperationId = new_operation_id(WalKind::ForgetRecord).expect("op id");
        let cell = AtomicU64::new(0);

        // First call inside its own transaction: live count = 1.
        let tx = conn.transaction().expect("tx 1");
        mark_tombstone_and_emit_receipt(&tx, &op_id, &payload, &cell).expect("first phase A");
        assert_eq!(
            cell.load(Ordering::SeqCst),
            1,
            "first Phase A inside the transaction must observe the one live row"
        );
        tx.commit().expect("commit first phase A");

        // Reset the cell so the assertion below cannot be satisfied by
        // the previous write surviving. Then run again on the now-
        // tombstoned target: in-txn count = 0. A pre-lock SELECT
        // regression would never write to this cell at all (the helper
        // wouldn't take it as a parameter), so the test would fail at
        // compile time — but for any future implementation that DOES
        // capture the count outside the transaction, this catches the
        // stale-count regression.
        cell.store(99, Ordering::SeqCst);
        let op_id_2: OperationId = new_operation_id(WalKind::ForgetRecord).expect("op id 2");
        let tx = conn.transaction().expect("tx 2");
        mark_tombstone_and_emit_receipt(&tx, &op_id_2, &payload, &cell).expect("second phase A");
        assert_eq!(
            cell.load(Ordering::SeqCst),
            0,
            "second Phase A must observe zero live rows — its SELECT \
             happens INSIDE the transaction, AFTER the previous \
             commit purged active=1. A pre-lock SELECT (captured \
             before this function ran) would leak its stale `1` here."
        );
    }

    /// Round-8 review (Codex): the integration-level race regression
    /// test cannot construct a real "stale payload after a same-target
    /// upsert" interleaving — apply_forget_record's pre-lock read sees
    /// whatever is committed when it runs. Pin the in-txn scope/tier
    /// invariant at the unit-test layer instead: build a payload that
    /// asserts a Private tier (the `stale` value), upsert a Public v2
    /// AFTER the payload is built, then call the helper directly. The
    /// emitted consent row's payload_json must say `"public"` — proving
    /// the helper read the in-txn snapshot, not the payload.
    #[test]
    fn mark_tombstone_uses_in_txn_scope_tier_not_stale_payload() {
        use std::sync::atomic::AtomicU64;

        use cairn_core::domain::Identity;
        use cairn_core::domain::taxonomy::MemoryVisibility;
        use cairn_core::wal::{OperationId, WalKind};

        use crate::record_wal::ops::new_operation_id;
        use crate::record_wal::payload::ForgetPayload;

        let mut conn = crate::open::open_in_memory_sync().expect("open");
        let mut record = cairn_core::domain::record::tests_export::sample_record();
        record.visibility = MemoryVisibility::Private;
        let target = record.target_id.clone();

        // v1 Private — what the simulated pre-lock read would observe.
        let tx = conn.transaction().expect("tx v1");
        let plan_v1 = crate::store::upsert::plan_upsert_in_tx(&tx, &record).expect("plan v1");
        crate::store::upsert::stage_upsert_cow_in_tx(&tx, &record, &plan_v1).expect("stage v1");
        crate::store::upsert::activate_upsert_in_tx(&tx, &plan_v1).expect("activate v1");
        tx.commit().expect("commit v1");

        // Build the "stale" payload that an apply_forget_record running
        // in the race window would have constructed.
        let payload = ForgetPayload {
            target_id: target.clone(),
            scope: record.scope.clone(),
            reason_code: "user_command".to_owned(),
            actor: Identity::parse("hmn:test:v1").expect("identity"),
            scope_tier: MemoryVisibility::Private,
        };

        // Simulated race: a competing upsert commits Public v2 AFTER the
        // payload was built, BEFORE the helper runs.
        let mut v2 = record.clone();
        v2.visibility = MemoryVisibility::Public;
        v2.body = format!("{}-public", record.body);
        let tx = conn.transaction().expect("tx v2");
        let plan_v2 = crate::store::upsert::plan_upsert_in_tx(&tx, &v2).expect("plan v2");
        crate::store::upsert::stage_upsert_cow_in_tx(&tx, &v2, &plan_v2).expect("stage v2");
        crate::store::upsert::activate_upsert_in_tx(&tx, &plan_v2).expect("activate v2");
        tx.commit().expect("commit v2");

        // Run the helper with the stale (Private) payload against the
        // database where Public v2 is now the latest version.
        let op_id: OperationId = new_operation_id(WalKind::ForgetRecord).expect("op id");
        let cell = AtomicU64::new(0);
        let tx = conn.transaction().expect("tx forget");
        mark_tombstone_and_emit_receipt(&tx, &op_id, &payload, &cell).expect("phase A");
        tx.commit().expect("commit phase A");

        // The consent_journal row written under op_id MUST carry the
        // in-txn (Public) tier, not the payload's stale Private.
        let payload_json: String = conn
            .query_row(
                "SELECT payload_json FROM consent_journal \
                  WHERE kind = 'forget_intent' AND op_id = ?1",
                params![op_id.as_str()],
                |row| row.get(0),
            )
            .expect("consent row");
        assert!(
            payload_json.contains("\"scope_tier\":\"public\""),
            "receipt scope_tier must come from the in-txn snapshot of the \
             latest version (Public). A regression that read from \
             payload.scope_tier (Private) would dilute the audit trail. \
             Got: {payload_json}"
        );
        assert!(
            !payload_json.contains("\"scope_tier\":\"private\""),
            "receipt must NOT echo the stale payload's Private tier. \
             Got: {payload_json}"
        );
    }

    /// Round-8 review (Codex): when `total_rows > 0` but the in-txn
    /// `records.visibility` is unparseable (corrupt row / schema drift),
    /// the helper MUST fail closed instead of falling back to the
    /// stale payload tier. This test inserts a row with a garbage
    /// visibility string and asserts the helper rejects with
    /// `StepBodyError::Failed`.
    #[test]
    fn mark_tombstone_fails_closed_on_unparseable_visibility() {
        use std::sync::atomic::AtomicU64;

        use cairn_core::domain::Identity;
        use cairn_core::domain::taxonomy::MemoryVisibility;
        use cairn_core::wal::{OperationId, WalKind};

        use crate::record_wal::ops::new_operation_id;
        use crate::record_wal::payload::ForgetPayload;

        let mut conn = crate::open::open_in_memory_sync().expect("open");
        let record = cairn_core::domain::record::tests_export::sample_record();

        let tx = conn.transaction().expect("tx upsert");
        let plan = crate::store::upsert::plan_upsert_in_tx(&tx, &record).expect("plan");
        crate::store::upsert::stage_upsert_cow_in_tx(&tx, &record, &plan).expect("stage");
        crate::store::upsert::activate_upsert_in_tx(&tx, &plan).expect("activate");
        // Stomp the visibility column with an invalid value.
        tx.execute(
            "UPDATE records SET visibility = 'not_a_real_tier' WHERE target_id = ?1",
            params![record.target_id.as_str()],
        )
        .expect("stomp visibility");
        tx.commit().expect("commit");

        let payload = ForgetPayload {
            target_id: record.target_id.clone(),
            scope: record.scope.clone(),
            reason_code: "user_command".to_owned(),
            actor: Identity::parse("hmn:test:v1").expect("identity"),
            scope_tier: MemoryVisibility::Private,
        };
        let op_id: OperationId = new_operation_id(WalKind::ForgetRecord).expect("op id");
        let cell = AtomicU64::new(0);
        let tx = conn.transaction().expect("tx forget");
        let result = mark_tombstone_and_emit_receipt(&tx, &op_id, &payload, &cell);
        match result {
            Err(StepBodyError::Failed(msg)) => {
                assert!(
                    msg.contains("visibility") && msg.contains("not_a_real_tier"),
                    "error must name the bad column + value; got: {msg}"
                );
            }
            other => {
                panic!("expected StepBodyError::Failed for unparseable visibility, got {other:?}")
            }
        }
    }

    /// Round-9 review (Codex): the parse step accepts any JSON that
    /// deserializes to `ScopeTuple`, but `{}` and `{"tenant":""}` etc.
    /// are syntactically valid yet domain-invalid (no IDL-addressable
    /// dimension; empty value). Without `ScopeTuple::validate()`,
    /// schema drift could let an irreversible forget commit with a
    /// receipt that lost the original scope. Pin the validation gate.
    #[test]
    fn mark_tombstone_fails_closed_on_invalid_scope_shape() {
        use std::sync::atomic::AtomicU64;

        use cairn_core::domain::Identity;
        use cairn_core::domain::taxonomy::MemoryVisibility;
        use cairn_core::wal::{OperationId, WalKind};

        use crate::record_wal::ops::new_operation_id;
        use crate::record_wal::payload::ForgetPayload;

        let mut conn = crate::open::open_in_memory_sync().expect("open");
        let record = cairn_core::domain::record::tests_export::sample_record();

        let tx = conn.transaction().expect("tx upsert");
        let plan = crate::store::upsert::plan_upsert_in_tx(&tx, &record).expect("plan");
        crate::store::upsert::stage_upsert_cow_in_tx(&tx, &record, &plan).expect("stage");
        crate::store::upsert::activate_upsert_in_tx(&tx, &plan).expect("activate");
        // Stomp scope with valid JSON but invalid domain shape: `{}`
        // deserializes to ScopeTuple::default() which has zero
        // IDL-addressable dimensions → ScopeTuple::validate() rejects.
        tx.execute(
            "UPDATE records SET scope = '{}' WHERE target_id = ?1",
            params![record.target_id.as_str()],
        )
        .expect("stomp scope");
        tx.commit().expect("commit");

        let payload = ForgetPayload {
            target_id: record.target_id.clone(),
            scope: record.scope.clone(),
            reason_code: "user_command".to_owned(),
            actor: Identity::parse("hmn:test:v1").expect("identity"),
            scope_tier: MemoryVisibility::Private,
        };
        let op_id: OperationId = new_operation_id(WalKind::ForgetRecord).expect("op id");
        let cell = AtomicU64::new(0);
        let tx = conn.transaction().expect("tx forget");
        let result = mark_tombstone_and_emit_receipt(&tx, &op_id, &payload, &cell);
        match result {
            Err(StepBodyError::Failed(msg)) => {
                assert!(
                    msg.contains("scope") && msg.contains("domain validation"),
                    "error must name scope + domain validation; got: {msg}"
                );
            }
            other => panic!("expected StepBodyError::Failed for invalid scope, got {other:?}"),
        }
    }
}
