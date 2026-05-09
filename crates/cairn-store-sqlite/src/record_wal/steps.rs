//! Record WAL step bodies.

use cairn_core::domain::RecordId;
use cairn_core::wal::{OperationId, StepDef};
use rusqlite::{Transaction, params};

use crate::record_wal::locks::RecordLocks;
use crate::record_wal::payload::{ExpirePayload, StoredEmbedOutcome, UpsertPayload};
use crate::store::upsert::upsert_in_tx_with_record_id;
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
        _op_id: &OperationId,
        step: &StepDef,
    ) -> Result<(), StepBodyError> {
        self.locks
            .assert_live_in_tx(tx)
            .map_err(|e| StepBodyError::Failed(format!("fenced: {e}")))?;

        match (&self.payload, step.name) {
            (RecordStepPayload::Upsert(payload), "primary.upsert_cow") => {
                let planned_record_id = RecordId::parse(payload.planned.outcome_record_id.clone())
                    .map_err(|e| StepBodyError::Failed(format!("planned record_id: {e}")))?;
                let outcome =
                    upsert_in_tx_with_record_id(tx, &payload.record, Some(&planned_record_id))
                        .map_err(|e| StepBodyError::Failed(e.to_string()))?;
                apply_embed_outcome(tx, outcome.record_id.as_str(), &payload.embed)
                    .map_err(StepBodyError::Storage)?;
                Ok(())
            }
            (RecordStepPayload::Upsert(_), _) => Ok(()),
            (RecordStepPayload::Expire(_), _) => Ok(()),
        }
    }
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
