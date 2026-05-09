//! Public upsert apply through record WAL.

use std::sync::Arc;

use cairn_core::contract::memory_store::UpsertOutcome;
use cairn_core::domain::{BodyHash, MemoryRecord, RecordId};
use cairn_core::wal::{OpState, WalKind, graph_for};

use crate::error::StoreError;
use crate::record_wal::locks::acquire_for_record;
use crate::record_wal::ops::{finalize, issue_prepared, new_operation_id};
use crate::record_wal::payload::{
    PlannedUpsert, RecordWalPayload, StoredEmbedOutcome, UpsertPayload, save_payload,
};
use crate::record_wal::steps::RecordStepBody;
use crate::store::SqliteMemoryStore;
use crate::store::upsert::upsert_in_tx;
use crate::wal::runner::{self, StepBody};

pub(crate) async fn apply_upsert(
    store: &SqliteMemoryStore,
    record: &MemoryRecord,
    embed: StoredEmbedOutcome,
) -> Result<UpsertOutcome, StoreError> {
    let conn = Arc::clone(store.require_conn("upsert")?);
    let incarnation = store.incarnation().cloned().ok_or(StoreError::Invariant {
        what: "upsert requires daemon incarnation".to_owned(),
    })?;
    let op_id = new_operation_id(WalKind::Upsert)?;
    let locks = acquire_for_record(
        &conn,
        &record.scope,
        &record.target_id,
        &incarnation,
        op_id.as_str(),
        "record_wal_upsert",
    )
    .await
    .map_err(|e| StoreError::RecordWalLock(Box::new(e)))?;

    let record_for_plan = record.clone();
    let record_for_payload = record.clone();
    let planned = conn
        .call(move |c| {
            let mut tx = c.transaction()?;
            let out = upsert_in_tx(&mut tx, &record_for_plan)
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
            tx.rollback()?;
            Ok::<_, tokio_rusqlite::Error>(PlannedUpsert {
                outcome_record_id: out.record_id.as_str().to_owned(),
                target_id: out.target_id.as_str().to_owned(),
                version: out.version,
                content_changed: out.content_changed,
                prior_record_id: None,
                prior_hash: out.prior_hash.map(|h| h.to_string()),
                consent_model: "legacy_event".to_owned(),
            })
        })
        .await?;

    let payload = UpsertPayload {
        record: record_for_payload,
        embed,
        planned,
    };
    let payload_for_body = payload.clone();
    let op_for_issue = op_id.clone();
    conn.call(move |c| {
        let tx = c.transaction()?;
        issue_prepared(
            &tx,
            &op_for_issue,
            WalKind::Upsert,
            &payload_for_body.planned.target_id,
            "{}",
        )
        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        save_payload(
            &tx,
            &op_for_issue,
            &RecordWalPayload::Upsert(payload_for_body),
        )
        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        tx.commit()?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await?;

    let planned_for_outcome = payload.planned.clone();
    let body: Arc<dyn StepBody> = Arc::new(RecordStepBody::new_upsert(payload, locks));
    runner::run_from(&conn, graph_for(WalKind::Upsert), &op_id, 0, body).await?;

    let op_for_finalize = op_id.clone();
    conn.call(move |c| {
        finalize(c, &op_for_finalize, OpState::Committed, "applied")
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await?;

    Ok(UpsertOutcome {
        record_id: RecordId::parse(planned_for_outcome.outcome_record_id.clone()).map_err(|e| {
            StoreError::Invariant {
                what: format!("planned record_id invalid: {e}"),
            }
        })?,
        target_id: record.target_id.clone(),
        version: planned_for_outcome.version,
        content_changed: planned_for_outcome.content_changed,
        prior_hash: planned_for_outcome
            .prior_hash
            .map(BodyHash::parse)
            .transpose()
            .map_err(|e| StoreError::Invariant {
                what: format!("planned prior_hash invalid: {e}"),
            })?,
    })
}
