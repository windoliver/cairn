//! Record WAL step bodies.

use cairn_core::wal::{OperationId, StepDef};
use rusqlite::{Transaction, params};

use crate::record_wal::locks::RecordLocks;
use crate::record_wal::payload::{ExpirePayload, StoredEmbedOutcome, UpsertPayload};
use crate::wal::runner::{StepBody, StepBodyError};

pub(crate) enum RecordStepPayload {
    Upsert(UpsertPayload),
    Expire(ExpirePayload),
}

pub(crate) struct RecordStepBody {
    payload: RecordStepPayload,
    locks: RecordLocks,
}

impl RecordStepBody {
    #[must_use]
    pub(crate) fn new_upsert(payload: UpsertPayload, locks: RecordLocks) -> Self {
        Self {
            payload: RecordStepPayload::Upsert(payload),
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
                    .map_err(|e| StepBodyError::Failed(e.to_string()))?;
                apply_embed_outcome(tx, plan.outcome_record_id.as_str(), &payload.embed)
                    .map_err(StepBodyError::Storage)?;
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
            (RecordStepPayload::Upsert(_), _) => Ok(()),
            (RecordStepPayload::Expire(_), _) => Ok(()),
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

fn apply_embed_outcome(
    tx: &Transaction<'_>,
    record_id: &str,
    embed: &StoredEmbedOutcome,
) -> Result<(), rusqlite::Error> {
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
            )?;
            tx.execute(
                "INSERT INTO record_vectors(record_id, embedding, model) \
                   VALUES (?, ?, ?)",
                params![record_id, vector, model_label],
            )?;
            tx.execute(
                "DELETE FROM pending_embeddings WHERE record_id = ?",
                params![record_id],
            )?;
        }
        StoredEmbedOutcome::Failed { error } => {
            let now_secs = now_secs();
            tx.execute(
                "INSERT INTO pending_embeddings \
                     (record_id, reason, attempt_count, last_error, enqueued_at) \
                   VALUES (?, 'embed_failed', 0, ?, ?) \
                   ON CONFLICT(record_id) DO UPDATE \
                     SET attempt_count   = attempt_count + 1, \
                         last_error      = excluded.last_error, \
                         last_attempt_at = ?",
                params![record_id, error, now_secs, now_secs],
            )?;
        }
        StoredEmbedOutcome::Skipped => {}
    }
    Ok(())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}
