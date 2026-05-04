//! `MemoryStore` trait impl.
//!
//! Every method first checks `self.conn`: `None` means the store was
//! constructed via `Default::default()` (registry stub) and is not
//! initialized, so we return a clear error directing callers to `open()`.

use std::collections::HashMap;

use async_trait::async_trait;
use cairn_core::contract::memory_store::{
    Edge, EdgeDir, EdgeKey, HybridSearchArgs, HybridSearchPage, IndexStats, KeywordSearchArgs,
    KeywordSearchPage, ListArgs, ListPage, MemoryStore, MemoryStoreCapabilities, RecordVersion,
    SemanticSearchArgs, SemanticSearchPage, StoreError, TombstoneReason, UpsertOutcome,
};
use cairn_core::contract::version::VersionRange;
use cairn_core::domain::consent_timeline::ConsentModel;
use cairn_core::domain::{MemoryRecord, RecordId, TargetId};

use crate::error::StoreError as ConcreteError;
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
        &self.caps
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

    async fn search_semantic(
        &self,
        args: &SemanticSearchArgs<'_>,
    ) -> Result<SemanticSearchPage, StoreError> {
        if self.conn.is_none() {
            return not_initialized("search_semantic");
        }
        self.do_search_semantic(args).await.map_err(Into::into)
    }

    async fn search_hybrid(
        &self,
        args: &HybridSearchArgs<'_>,
    ) -> Result<HybridSearchPage, StoreError> {
        if self.conn.is_none() {
            return not_initialized("search_hybrid");
        }
        self.do_search_hybrid(args).await.map_err(Into::into)
    }

    async fn index_stats(&self) -> Result<IndexStats, StoreError> {
        if self.conn.is_none() {
            return not_initialized("index_stats");
        }
        self.do_index_stats().await.map_err(Into::into)
    }

    async fn list_consent_models(&self) -> Result<HashMap<RecordId, ConsentModel>, StoreError> {
        if self.conn.is_none() {
            return not_initialized("list_consent_models");
        }
        self.do_list_consent_models().await.map_err(Into::into)
    }

    fn as_consent_lookup(
        &self,
    ) -> Option<&dyn cairn_core::contract::consent_lookup::ConsentLookup> {
        Some(self)
    }

    async fn upsert_entity(
        &self,
        node: &cairn_core::domain::graph::EntityNode,
    ) -> Result<cairn_core::domain::graph::EntityId, StoreError> {
        if self.conn.is_none() {
            return not_initialized("upsert_entity");
        }
        self.do_upsert_entity(node).await.map_err(Into::into)
    }

    async fn link_entity_episode(
        &self,
        entity_id: &cairn_core::domain::graph::EntityId,
        record_id: &cairn_core::domain::RecordId,
    ) -> Result<bool, StoreError> {
        if self.conn.is_none() {
            return not_initialized("link_entity_episode");
        }
        self.do_link_entity_episode(entity_id, record_id)
            .await
            .map_err(Into::into)
    }

    async fn upsert_entity_edge(
        &self,
        edge: &cairn_core::domain::graph::EntityEdge,
    ) -> Result<cairn_core::domain::graph::EntityEdgeOutcome, StoreError> {
        if self.conn.is_none() {
            return not_initialized("upsert_entity_edge");
        }
        self.do_upsert_entity_edge(edge).await.map_err(Into::into)
    }

    async fn resolve_contradiction(
        &self,
        old_edge_id: &cairn_core::domain::graph::EntityEdgeId,
        new_edge: &cairn_core::domain::graph::EntityEdge,
    ) -> Result<cairn_core::domain::graph::EntityEdgeOutcome, StoreError> {
        if self.conn.is_none() {
            return not_initialized("resolve_contradiction");
        }
        self.do_resolve_contradiction(old_edge_id, new_edge)
            .await
            .map_err(Into::into)
    }

    async fn graph_edges(
        &self,
        args: &cairn_core::domain::graph::GraphEdgesArgs<'_>,
    ) -> Result<Vec<cairn_core::domain::graph::EntityEdge>, StoreError> {
        if self.conn.is_none() {
            return not_initialized("graph_edges");
        }
        self.do_graph_edges(args).await.map_err(Into::into)
    }
}
