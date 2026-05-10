//! Record WAL step bodies.

use cairn_core::domain::{
    ConsentEvent, ConsentKind, ConsentPayload, Identity, MemoryVisibility, Rfc3339Timestamp,
};
use cairn_core::wal::{OperationId, StepDef};
use rusqlite::{Transaction, params};

use crate::record_wal::locks::RecordLocks;
use crate::record_wal::payload::{
    ExpirePayload, ForgetPayload, PurgedPayload, RecordWalPayload, StoredEmbedOutcome,
    UpsertPayload,
};
use crate::wal::runner::{StepBody, StepBodyError};

pub(crate) enum RecordStepPayload {
    Upsert(Box<UpsertPayload>),
    Expire(Box<ExpirePayload>),
    ForgetRecord(Box<ForgetPayload>),
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
    pub(crate) fn new_forget_record(payload: ForgetPayload, locks: RecordLocks) -> Self {
        Self {
            payload: RecordStepPayload::ForgetRecord(Box::new(payload)),
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
            (RecordStepPayload::Expire(payload), "vector.drain") => drain_vectors(tx, payload),
            (RecordStepPayload::Expire(payload), "fts.drain") => drain_fts(tx, payload),
            (RecordStepPayload::Expire(payload), "edges.drain") => drain_edges(tx, payload),
            (RecordStepPayload::ForgetRecord(payload), "primary.mark_tombstone") => {
                mark_forget_tombstone(tx, op_id, payload)
            }
            (RecordStepPayload::ForgetRecord(payload), "vector.drain") => {
                drain_forget_vectors(tx, payload)
            }
            (RecordStepPayload::ForgetRecord(payload), "fts.drain") => {
                drain_forget_fts(tx, payload)
            }
            (RecordStepPayload::ForgetRecord(payload), "edges.drain") => {
                drain_forget_edges(tx, payload)
            }
            (RecordStepPayload::ForgetRecord(payload), "primary.purge") => {
                purge_forget_primary(tx, payload)
            }
            (RecordStepPayload::ForgetRecord(payload), "wal.purge_pre_images") => {
                scrub_forget_wal(tx, op_id, payload)
            }
            (RecordStepPayload::ForgetRecord(_), "snapshot.purge")
            | (
                RecordStepPayload::Upsert(_)
                | RecordStepPayload::Expire(_)
                | RecordStepPayload::ForgetRecord(_),
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

fn drain_vectors(tx: &Transaction<'_>, payload: &ExpirePayload) -> Result<(), StepBodyError> {
    tx.execute(
        "DELETE FROM record_vectors \
          WHERE record_id IN (SELECT record_id FROM records WHERE target_id = ?1)",
        params![payload.target_id.as_str()],
    )
    .map_err(StepBodyError::Storage)?;
    tx.execute(
        "DELETE FROM pending_embeddings \
          WHERE record_id IN (SELECT record_id FROM records WHERE target_id = ?1)",
        params![payload.target_id.as_str()],
    )
    .map_err(StepBodyError::Storage)?;
    Ok(())
}

fn drain_fts(tx: &Transaction<'_>, payload: &ExpirePayload) -> Result<(), StepBodyError> {
    let rowids = {
        let mut stmt = tx
            .prepare("SELECT rowid FROM records WHERE target_id = ?1")
            .map_err(StepBodyError::Storage)?;
        stmt.query_map(params![payload.target_id.as_str()], |row| {
            row.get::<_, i64>(0)
        })
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

fn drain_edges(tx: &Transaction<'_>, payload: &ExpirePayload) -> Result<(), StepBodyError> {
    tx.execute(
        "DELETE FROM edges \
          WHERE src IN (SELECT record_id FROM records WHERE target_id = ?1) \
             OR dst IN (SELECT record_id FROM records WHERE target_id = ?1)",
        params![payload.target_id.as_str()],
    )
    .map_err(StepBodyError::Storage)?;
    Ok(())
}

fn mark_forget_tombstone(
    tx: &Transaction<'_>,
    op_id: &OperationId,
    payload: &ForgetPayload,
) -> Result<(), StepBodyError> {
    tx.execute(
        "UPDATE records \
            SET active = 0, \
                tombstoned = 1, \
                tombstone_reason = 'forget', \
                updated_at = ?1 \
          WHERE target_id = ?2",
        params![crate::store::current_unix_ms(), payload.target_id.as_str()],
    )
    .map_err(StepBodyError::Storage)?;
    append_forget_consent(tx, op_id, payload)
}

fn append_forget_consent(
    tx: &Transaction<'_>,
    op_id: &OperationId,
    payload: &ForgetPayload,
) -> Result<(), StepBodyError> {
    let actor =
        Identity::parse("hmn:cairn-cli").map_err(|e| StepBodyError::Failed(e.to_string()))?;
    let decided_at = Rfc3339Timestamp::from_unix_secs(crate::store::current_unix_ms() / 1000)
        .map_err(|e| StepBodyError::Failed(e.to_string()))?;
    let event = ConsentEvent {
        consent_id: ulid::Ulid::new().to_string(),
        kind: ConsentKind::ForgetIntent,
        actor,
        subject: payload.target_hash.clone(),
        scope: payload.scope.canonical_wire(),
        op_id: Some(op_id.as_str().to_owned()),
        sensor_id: None,
        payload: ConsentPayload::IntentReceipt {
            target_id_hash: payload.target_hash.clone(),
            scope_tier: MemoryVisibility::Private,
            reason_code: payload.reason_code.clone(),
        },
        decided_at,
        expires_at: None,
    };
    crate::consent::append(tx, &event).map_err(|e| StepBodyError::Failed(e.to_string()))?;
    Ok(())
}

fn drain_forget_vectors(
    tx: &Transaction<'_>,
    payload: &ForgetPayload,
) -> Result<(), StepBodyError> {
    tx.execute(
        "DELETE FROM record_vectors \
          WHERE record_id IN (SELECT record_id FROM records WHERE target_id = ?1)",
        params![payload.target_id.as_str()],
    )
    .map_err(StepBodyError::Storage)?;
    tx.execute(
        "DELETE FROM pending_embeddings \
          WHERE record_id IN (SELECT record_id FROM records WHERE target_id = ?1)",
        params![payload.target_id.as_str()],
    )
    .map_err(StepBodyError::Storage)?;
    Ok(())
}

fn drain_forget_fts(tx: &Transaction<'_>, payload: &ForgetPayload) -> Result<(), StepBodyError> {
    let rowids = {
        let mut stmt = tx
            .prepare("SELECT rowid FROM records WHERE target_id = ?1")
            .map_err(StepBodyError::Storage)?;
        stmt.query_map(params![payload.target_id.as_str()], |row| {
            row.get::<_, i64>(0)
        })
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

fn drain_forget_edges(tx: &Transaction<'_>, payload: &ForgetPayload) -> Result<(), StepBodyError> {
    tx.execute(
        "DELETE FROM edges \
          WHERE src IN (SELECT record_id FROM records WHERE target_id = ?1) \
             OR dst IN (SELECT record_id FROM records WHERE target_id = ?1)",
        params![payload.target_id.as_str()],
    )
    .map_err(StepBodyError::Storage)?;
    tx.execute(
        "DELETE FROM entity_episodes \
          WHERE episode_id IN (SELECT record_id FROM records WHERE target_id = ?1)",
        params![payload.target_id.as_str()],
    )
    .map_err(StepBodyError::Storage)?;
    Ok(())
}

fn purge_forget_primary(
    tx: &Transaction<'_>,
    payload: &ForgetPayload,
) -> Result<(), StepBodyError> {
    tx.execute(
        "DELETE FROM records WHERE target_id = ?1",
        params![payload.target_id.as_str()],
    )
    .map_err(StepBodyError::Storage)?;
    Ok(())
}

fn scrub_forget_wal(
    tx: &Transaction<'_>,
    op_id: &OperationId,
    payload: &ForgetPayload,
) -> Result<(), StepBodyError> {
    let stub = serde_json::to_string(&RecordWalPayload::Purged(Box::new(PurgedPayload {
        target_hash: payload.target_hash.clone(),
        purged_by: op_id.as_str().to_owned(),
        purged_at: crate::store::current_unix_ms(),
    })))
    .map_err(|e| StepBodyError::Failed(format!("purged payload json: {e}")))?;
    let stub_bytes = stub.as_bytes();
    let mut needles = Vec::with_capacity(payload.record_ids.len() + 1);
    needles.push(payload.target_id.as_str().to_owned());
    needles.extend(
        payload
            .record_ids
            .iter()
            .map(|record_id| record_id.as_str().to_owned()),
    );

    for needle in needles {
        let like = format!("%{needle}%");
        tx.execute(
            "UPDATE wal_steps \
                SET pre_image = ?1 \
              WHERE operation_id <> ?2 \
                AND pre_image IS NOT NULL \
                AND CAST(pre_image AS TEXT) LIKE ?3",
            params![stub_bytes, op_id.as_str(), like],
        )
        .map_err(StepBodyError::Storage)?;
        tx.execute(
            "UPDATE wal_payloads \
                SET kind = 'purged', payload_json = ?1 \
              WHERE operation_id <> ?2 \
                AND kind IN ('upsert', 'expire') \
                AND payload_json LIKE ?3",
            params![stub, op_id.as_str(), like],
        )
        .map_err(StepBodyError::Storage)?;
    }
    Ok(())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
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
