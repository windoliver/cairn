//! `MemoryStore` trait impl — stub bodies until Tasks 14-18 wire real ones.
//!
//! Every method first checks `self.conn`: `None` means the store was
//! constructed via `Default::default()` (registry stub) and is not
//! initialized, so we return a clear error directing callers to `open()`.

use async_trait::async_trait;
use cairn_core::contract::memory_store::{
    Edge, EdgeDir, EdgeKey, KeywordSearchArgs, KeywordSearchPage, ListArgs, ListPage, MemoryStore,
    MemoryStoreCapabilities, RecordVersion, StoreError, TombstoneReason, UpsertOutcome,
};
use cairn_core::contract::version::VersionRange;
use cairn_core::domain::{MemoryRecord, RecordId, TargetId};

use crate::open::CAPS;
use crate::store::SqliteMemoryStore;
use crate::{ACCEPTED_RANGE, PLUGIN_NAME};

fn not_initialized<T>(method: &'static str) -> Result<T, StoreError> {
    Err(format!(
        "cairn-store-sqlite: {method} called on unconnected store \
         (use cairn_store_sqlite::open(path).await first)"
    )
    .into())
}

fn not_implemented<T>(method: &'static str, issue: u32) -> Result<T, StoreError> {
    Err(format!("cairn-store-sqlite: {method} not yet implemented (#{issue})").into())
}

#[async_trait]
impl MemoryStore for SqliteMemoryStore {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn capabilities(&self) -> &MemoryStoreCapabilities {
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        ACCEPTED_RANGE
    }

    async fn upsert(&self, _r: &MemoryRecord) -> Result<UpsertOutcome, StoreError> {
        if self.conn.is_none() {
            return not_initialized("upsert");
        }
        not_implemented("upsert", 46)
    }

    async fn get(&self, _id: &RecordId) -> Result<Option<MemoryRecord>, StoreError> {
        if self.conn.is_none() {
            return not_initialized("get");
        }
        not_implemented("get", 46)
    }

    async fn list(&self, _a: &ListArgs) -> Result<ListPage, StoreError> {
        if self.conn.is_none() {
            return not_initialized("list");
        }
        not_implemented("list", 46)
    }

    async fn tombstone(&self, _id: &RecordId, _reason: TombstoneReason) -> Result<(), StoreError> {
        if self.conn.is_none() {
            return not_initialized("tombstone");
        }
        not_implemented("tombstone", 46)
    }

    async fn versions(&self, _t: &TargetId) -> Result<Vec<RecordVersion>, StoreError> {
        if self.conn.is_none() {
            return not_initialized("versions");
        }
        not_implemented("versions", 46)
    }

    async fn put_edge(&self, _e: &Edge) -> Result<(), StoreError> {
        if self.conn.is_none() {
            return not_initialized("put_edge");
        }
        not_implemented("put_edge", 46)
    }

    async fn remove_edge(&self, _k: &EdgeKey) -> Result<bool, StoreError> {
        if self.conn.is_none() {
            return not_initialized("remove_edge");
        }
        not_implemented("remove_edge", 46)
    }

    async fn neighbours(&self, _id: &RecordId, _d: EdgeDir) -> Result<Vec<Edge>, StoreError> {
        if self.conn.is_none() {
            return not_initialized("neighbours");
        }
        not_implemented("neighbours", 46)
    }

    async fn search_keyword(
        &self,
        _a: &KeywordSearchArgs<'_>,
    ) -> Result<KeywordSearchPage, StoreError> {
        if self.conn.is_none() {
            return not_initialized("search_keyword");
        }
        not_implemented("search_keyword", 47)
    }
}
