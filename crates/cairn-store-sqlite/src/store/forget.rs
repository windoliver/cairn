//! Store-facing record forget helper.

use cairn_core::domain::RecordId;

use crate::error::StoreError;
use crate::record_wal::forget::{ForgetOutcome, apply_forget_record};
use crate::store::SqliteMemoryStore;

impl SqliteMemoryStore {
    /// Forget one public record id by deleting the full target lineage.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the record id is not present, WAL setup fails,
    /// lock fencing fails, or a Phase B step exhausts retries.
    pub async fn forget_record(&self, record_id: &RecordId) -> Result<ForgetOutcome, StoreError> {
        apply_forget_record(self, record_id).await
    }
}
