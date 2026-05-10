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
            WalKind::Upsert | WalKind::Expire => {}
            _ => return Ok(None),
        }

        let op_for_load = op_id.clone();
        let payload = conn
            .call(move |c| {
                load_payload(c, &op_for_load).map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
            })
            .await
            .map_err(RecoveryError::Storage)?;

        match kind {
            WalKind::Upsert => match payload {
                RecordWalPayload::Upsert(payload) => {
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
                RecordWalPayload::Expire(_) => Err(payload_mismatch("expire", "upsert")),
                RecordWalPayload::ForgetRecord(_) => {
                    Err(payload_mismatch("forget_record", "upsert"))
                }
                RecordWalPayload::Purged(_) => Err(payload_mismatch("purged", "upsert")),
            },
            WalKind::Expire => match payload {
                RecordWalPayload::Expire(payload) => {
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
                RecordWalPayload::Upsert(_) => Err(payload_mismatch("upsert", "expire")),
                RecordWalPayload::ForgetRecord(_) => {
                    Err(payload_mismatch("forget_record", "expire"))
                }
                RecordWalPayload::Purged(_) => Err(payload_mismatch("purged", "expire")),
            },
            _ => Ok(None),
        }
    }
}

fn payload_mismatch(payload_variant: &str, wal_kind: &str) -> RecoveryError {
    RecoveryError::Invariant(format!(
        "record wal payload variant {payload_variant} does not match wal kind {wal_kind}"
    ))
}
