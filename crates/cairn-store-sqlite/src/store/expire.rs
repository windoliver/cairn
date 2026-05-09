//! Concrete expire API for target-level soft retirement.

use cairn_core::domain::TargetId;
use tracing::instrument;

use crate::error::StoreError;
use crate::store::SqliteMemoryStore;

impl SqliteMemoryStore {
    /// Soft-expire every version in a target lineage through WAL.
    ///
    /// Expire removes the target from default read and search results by
    /// setting `active = 0`, `tombstoned = 1`, and
    /// `tombstone_reason = 'expire'`. It does not physically delete rows.
    ///
    /// # Errors
    ///
    /// Returns store, lock, WAL runner, or recovery-shape errors.
    #[instrument(skip(self), err, fields(verb = "expire", target_id = %target.as_str()))]
    pub async fn expire(&self, target: &TargetId) -> Result<(), StoreError> {
        if self.conn.is_none() {
            return Err(StoreError::NotInitialized { method: "expire" });
        }
        crate::record_wal::apply_expire(self, target).await
    }
}
