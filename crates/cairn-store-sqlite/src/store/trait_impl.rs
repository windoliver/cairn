//! `MemoryStore` trait impl.
//!
//! Every method first checks `self.conn`: `None` means the store was
//! constructed via `Default::default()` (registry stub) and is not
//! initialized, so we return a clear error directing callers to `open()`.

use std::collections::HashMap;

use async_trait::async_trait;
use cairn_core::contract::memory_store::{
    AccessUpdate, DecayBatchOutcome, DecayPolicy, Edge, EdgeDir, EdgeKey, GraphNeighborsArgs,
    HybridSearchArgs, HybridSearchPage, IndexStats, KeywordSearchArgs, KeywordSearchPage, ListArgs,
    ListPage, MemoryStore, MemoryStoreCapabilities, ProjectionApplyItem, ProjectionRecord,
    RecordVersion, SemanticSearchArgs, SemanticSearchPage, StoreError, TombstoneReason,
    UpsertOutcome,
};
use cairn_core::contract::version::VersionRange;
use cairn_core::domain::consent_timeline::ConsentModel;
use cairn_core::domain::projection::{ProjectionCursor, ProjectionLedgerRow, ProjectionSummary};
use cairn_core::domain::{
    MemoryRecord, MergeStrategy, RecordId, SessionId, SessionMerge, SessionTree, TargetId,
};
use cairn_core::search::GraphCandidate;

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

    async fn get_session_tree(&self, root: &SessionId) -> Result<Option<SessionTree>, StoreError> {
        if self.conn.is_none() {
            return not_initialized("get_session_tree");
        }
        SqliteMemoryStore::get_session_tree(self, root)
            .await
            .map_err(Into::into)
    }

    async fn record_session_fork(
        &self,
        from: &SessionId,
        child: &SessionId,
        at_turn_id: &str,
    ) -> Result<(), StoreError> {
        if self.conn.is_none() {
            return not_initialized("record_session_fork");
        }
        SqliteMemoryStore::record_session_fork(self, from, child, at_turn_id)
            .await
            .map_err(Into::into)
    }

    async fn record_session_clone(
        &self,
        from: &SessionId,
        child: &SessionId,
    ) -> Result<(), StoreError> {
        if self.conn.is_none() {
            return not_initialized("record_session_clone");
        }
        SqliteMemoryStore::record_session_clone(self, from, child)
            .await
            .map_err(Into::into)
    }

    async fn record_session_tool_spawn(
        &self,
        from: &SessionId,
        child: &SessionId,
        at_turn_id: &str,
        tool_call_id: &str,
    ) -> Result<(), StoreError> {
        if self.conn.is_none() {
            return not_initialized("record_session_tool_spawn");
        }
        SqliteMemoryStore::record_session_tool_spawn(self, from, child, at_turn_id, tool_call_id)
            .await
            .map_err(Into::into)
    }

    async fn record_session_merge(
        &self,
        source: &SessionId,
        destination: &SessionId,
        strategy: MergeStrategy,
        applied_at_turn_id: &str,
    ) -> Result<SessionMerge, StoreError> {
        if self.conn.is_none() {
            return not_initialized("record_session_merge");
        }
        SqliteMemoryStore::record_session_merge(
            self,
            source,
            destination,
            strategy,
            applied_at_turn_id,
        )
        .await
        .map_err(Into::into)
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

    async fn search_graph_neighbors(
        &self,
        args: &GraphNeighborsArgs<'_>,
    ) -> Result<Vec<GraphCandidate>, StoreError> {
        if self.conn.is_none() {
            return not_initialized("search_graph_neighbors");
        }
        // Fail closed when the runtime probe in `bootstrap` decided
        // graph_search is unavailable (schema skew, partial migration,
        // stripped-down fork). The hybrid leg already short-circuits
        // through the verb-layer cap gate, but direct callers must get
        // the same error contract instead of a downstream SQL failure.
        if !self.caps.graph_search {
            return Err(ConcreteError::CapabilityUnavailable {
                what: "graph_search",
            }
            .into());
        }
        self.do_search_graph_neighbors(args)
            .await
            .map_err(Into::into)
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

    async fn projection_summaries(&self) -> Result<Vec<ProjectionSummary>, StoreError> {
        if self.conn.is_none() {
            return not_initialized("projection_summaries");
        }
        self.do_projection_summaries().await.map_err(Into::into)
    }

    async fn projection_cursors(&self) -> Result<Vec<ProjectionCursor>, StoreError> {
        if self.conn.is_none() {
            return not_initialized("projection_cursors");
        }
        self.do_projection_records()
            .await
            .map(|records| records.into_iter().map(|record| record.cursor).collect())
            .map_err(Into::into)
    }

    async fn projection_records(&self) -> Result<Vec<ProjectionRecord>, StoreError> {
        if self.conn.is_none() {
            return not_initialized("projection_records");
        }
        self.do_projection_records().await.map_err(Into::into)
    }

    async fn projection_failures(&self) -> Result<Vec<ProjectionLedgerRow>, StoreError> {
        if self.conn.is_none() {
            return not_initialized("projection_failures");
        }
        self.do_projection_failures().await.map_err(Into::into)
    }

    async fn apply_projection_items(
        &self,
        items: Vec<ProjectionApplyItem>,
    ) -> Result<(), StoreError> {
        if self.conn.is_none() {
            return not_initialized("apply_projection_items");
        }
        self.do_apply_projection_items(items)
            .await
            .map_err(Into::into)
    }

    async fn trace_canvas_lint_snapshot(
        &self,
    ) -> Result<cairn_core::verbs::lint::checks::trace_canvas::TraceCanvasLintSnapshot, StoreError>
    {
        if self.conn.is_none() {
            return not_initialized("trace_canvas_lint_snapshot");
        }
        SqliteMemoryStore::trace_canvas_lint_snapshot(self)
            .await
            .map_err(Into::into)
    }

    async fn record_access(
        &self,
        record_ids: &[RecordId],
        accessed_at_ms: i64,
        reason: &str,
    ) -> Result<Vec<AccessUpdate>, StoreError> {
        if self.conn.is_none() {
            return not_initialized("record_access");
        }
        self.do_record_access(record_ids, accessed_at_ms, reason)
            .await
            .map_err(Into::into)
    }

    async fn pin_record(&self, record_id: &RecordId, pinned: bool) -> Result<(), StoreError> {
        if self.conn.is_none() {
            return not_initialized("pin_record");
        }
        self.do_pin_record(record_id, pinned)
            .await
            .map_err(Into::into)
    }

    async fn decay_salience_batch(
        &self,
        now_ms: i64,
        policy: DecayPolicy,
    ) -> Result<DecayBatchOutcome, StoreError> {
        if self.conn.is_none() {
            return not_initialized("decay_salience_batch");
        }
        self.do_decay_salience_batch(now_ms, policy)
            .await
            .map_err(Into::into)
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
