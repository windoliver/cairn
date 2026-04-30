//! `MemoryStore` trait impl.
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

use crate::error::StoreError as ConcreteError;
use crate::open::CAPS;
use crate::store::SqliteMemoryStore;
use crate::{ACCEPTED_RANGE, PLUGIN_NAME};

fn not_initialized<T>(method: &'static str) -> Result<T, StoreError> {
    Err(ConcreteError::NotInitialized { method }.into())
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

    async fn upsert(&self, record: &MemoryRecord) -> Result<UpsertOutcome, StoreError> {
        if self.conn.is_none() {
            return not_initialized("upsert");
        }
        self.do_upsert(record).await.map_err(Into::into)
    }

    async fn get(&self, id: &RecordId) -> Result<Option<MemoryRecord>, StoreError> {
        if self.conn.is_none() {
            return not_initialized("get");
        }
        self.do_get(id).await.map_err(Into::into)
    }

    async fn list(&self, args: &ListArgs) -> Result<ListPage, StoreError> {
        if self.conn.is_none() {
            return not_initialized("list");
        }
        self.do_list(args).await.map_err(Into::into)
    }

    async fn tombstone(&self, id: &RecordId, reason: TombstoneReason) -> Result<(), StoreError> {
        if self.conn.is_none() {
            return not_initialized("tombstone");
        }
        self.do_tombstone(id, reason).await.map_err(Into::into)
    }

    async fn versions(&self, target: &TargetId) -> Result<Vec<RecordVersion>, StoreError> {
        if self.conn.is_none() {
            return not_initialized("versions");
        }
        self.do_versions(target).await.map_err(Into::into)
    }

    async fn put_edge(&self, edge: &Edge) -> Result<(), StoreError> {
        if self.conn.is_none() {
            return not_initialized("put_edge");
        }
        self.do_put_edge(edge).await.map_err(Into::into)
    }

    async fn remove_edge(&self, key: &EdgeKey) -> Result<bool, StoreError> {
        if self.conn.is_none() {
            return not_initialized("remove_edge");
        }
        self.do_remove_edge(key).await.map_err(Into::into)
    }

    async fn neighbours(&self, id: &RecordId, dir: EdgeDir) -> Result<Vec<Edge>, StoreError> {
        if self.conn.is_none() {
            return not_initialized("neighbours");
        }
        self.do_neighbours(id, dir).await.map_err(Into::into)
    }

    async fn search_keyword(
        &self,
        args: &KeywordSearchArgs<'_>,
    ) -> Result<KeywordSearchPage, StoreError> {
        if self.conn.is_none() {
            return not_initialized("search_keyword");
        }
        self.do_search_keyword(args).await.map_err(Into::into)
    }
}
