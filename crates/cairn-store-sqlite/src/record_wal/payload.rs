//! Durable JSON payloads for record WAL operations.

use cairn_core::domain::{
    BodyHash, Identity, MemoryRecord, MemoryVisibility, RecordId, ScopeTuple, TargetId,
};
use cairn_core::wal::{OperationId, WalKind};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::error::StoreError;
use crate::store::current_unix_ms;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RecordWalPayload {
    Upsert(Box<UpsertPayload>),
    Expire(Box<ExpirePayload>),
    #[serde(rename = "forget_record")]
    Forget(Box<ForgetPayload>),
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

impl PlannedUpsert {
    pub(crate) fn to_store_plan(&self) -> Result<crate::store::upsert::UpsertPlan, StoreError> {
        Ok(crate::store::upsert::UpsertPlan {
            outcome_record_id: RecordId::parse(self.outcome_record_id.clone()).map_err(|e| {
                StoreError::Invariant {
                    what: format!("planned record id invalid: {e}"),
                }
            })?,
            target_id: TargetId::parse(self.target_id.clone()).map_err(|e| {
                StoreError::Invariant {
                    what: format!("planned target id invalid: {e}"),
                }
            })?,
            version: self.version,
            content_changed: self.content_changed,
            prior_record_id: self.prior_record_id.clone(),
            prior_hash: self
                .prior_hash
                .as_ref()
                .map(BodyHash::parse)
                .transpose()
                .map_err(|e| StoreError::Invariant {
                    what: format!("planned prior hash invalid: {e}"),
                })?,
            consent_model: self.consent_model.clone(),
        })
    }
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
    #[serde(default)]
    pub scope: ScopeTuple,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ForgetPayload {
    pub target_id: TargetId,
    #[serde(default)]
    pub scope: ScopeTuple,
    pub reason_code: String,
    pub actor: Identity,
    pub scope_tier: MemoryVisibility,
}

pub(crate) fn save_payload(
    conn: &Connection,
    op_id: &OperationId,
    payload: &RecordWalPayload,
) -> Result<(), StoreError> {
    let kind = match payload {
        RecordWalPayload::Upsert(_) => WalKind::Upsert.as_str(),
        RecordWalPayload::Expire(_) => WalKind::Expire.as_str(),
        RecordWalPayload::Forget(_) => WalKind::ForgetRecord.as_str(),
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
    save_payload(
        conn,
        &op,
        &RecordWalPayload::Upsert(Box::new(payload.clone())),
    )
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
        RecordWalPayload::Upsert(payload) => Ok(*payload),
        RecordWalPayload::Expire(_) => Err(StoreError::Invariant {
            what: "expected upsert payload, found expire payload".to_owned(),
        }),
        RecordWalPayload::Forget(_) => Err(StoreError::Invariant {
            what: "expected upsert payload, found forget payload".to_owned(),
        }),
    }
}

#[cfg(test)]
mod forget_payload_tests {
    use super::*;

    #[test]
    fn forget_payload_round_trips_through_record_wal_payload() {
        let target =
            TargetId::parse("01HQZX9F5N0000000000000000".to_owned()).expect("valid target id");
        let actor = Identity::parse("hmn:alice:v1".to_owned()).expect("valid identity");
        let payload = ForgetPayload {
            target_id: target.clone(),
            scope: ScopeTuple::default(),
            reason_code: "user_command".to_owned(),
            actor: actor.clone(),
            scope_tier: MemoryVisibility::Private,
        };
        let wrapped = RecordWalPayload::Forget(Box::new(payload.clone()));
        let json = serde_json::to_string(&wrapped).expect("serialize");
        assert!(
            json.contains("\"type\":\"forget_record\""),
            "wire tag should match wal_payloads.kind"
        );
        let decoded: RecordWalPayload = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            RecordWalPayload::Forget(p) => {
                assert_eq!(p.target_id, target);
                assert_eq!(p.actor, actor);
                assert_eq!(p.scope_tier, MemoryVisibility::Private);
                assert_eq!(p.reason_code, "user_command");
            }
            _ => panic!("expected Forget variant after round trip"),
        }
    }
}
