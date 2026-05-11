//! Record WAL recovery registry.

use std::sync::Arc;

use cairn_core::wal::{OperationId, WalKind};
use tokio_rusqlite::Connection;

use crate::record_wal::locks::acquire_for_record;
use crate::record_wal::payload::{RecordWalPayload, load_payload};
use crate::record_wal::steps::RecordStepBody;
use crate::wal::{RecoveryError, StepBody, StepBodyRegistry};

#[derive(Debug, Clone)]
pub struct RecordWalRegistry {
    incarnation: Arc<str>,
}

impl RecordWalRegistry {
    #[must_use]
    pub fn new(incarnation: Arc<str>) -> Self {
        Self { incarnation }
    }

    #[must_use]
    pub fn incarnation(&self) -> &Arc<str> {
        &self.incarnation
    }
}

#[async_trait::async_trait]
impl StepBodyRegistry for RecordWalRegistry {
    async fn body_for(
        &self,
        conn: &Arc<Connection>,
        kind: WalKind,
        op_id: &OperationId,
    ) -> Result<Option<Arc<dyn StepBody>>, RecoveryError> {
        match kind {
            WalKind::Upsert | WalKind::Expire | WalKind::ForgetRecord => {}
            _ => return Ok(None),
        }

        let op_for_load = op_id.clone();
        let payload = conn
            .call(move |c| {
                load_payload(c, &op_for_load).map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
            })
            .await
            .map_err(RecoveryError::Storage)?;

        match (kind, payload) {
            (WalKind::Upsert, RecordWalPayload::Upsert(payload)) => {
                let locks = acquire_for_record(
                    conn,
                    &payload.record.scope,
                    &payload.record.target_id,
                    &self.incarnation,
                    op_id.as_str(),
                    "record_wal_recovery_upsert",
                )
                .await
                .map_err(|e| RecoveryError::Invariant(format!("recovery lock failed: {e}")))?;
                Ok(Some(Arc::new(RecordStepBody::new_upsert(*payload, locks))))
            }
            (WalKind::Expire, RecordWalPayload::Expire(payload)) => {
                let locks = acquire_for_record(
                    conn,
                    &payload.scope,
                    &payload.target_id,
                    &self.incarnation,
                    op_id.as_str(),
                    "record_wal_recovery_expire",
                )
                .await
                .map_err(|e| RecoveryError::Invariant(format!("recovery lock failed: {e}")))?;
                Ok(Some(Arc::new(RecordStepBody::new_expire(*payload, locks))))
            }
            (WalKind::ForgetRecord, RecordWalPayload::ForgetRecord(payload)) => {
                let locks = acquire_for_record(
                    conn,
                    &payload.scope,
                    &payload.target_id,
                    &self.incarnation,
                    op_id.as_str(),
                    "record_wal_recovery_forget_record",
                )
                .await
                .map_err(|e| RecoveryError::Invariant(format!("recovery lock failed: {e}")))?;
                Ok(Some(Arc::new(RecordStepBody::new_forget_record(
                    *payload, locks,
                ))))
            }
            (kind, RecordWalPayload::Purged(_)) => Err(purged_payload_recovery(kind)),
            (WalKind::Upsert, RecordWalPayload::Expire(_)) => {
                Err(payload_mismatch("expire", "upsert"))
            }
            (WalKind::Upsert, RecordWalPayload::ForgetRecord(_)) => {
                Err(payload_mismatch("forget_record", "upsert"))
            }
            (WalKind::Expire, RecordWalPayload::Upsert(_)) => {
                Err(payload_mismatch("upsert", "expire"))
            }
            (WalKind::Expire, RecordWalPayload::ForgetRecord(_)) => {
                Err(payload_mismatch("forget_record", "expire"))
            }
            (WalKind::ForgetRecord, RecordWalPayload::Upsert(_)) => {
                Err(payload_mismatch("upsert", "forget_record"))
            }
            (WalKind::ForgetRecord, RecordWalPayload::Expire(_)) => {
                Err(payload_mismatch("expire", "forget_record"))
            }
            _ => Ok(None),
        }
    }
}

fn payload_mismatch(payload_variant: &str, wal_kind: &str) -> RecoveryError {
    RecoveryError::Invariant(format!(
        "record wal payload variant {payload_variant} does not match wal kind {wal_kind}"
    ))
}

fn purged_payload_recovery(kind: WalKind) -> RecoveryError {
    RecoveryError::Invariant(format!(
        "purged record wal payload cannot be recovered as an active operation; \
         record wal payload variant purged does not match wal kind {}",
        kind.as_str()
    ))
}
