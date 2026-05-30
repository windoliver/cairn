//! Public record-level forget apply through record WAL.

use std::sync::Arc;
use std::time::Instant;

use cairn_core::domain::{RecordId, ScopeTuple, TargetId};
use cairn_core::wal::{OpState, OperationId, WalKind, graph_for};
use rusqlite::{OptionalExtension, params};

use crate::error::StoreError;
use crate::record_wal::locks::acquire_for_record;
use crate::record_wal::ops::{finalize, issue_prepared, new_operation_id};
use crate::record_wal::payload::{ForgetPayload, RecordWalPayload, save_payload};
use crate::record_wal::steps::RecordStepBody;
use crate::store::SqliteMemoryStore;
use crate::wal::runner::{self, StepBody};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgetTarget {
    pub requested_record_id: RecordId,
    pub target_id: TargetId,
    pub scope: ScopeTuple,
    pub record_ids: Vec<RecordId>,
    pub target_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ForgetTargetSeed {
    requested_record_id: RecordId,
    target_id: TargetId,
    scope: ScopeTuple,
    target_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgetOutcome {
    pub deleted_count: u64,
    pub tombstones: Vec<RecordId>,
    pub operation_id: OperationId,
}

pub(crate) async fn apply_forget_record(
    store: &SqliteMemoryStore,
    record_id: &RecordId,
) -> Result<ForgetOutcome, StoreError> {
    let started = Instant::now();
    let conn = Arc::clone(store.require_conn("forget_record")?);
    // Hold the vault write gate SHARED for the whole forget. A restore holds
    // the gate EXCLUSIVE across its purge→swap, so this blocks until any
    // in-flight restore finishes — and conversely makes a restore wait for an
    // in-flight forget — guaranteeing a forget can never commit into the DB
    // that a restore is about to discard, which would resurrect the record
    // (round-7 review #2). Released when `_write_gate` drops at function end.
    let _write_gate = store.acquire_write_gate_shared().await?;
    let incarnation = store.incarnation().cloned().ok_or(StoreError::Invariant {
        what: "forget_record requires daemon incarnation".to_owned(),
    })?;
    let target_seed = resolve_forget_target_seed(store, record_id).await?;
    let op_id = new_operation_id(WalKind::ForgetRecord)?;
    let locks = acquire_for_record(
        &conn,
        &target_seed.scope,
        &target_seed.target_id,
        &incarnation,
        op_id.as_str(),
        "record_wal_forget_record",
    )
    .await
    .map_err(|e| StoreError::RecordWalLock(Box::new(e)))?;

    let record_ids = load_forget_lineage(
        &conn,
        &target_seed.target_id,
        &target_seed.requested_record_id,
    )
    .await?;
    let target = ForgetTarget {
        requested_record_id: target_seed.requested_record_id,
        target_id: target_seed.target_id,
        scope: target_seed.scope,
        record_ids,
        target_hash: target_seed.target_hash,
    };

    let payload = ForgetPayload {
        requested_record_id: target.requested_record_id.clone(),
        target_id: target.target_id.clone(),
        scope: target.scope.clone(),
        record_ids: target.record_ids.clone(),
        target_hash: target.target_hash.clone(),
        reason_code: "user_command".to_owned(),
    };

    let payload_for_body = payload.clone();
    let op_for_issue = op_id.clone();
    let target_hash = target.target_hash.clone();
    let scope_wire = target.scope.canonical_wire();
    conn.call(move |c| {
        let tx = c.transaction()?;
        issue_prepared(
            &tx,
            &op_for_issue,
            WalKind::ForgetRecord,
            &target_hash,
            &scope_wire,
        )
        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        save_payload(
            &tx,
            &op_for_issue,
            &RecordWalPayload::ForgetRecord(Box::new(payload_for_body)),
        )
        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        tx.commit()?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .map_err(unpack_worker_err)?;

    let body: Arc<dyn StepBody> = Arc::new(RecordStepBody::new_forget_record(payload, locks));
    runner::run_from(&conn, graph_for(WalKind::ForgetRecord), &op_id, 0, body).await?;

    let op_for_finalize = op_id.clone();
    conn.call(move |c| {
        finalize(c, &op_for_finalize, OpState::Committed, "applied")
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .map_err(unpack_worker_err)?;

    tracing::info!(
        wal.kind = WalKind::ForgetRecord.as_str(),
        operation_id = op_id.as_str(),
        state = OpState::Committed.as_str(),
        latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        retry_count = 0_u32,
        "record WAL apply complete"
    );

    Ok(ForgetOutcome {
        deleted_count: u64::try_from(target.record_ids.len()).unwrap_or(u64::MAX),
        tombstones: target.record_ids,
        operation_id: op_id,
    })
}

async fn resolve_forget_target(
    store: &SqliteMemoryStore,
    record_id: &RecordId,
) -> Result<ForgetTarget, StoreError> {
    let conn = Arc::clone(store.require_conn("forget_record")?);
    let seed = resolve_forget_target_seed(store, record_id).await?;
    let record_ids = load_forget_lineage(&conn, &seed.target_id, &seed.requested_record_id).await?;
    Ok(ForgetTarget {
        requested_record_id: seed.requested_record_id,
        target_id: seed.target_id,
        scope: seed.scope,
        record_ids,
        target_hash: seed.target_hash,
    })
}

async fn resolve_forget_target_seed(
    store: &SqliteMemoryStore,
    record_id: &RecordId,
) -> Result<ForgetTargetSeed, StoreError> {
    let conn = Arc::clone(store.require_conn("forget_record")?);
    let requested = record_id.clone();
    conn.call(move |c| {
        let row: Option<(String, String)> = c
            .query_row(
                "SELECT target_id, scope \
                   FROM records \
                  WHERE record_id = ?1 \
                  ORDER BY version DESC \
                  LIMIT 1",
                params![requested.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((target_raw, scope_json)) = row else {
            return Err(tokio_rusqlite::Error::Other(Box::new(
                StoreError::NotFound {
                    id: requested.as_str().to_owned(),
                },
            )));
        };
        let target_id = TargetId::parse(target_raw.clone()).map_err(|e| {
            tokio_rusqlite::Error::Other(Box::new(StoreError::Invariant {
                what: format!("invalid target_id `{target_raw}` in forget target: {e}"),
            }))
        })?;
        let scope: ScopeTuple = serde_json::from_str(&scope_json)
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(StoreError::Codec(e))))?;
        let target_hash = hash_target_id(target_id.as_str());
        Ok::<_, tokio_rusqlite::Error>(ForgetTargetSeed {
            requested_record_id: requested,
            target_id,
            scope,
            target_hash,
        })
    })
    .await
    .map_err(unpack_worker_err)
}

async fn load_forget_lineage(
    conn: &Arc<tokio_rusqlite::Connection>,
    target_id: &TargetId,
    requested: &RecordId,
) -> Result<Vec<RecordId>, StoreError> {
    let target = target_id.as_str().to_owned();
    let requested = requested.clone();
    conn.call(move |c| {
        let mut stmt = c.prepare(
            "SELECT record_id \
               FROM records \
              WHERE target_id = ?1 \
              ORDER BY version ASC, record_id ASC",
        )?;
        let raw_ids = stmt
            .query_map(params![target], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if raw_ids.is_empty() {
            return Err(tokio_rusqlite::Error::Other(Box::new(
                StoreError::NotFound {
                    id: requested.as_str().to_owned(),
                },
            )));
        }
        raw_ids
            .into_iter()
            .map(|raw| {
                RecordId::parse(raw.clone()).map_err(|e| StoreError::Invariant {
                    what: format!("invalid record_id `{raw}` in forget lineage: {e}"),
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
    })
    .await
    .map_err(unpack_worker_err)
}

fn hash_target_id(target_id: &str) -> String {
    format!(
        "hash:{}",
        cairn_core::domain::projection::body_hash(&format!("forget_record:{target_id}"))
    )
}

fn unpack_worker_err(err: tokio_rusqlite::Error) -> StoreError {
    match err {
        tokio_rusqlite::Error::Other(boxed) => match boxed.downcast::<StoreError>() {
            Ok(inner) => *inner,
            Err(other) => StoreError::Worker(tokio_rusqlite::Error::Other(other)),
        },
        other => StoreError::from(other),
    }
}

#[cfg(any(test, feature = "test-helpers"))]
pub async fn resolve_forget_target_for_test(
    store: &SqliteMemoryStore,
    record_id: &RecordId,
) -> Result<ForgetTarget, StoreError> {
    resolve_forget_target(store, record_id).await
}
