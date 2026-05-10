//! Public record-level forget apply through record WAL.

use std::sync::Arc;

use cairn_core::domain::{RecordId, ScopeTuple, TargetId};
use rusqlite::{OptionalExtension, params};

use crate::error::StoreError;
use crate::store::SqliteMemoryStore;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgetTarget {
    pub requested_record_id: RecordId,
    pub target_id: TargetId,
    pub scope: ScopeTuple,
    pub record_ids: Vec<RecordId>,
    pub target_hash: String,
}

async fn resolve_forget_target(
    store: &SqliteMemoryStore,
    record_id: &RecordId,
) -> Result<ForgetTarget, StoreError> {
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
        let target_id = TargetId::parse(target_raw.clone())
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(StoreError::InvalidRecord(e))))?;
        let scope: ScopeTuple = serde_json::from_str(&scope_json)
            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;

        let mut stmt = c.prepare(
            "SELECT record_id \
               FROM records \
              WHERE target_id = ?1 \
              ORDER BY version ASC, record_id ASC",
        )?;
        let ids = stmt
            .query_map(params![target_id.as_str()], |row| row.get::<_, String>(0))?
            .map(|row| {
                row.and_then(|raw| {
                    RecordId::parse(raw).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })
                })
            })
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let target_hash = hash_target_id(target_id.as_str());
        Ok::<_, tokio_rusqlite::Error>(ForgetTarget {
            requested_record_id: requested,
            target_id,
            scope,
            record_ids: ids,
            target_hash,
        })
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
