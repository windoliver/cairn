//! Record WAL recovery registry.

use std::sync::Arc;

use cairn_core::domain::ScopeTuple;
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
            WalKind::ForgetRecord => return Ok(None),
            _ => return Ok(None),
        }

        let op_for_load = op_id.clone();
        let payload = conn
            .call(move |c| {
                load_payload(c, &op_for_load).map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
            })
            .await
            .map_err(RecoveryError::Storage)?;

        match payload {
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
                Ok(Some(Arc::new(RecordStepBody::new_upsert(payload, locks))))
            }
            RecordWalPayload::Expire(payload) => {
                let scope = ScopeTuple::default();
                let locks = acquire_for_record(
                    conn,
                    &scope,
                    &payload.target_id,
                    &self.incarnation,
                    op_id.as_str(),
                    "record_wal_recovery_expire",
                )
                .await
                .map_err(|e| RecoveryError::Invariant(format!("recovery lock failed: {e}")))?;
                Ok(Some(Arc::new(RecordStepBody::new_expire(payload, locks))))
            }
        }
    }
}
