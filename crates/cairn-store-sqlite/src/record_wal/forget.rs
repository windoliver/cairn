//! Public `forget_record` apply through record WAL.

use std::sync::Arc;

use cairn_core::contract::memory_store::ForgetReceipt;
use cairn_core::domain::taxonomy::MemoryVisibility;
use cairn_core::domain::{Identity, ScopeTuple, TargetId};
use cairn_core::wal::{OpState, WalKind, graph_for};
use rusqlite::OptionalExtension;
use sha2::{Digest, Sha256};

use crate::error::StoreError;
use crate::record_wal::locks::acquire_for_record;
use crate::record_wal::ops::{finalize, issue_prepared, new_operation_id};
use crate::record_wal::payload::{ForgetPayload, RecordWalPayload, save_payload};
use crate::record_wal::steps::RecordStepBody;
use crate::store::SqliteMemoryStore;
use crate::wal::runner::{self, StepBody};

pub(crate) async fn apply_forget_record(
    store: &SqliteMemoryStore,
    target: &TargetId,
    actor: &Identity,
) -> Result<ForgetReceipt, StoreError> {
    let conn = Arc::clone(store.require_conn("forget_record")?);
    let incarnation = store.incarnation().cloned().ok_or(StoreError::Invariant {
        what: "forget_record requires daemon incarnation".to_owned(),
    })?;
    let op_id = new_operation_id(WalKind::ForgetRecord)?;

    // Capture the live-version count BEFORE acquiring locks so an
    // idempotent re-forget reports `deleted_count = 0` (rows already
    // tombstoned + purged) while the first call reports the active
    // row count. Operators reading `deleted_count: 0` get the signal
    // they need to debug stale IDs / typos without us conflating it
    // with a hard failure — the WAL op still executes, idempotently
    // (brief §5.6 row 2). See `ForgetReceipt::deleted_count`.
    let deleted_count = count_active_rows(&conn, target).await?;

    let (scope, scope_tier) = load_scope_and_tier(&conn, target).await?;

    let locks = acquire_for_record(
        &conn,
        &scope,
        target,
        &incarnation,
        op_id.as_str(),
        "record_wal_forget_record",
    )
    .await
    .map_err(|e| StoreError::RecordWalLock(Box::new(e)))?;

    let payload = ForgetPayload {
        target_id: target.clone(),
        scope: scope.clone(),
        reason_code: "user_command".to_owned(),
        actor: actor.clone(),
        scope_tier,
    };
    let payload_for_body = payload.clone();
    let op_for_issue = op_id.clone();
    let target_hash = target.as_str().to_owned();

    conn.call(move |c| {
        let tx = c.transaction()?;
        issue_prepared(
            &tx,
            &op_for_issue,
            WalKind::ForgetRecord,
            &target_hash,
            "{}",
        )
        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        save_payload(
            &tx,
            &op_for_issue,
            &RecordWalPayload::Forget(Box::new(payload_for_body)),
        )
        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        tx.commit()?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await?;

    let body: Arc<dyn StepBody> = Arc::new(RecordStepBody::new_forget(payload, locks));
    runner::run_from(&conn, graph_for(WalKind::ForgetRecord), &op_id, 0, body).await?;

    let purged_at = crate::store::current_unix_ms();
    let op_for_finalize = op_id.clone();
    conn.call(move |c| {
        finalize(c, &op_for_finalize, OpState::Committed, "applied")
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await?;

    let mut hasher = Sha256::new();
    hasher.update(target.as_str().as_bytes());
    let target_id_hash = format!("sha256:{:x}", hasher.finalize());

    Ok(ForgetReceipt::new(
        target_id_hash,
        op_id.as_str().to_owned(),
        purged_at,
        deleted_count,
    ))
}

/// Read the count of active record rows for `target` without taking
/// the record-WAL locks. Called before `acquire_for_record` so an
/// idempotent re-forget — where every row is already tombstoned —
/// reports `0` rather than the historical version count.
async fn count_active_rows(
    conn: &Arc<tokio_rusqlite::Connection>,
    target: &TargetId,
) -> Result<u64, StoreError> {
    let target_id = target.as_str().to_owned();
    Ok(conn
        .call(move |c| {
            let count: i64 = c.query_row(
                "SELECT COUNT(*) FROM records WHERE target_id = ?1 AND active = 1",
                rusqlite::params![target_id],
                |row| row.get(0),
            )?;
            Ok(u64::try_from(count).unwrap_or(0))
        })
        .await?)
}

async fn load_scope_and_tier(
    conn: &Arc<tokio_rusqlite::Connection>,
    target: &TargetId,
) -> Result<(ScopeTuple, MemoryVisibility), StoreError> {
    let target_id = target.as_str().to_owned();
    Ok(conn
        .call(move |c| {
            let row: Option<(String, String)> = c
                .query_row(
                    "SELECT scope, visibility FROM records \
                       WHERE target_id = ?1 \
                       ORDER BY version DESC LIMIT 1",
                    rusqlite::params![target_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((scope_json, visibility)) = row else {
                return Ok((ScopeTuple::default(), MemoryVisibility::Private));
            };
            let scope: ScopeTuple = serde_json::from_str(&scope_json)
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
            let tier: MemoryVisibility =
                MemoryVisibility::parse(&visibility).unwrap_or(MemoryVisibility::Private);
            Ok((scope, tier))
        })
        .await?)
}
