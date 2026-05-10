//! Concrete `forget_record` API for record-level forget through the record WAL.

use cairn_core::contract::memory_store::ForgetReceipt;
use cairn_core::domain::{Identity, TargetId};
use tracing::instrument;

use crate::error::StoreError;
use crate::store::SqliteMemoryStore;

impl SqliteMemoryStore {
    /// Inherent `forget_record` implementation; the trait method
    /// [`MemoryStore::forget_record`] guards `self.conn` then delegates here.
    ///
    /// Phase A tombstones the lineage and emits the body-free receipt; Phase
    /// B physically purges vectors, FTS, edges, primary rows, and WAL
    /// pre-images. Idempotent and crash-safe; returns the §14 forget-receipt
    /// allowlist {`target_id_hash`, `op_id`, `purged_at`}.
    ///
    /// [`MemoryStore::forget_record`]: cairn_core::contract::memory_store::MemoryStore::forget_record
    ///
    /// # Errors
    ///
    /// Returns store, lock, WAL runner, or recovery-shape errors.
    #[instrument(
        skip(self),
        err,
        fields(verb = "forget_record", target_id = %target.as_str(), actor = %actor.as_str()),
    )]
    pub(crate) async fn do_forget_record(
        &self,
        target: &TargetId,
        actor: &Identity,
    ) -> Result<ForgetReceipt, StoreError> {
        if self.conn.is_none() {
            return Err(StoreError::NotInitialized {
                method: "forget_record",
            });
        }
        crate::record_wal::apply_forget_record(self, target, actor).await
    }
}
