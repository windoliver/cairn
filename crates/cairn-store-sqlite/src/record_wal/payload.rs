//! Durable JSON payloads for record WAL operations.

use cairn_core::domain::{MemoryRecord, TargetId};
use cairn_core::wal::{OperationId, WalKind};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::error::StoreError;
use crate::store::current_unix_ms;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RecordWalPayload {
    Upsert(UpsertPayload),
    Expire(ExpirePayload),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UpsertPayload {
    pub record: MemoryRecord,
    pub embed: StoredEmbedOutcome,
    pub planned: PlannedUpsert,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlannedUpsert {
    pub outcome_record_id: String,
    pub target_id: String,
    pub version: u32,
    pub content_changed: bool,
    pub prior_record_id: Option<String>,
    pub prior_hash: Option<String>,
    pub consent_model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StoredEmbedOutcome {
    Succeeded {
        vector: Vec<u8>,
        model_label: String,
    },
    Failed {
        error: String,
    },
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExpirePayload {
    pub target_id: TargetId,
    pub reason: String,
}

pub(crate) fn save_payload(
    conn: &Connection,
    op_id: &OperationId,
    payload: &RecordWalPayload,
) -> Result<(), StoreError> {
    let kind = match payload {
        RecordWalPayload::Upsert(_) => WalKind::Upsert.as_str(),
        RecordWalPayload::Expire(_) => WalKind::Expire.as_str(),
    };
    let json = serde_json::to_string(payload)?;
    conn.execute(
        "INSERT INTO wal_payloads(operation_id, kind, payload_json, created_at) \
         VALUES (?1, ?2, ?3, ?4)",
        params![op_id.as_str(), kind, json, current_unix_ms()],
    )?;
    Ok(())
}

pub(crate) fn load_payload(
    conn: &Connection,
    op_id: &OperationId,
) -> Result<RecordWalPayload, StoreError> {
    let json: String = conn
        .query_row(
            "SELECT payload_json FROM wal_payloads WHERE operation_id = ?1",
            params![op_id.as_str()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| StoreError::Invariant {
            what: format!("missing wal_payloads row for operation {}", op_id.as_str()),
        })?;
    Ok(serde_json::from_str(&json)?)
}

#[cfg(any(test, feature = "test-helpers"))]
impl UpsertPayload {
    #[must_use]
    pub fn new_for_test(record: MemoryRecord) -> Self {
        Self {
            planned: PlannedUpsert {
                outcome_record_id: record.id.as_str().to_owned(),
                target_id: record.target_id.as_str().to_owned(),
                version: 1,
                content_changed: true,
                prior_record_id: None,
                prior_hash: None,
                consent_model: "legacy_event".to_owned(),
            },
            record,
            embed: StoredEmbedOutcome::Skipped,
        }
    }
}

#[cfg(any(test, feature = "test-helpers"))]
pub fn save_upsert_payload_for_test(
    conn: &Connection,
    op_id: &str,
    payload: &UpsertPayload,
) -> Result<(), StoreError> {
    let op = OperationId::parse(op_id.to_owned()).map_err(|e| StoreError::Invariant {
        what: format!("invalid test op id: {e}"),
    })?;
    save_payload(conn, &op, &RecordWalPayload::Upsert(payload.clone()))
}

#[cfg(any(test, feature = "test-helpers"))]
pub fn load_upsert_payload_for_test(
    conn: &Connection,
    op_id: &str,
) -> Result<UpsertPayload, StoreError> {
    let op = OperationId::parse(op_id.to_owned()).map_err(|e| StoreError::Invariant {
        what: format!("invalid test op id: {e}"),
    })?;
    match load_payload(conn, &op)? {
        RecordWalPayload::Upsert(payload) => Ok(payload),
        RecordWalPayload::Expire(_) => Err(StoreError::Invariant {
            what: "expected upsert payload, found expire payload".to_owned(),
        }),
    }
}
