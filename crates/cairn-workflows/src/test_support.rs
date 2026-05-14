//! Crate-internal test helpers shared by unit tests across the new
//! workflow modules (issue #91). Production code never sees these —
//! the module is `#[cfg(test)]`-gated.

#![cfg(test)]
#![allow(clippy::expect_used)]

use std::sync::Mutex;

use async_trait::async_trait;
use cairn_core::contract::memory_store::{
    Edge, EdgeDir, EdgeKey, KeywordSearchArgs, KeywordSearchPage, ListArgs, ListPage, MemoryStore,
    MemoryStoreCapabilities, RecordVersion, StoreError, StoredRecord, TombstoneReason,
    UpsertOutcome,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::{RecordId, TargetId, record::MemoryRecord};

/// `MemoryStore` stub used by unit tests that never need to read or
/// mutate the store — the handler short-circuits to
/// [`crate::scheduler::HandlerOutcome::Permanent`] before touching it.
/// `upsert` and `tombstone` record their argument so a test can assert
/// they were called, but every other method returns the trait's
/// default (empty page / `None` / empty vec).
#[derive(Default)]
pub struct NoopMemoryStore {
    /// Records the trait-method calls in arrival order. Read via
    /// [`Self::log`] in tests.
    log: Mutex<Vec<String>>,
}

impl NoopMemoryStore {
    /// Snapshot of the call log captured so far.
    #[allow(dead_code, reason = "test helper exercised by other modules' tests")]
    pub fn log(&self) -> Vec<String> {
        self.log.lock().expect("test log").clone()
    }
}

#[async_trait]
impl MemoryStore for NoopMemoryStore {
    fn name(&self) -> &str {
        "noop"
    }
    fn capabilities(&self) -> &MemoryStoreCapabilities {
        static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
            fts: false,
            vector: false,
            graph_edges: false,
            transactions: false,
            per_record_consent_model: false,
            graph_search: false,
        };
        &CAPS
    }
    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
    }
    async fn upsert(&self, record: &MemoryRecord) -> Result<UpsertOutcome, StoreError> {
        self.log
            .lock()
            .expect("test log")
            .push(format!("upsert:{}", record.id.as_str()));
        Ok(UpsertOutcome {
            record_id: record.id.clone(),
            target_id: record.target_id.clone(),
            version: 1,
            content_changed: true,
            prior_hash: None,
        })
    }
    async fn get(&self, _id: &RecordId) -> Result<Option<MemoryRecord>, StoreError> {
        Ok(None)
    }
    async fn list(&self, _args: &ListArgs) -> Result<ListPage, StoreError> {
        Ok(ListPage {
            records: Vec::new(),
            next_cursor: None,
        })
    }
    async fn tombstone(&self, id: &RecordId, reason: TombstoneReason) -> Result<(), StoreError> {
        self.log.lock().expect("test log").push(format!(
            "tombstone:{}:{}",
            id.as_str(),
            reason.as_db_str()
        ));
        Ok(())
    }
    async fn versions(&self, _target: &TargetId) -> Result<Vec<RecordVersion>, StoreError> {
        Ok(Vec::new())
    }
    async fn list_active_stored(&self, _args: &ListArgs) -> Result<Vec<StoredRecord>, StoreError> {
        Ok(Vec::new())
    }
    async fn put_edge(&self, _edge: &Edge) -> Result<(), StoreError> {
        Ok(())
    }
    async fn remove_edge(&self, _key: &EdgeKey) -> Result<bool, StoreError> {
        Ok(false)
    }
    async fn neighbours(&self, _id: &RecordId, _dir: EdgeDir) -> Result<Vec<Edge>, StoreError> {
        Ok(Vec::new())
    }
    async fn search_keyword(
        &self,
        _args: &KeywordSearchArgs<'_>,
    ) -> Result<KeywordSearchPage, StoreError> {
        Err("test stub: search_keyword unavailable".into())
    }
}
