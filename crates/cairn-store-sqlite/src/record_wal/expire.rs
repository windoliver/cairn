//! Public expire apply through record WAL.

use std::sync::Arc;

use cairn_core::domain::{ScopeTuple, TargetId};
use cairn_core::wal::{OpState, WalKind, graph_for};
use rusqlite::OptionalExtension;

use crate::error::StoreError;
use crate::record_wal::locks::acquire_for_record;
use crate::record_wal::ops::{finalize, issue_prepared, new_operation_id};
use crate::record_wal::payload::{ExpirePayload, RecordWalPayload, save_payload};
use crate::record_wal::steps::RecordStepBody;
use crate::store::SqliteMemoryStore;
use crate::wal::runner::{self, StepBody};

pub(crate) async fn apply_expire(
    store: &SqliteMemoryStore,
    target: &TargetId,
) -> Result<(), StoreError> {
    let conn = Arc::clone(store.require_conn("expire")?);
    let incarnation = store.incarnation().cloned().ok_or(StoreError::Invariant {
        what: "expire requires daemon incarnation".to_owned(),
    })?;
    let op_id = new_operation_id(WalKind::Expire)?;
    let scope = load_scope_for_target(&conn, target).await?;
    let locks = acquire_for_record(
        &conn,
        &scope,
        target,
        &incarnation,
        op_id.as_str(),
        "record_wal_expire",
    )
    .await
    .map_err(|e| StoreError::RecordWalLock(Box::new(e)))?;

    let payload = ExpirePayload {
        target_id: target.clone(),
        reason: "expire".to_owned(),
    };
    let payload_for_body = payload.clone();
    let op_for_issue = op_id.clone();
    let target_hash = target.as_str().to_owned();
    conn.call(move |c| {
        let tx = c.transaction()?;
        issue_prepared(&tx, &op_for_issue, WalKind::Expire, &target_hash, "{}")
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        save_payload(
            &tx,
            &op_for_issue,
            &RecordWalPayload::Expire(payload_for_body),
        )
        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        tx.commit()?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await?;

    let body: Arc<dyn StepBody> = Arc::new(RecordStepBody::new_expire(payload, locks));
    runner::run_from(&conn, graph_for(WalKind::Expire), &op_id, 0, body).await?;

    let op_for_finalize = op_id.clone();
    conn.call(move |c| {
        finalize(c, &op_for_finalize, OpState::Committed, "applied")
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await?;
    Ok(())
}

async fn load_scope_for_target(
    conn: &Arc<tokio_rusqlite::Connection>,
    target: &TargetId,
) -> Result<ScopeTuple, StoreError> {
    let target_id = target.as_str().to_owned();
    Ok(conn
        .call(move |c| {
            let scope_json: Option<String> = c
                .query_row(
                    "SELECT scope FROM records WHERE target_id = ?1 ORDER BY version DESC LIMIT 1",
                    rusqlite::params![target_id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(scope_json) = scope_json else {
                return Ok(ScopeTuple::default());
            };
            serde_json::from_str(&scope_json).map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
        })
        .await?)
}
