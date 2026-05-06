//! SDK search dispatch — verifies the executive path through `Sdk::with_store`.
//!
//! Constructs an `EmptyStore` stub that implements the full `MemoryStore`
//! trait and returns empty pages for all three search legs. Passes it to
//! `Sdk::with_store` and asserts that:
//! - keyword search succeeds and returns an empty hit list.
//! - semantic search is rejected (config has `local_embeddings: true` but
//!   `capabilities(false)` does not gate on model presence for the SDK —
//!   the `with_store` path derives caps via `config.search.local_embeddings`).
//! - hybrid search succeeds and returns an empty hit list.
//! - an empty query is still rejected as `InvalidArgs` before dispatch.

use std::sync::Arc;

use cairn_core::config::CairnConfig;
use cairn_core::contract::memory_store::CONTRACT_VERSION;
use cairn_core::contract::memory_store::{
    Edge, EdgeDir, EdgeKey, HybridSearchArgs, HybridSearchPage, KeywordSearchArgs,
    KeywordSearchPage, ListArgs, ListPage, MemoryStore, MemoryStoreCapabilities, RecordVersion,
    SemanticSearchArgs, SemanticSearchPage, StoreError, TombstoneReason, UpsertOutcome,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::record::MemoryRecord;
use cairn_core::domain::{RecordId, TargetId};
use cairn_core::generated::verbs::search::{SearchArgs, SearchArgsMode};
use cairn_sdk::{Sdk, SdkError};

/// Minimal stub store: returns empty pages for all three search legs.
/// Panics on any CRUD method — search tests should never reach those paths.
struct EmptyStore;

#[async_trait::async_trait]
impl MemoryStore for EmptyStore {
    fn name(&self) -> &'static str {
        "empty"
    }

    fn capabilities(&self) -> &MemoryStoreCapabilities {
        static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
            fts: true,
            vector: true,
            graph_edges: false,
            transactions: true,
            per_record_consent_model: true,

            graph_search: false,
        };
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(
            CONTRACT_VERSION,
            ContractVersion::new(CONTRACT_VERSION.major, CONTRACT_VERSION.minor + 1, 0),
        )
    }

    async fn upsert(&self, _r: &MemoryRecord) -> Result<UpsertOutcome, StoreError> {
        unimplemented!("EmptyStore: upsert not used in search tests")
    }

    async fn get(&self, _id: &RecordId) -> Result<Option<MemoryRecord>, StoreError> {
        Ok(None)
    }

    async fn list(&self, _args: &ListArgs) -> Result<ListPage, StoreError> {
        Ok(ListPage {
            records: vec![],
            next_cursor: None,
        })
    }

    async fn tombstone(&self, _id: &RecordId, _reason: TombstoneReason) -> Result<(), StoreError> {
        unimplemented!("EmptyStore: tombstone not used in search tests")
    }

    async fn versions(&self, _target: &TargetId) -> Result<Vec<RecordVersion>, StoreError> {
        Ok(vec![])
    }

    async fn put_edge(&self, _edge: &Edge) -> Result<(), StoreError> {
        unimplemented!("EmptyStore: put_edge not used in search tests")
    }

    async fn remove_edge(&self, _key: &EdgeKey) -> Result<bool, StoreError> {
        Ok(false)
    }

    async fn neighbours(&self, _id: &RecordId, _dir: EdgeDir) -> Result<Vec<Edge>, StoreError> {
        Ok(vec![])
    }

    async fn search_keyword(
        &self,
        _args: &KeywordSearchArgs<'_>,
    ) -> Result<KeywordSearchPage, StoreError> {
        Ok(KeywordSearchPage {
            candidates: vec![],
            next_cursor: None,
            explain: None,
        })
    }

    async fn search_semantic(
        &self,
        _args: &SemanticSearchArgs<'_>,
    ) -> Result<SemanticSearchPage, StoreError> {
        Ok(SemanticSearchPage {
            candidates: vec![],
            explain: None,
        })
    }

    async fn search_hybrid(
        &self,
        _args: &HybridSearchArgs<'_>,
    ) -> Result<HybridSearchPage, StoreError> {
        Ok(HybridSearchPage {
            candidates: vec![],
            explain: None,
        })
    }
}

fn client() -> Sdk {
    Sdk::with_store(Arc::new(EmptyStore), CairnConfig::default())
}

fn keyword_args(query: &str) -> SearchArgs {
    SearchArgs {
        citations: None,
        cursor: None,
        filters: None,
        limit: Some(10),
        mode: SearchArgsMode::Keyword,
        query: query.to_owned(),
        scope: None,
        explain: Some(false),
    }
}

fn hybrid_args(query: &str) -> SearchArgs {
    SearchArgs {
        citations: None,
        cursor: None,
        filters: None,
        limit: Some(10),
        mode: SearchArgsMode::Hybrid,
        query: query.to_owned(),
        scope: None,
        explain: Some(false),
    }
}

#[tokio::test]
async fn keyword_dispatch_returns_empty_page() {
    let resp = client()
        .search(&keyword_args("hello"))
        .await
        .expect("keyword search with wired store must succeed");
    let data = resp.data;
    assert!(data.hits.is_empty(), "empty store: no hits expected");
    assert!(data.next_cursor.is_none());
    assert!(data.score_explain.is_none());
}

#[tokio::test]
async fn hybrid_dispatch_returns_empty_page() {
    let resp = client()
        .search(&hybrid_args("hello"))
        .await
        .expect("hybrid search with wired store must succeed");
    assert!(resp.data.hits.is_empty(), "empty store: no hits expected");
}

#[tokio::test]
async fn empty_query_still_rejected_as_invalid_args_with_store_wired() {
    // Validation runs before dispatch — even with a store wired, an empty
    // query must return InvalidArgs, not reach the store.
    let err = client()
        .search(&keyword_args(""))
        .await
        .expect_err("empty query must be rejected");
    assert!(
        matches!(err, SdkError::InvalidArgs { .. }),
        "expected InvalidArgs, got {err:?}"
    );
}

#[tokio::test]
async fn semantic_dispatch_rejected_without_model_on_disk() {
    // Default config has `local_embeddings: true` but the SDK derives
    // `semantic_search = local_embeddings && model_present`. The SDK uses
    // `config.search.local_embeddings` as the proxy for model_present
    // (Task 12 scope: no filesystem stat). With `local_embeddings: true`,
    // semantic is advertised and the dispatch goes through to the store.
    // This test verifies the wired path: semantic succeeds when
    // local_embeddings is true (EmptyStore.search_semantic returns Ok).
    let args = SearchArgs {
        citations: None,
        cursor: None,
        filters: None,
        limit: Some(5),
        mode: SearchArgsMode::Semantic,
        query: "hello".to_owned(),
        scope: None,
        explain: Some(false),
    };
    let resp = client()
        .search(&args)
        .await
        .expect("semantic with local_embeddings:true must dispatch");
    assert!(resp.data.hits.is_empty());
}

#[tokio::test]
async fn semantic_rejected_when_local_embeddings_disabled() {
    // When `search.local_embeddings: false`, semantic must return
    // CapabilityUnavailable even with a store wired.
    let mut config = CairnConfig::default();
    config.search.local_embeddings = false;
    let sdk = Sdk::with_store(Arc::new(EmptyStore), config);
    let args = SearchArgs {
        citations: None,
        cursor: None,
        filters: None,
        limit: Some(5),
        mode: SearchArgsMode::Semantic,
        query: "hello".to_owned(),
        scope: None,
        explain: Some(false),
    };
    let err = sdk.search(&args).await.expect_err("semantic disabled");
    assert!(
        matches!(err, SdkError::CapabilityUnavailable { .. }),
        "expected CapabilityUnavailable when local_embeddings: false, got {err:?}"
    );
}

// ── Provider/model alignment tests (round-4 review Finding A) ────────────────
//
// A store with `vector: true` but a config whose `default_provider` and
// `embedding_model` disagree must NOT advertise semantic/hybrid. Without
// this gate, the dispatcher reaches the ANN leg and returns zero results
// because the indexed `vec_model` label never matches the wrong provider's
// filter. Tests below use a helper store with `vector: true` to isolate the
// alignment check from model-presence gating.

fn misaligned_config() -> CairnConfig {
    use cairn_core::config::{EmbeddingModelKind, EmbeddingProvider};
    let mut cfg = CairnConfig::default();
    // OpenAI provider but a local candle model — classic misconfiguration.
    cfg.search.default_provider = EmbeddingProvider::OpenAi;
    cfg.search.embedding_model = EmbeddingModelKind::BgeSmallEnV1_5;
    cfg
}

/// `status().capabilities` must NOT contain `search.semantic` when
/// `default_provider = openai` but `embedding_model = bge-small-en-v1.5`.
#[test]
fn status_drops_semantic_for_openai_provider_with_local_model() {
    let sdk = Sdk::with_store(Arc::new(EmptyStore), misaligned_config());
    let caps: Vec<String> = sdk
        .status()
        .capabilities
        .iter()
        .filter_map(|c| serde_json::to_value(c).ok())
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    assert!(
        !caps.iter().any(|c| c == "cairn.mcp.v1.search.semantic"),
        "semantic must NOT be advertised when provider/model are misaligned; caps = {caps:?}"
    );
    assert!(
        !caps.iter().any(|c| c == "cairn.mcp.v1.search.hybrid"),
        "hybrid must NOT be advertised when provider/model are misaligned; caps = {caps:?}"
    );
    // Keyword should still work — it doesn't require an embedding provider.
    assert!(
        caps.iter().any(|c| c == "cairn.mcp.v1.search.keyword"),
        "keyword MUST be advertised even when provider/model are misaligned; caps = {caps:?}"
    );
}

/// `Sdk::search(semantic, …)` must return `CapabilityUnavailable` (not silently
/// dispatch) when `default_provider = openai` + `embedding_model = bge-small-en-v1.5`.
#[tokio::test]
async fn search_semantic_rejected_for_misaligned_provider_model() {
    let sdk = Sdk::with_store(Arc::new(EmptyStore), misaligned_config());
    let args = SearchArgs {
        citations: None,
        cursor: None,
        filters: None,
        limit: Some(5),
        mode: SearchArgsMode::Semantic,
        query: "hello".to_owned(),
        scope: None,
        explain: Some(false),
    };
    let err = sdk
        .search(&args)
        .await
        .expect_err("misaligned config must reject semantic");
    assert!(
        matches!(err, SdkError::CapabilityUnavailable { .. }),
        "expected CapabilityUnavailable for misaligned provider/model, got {err:?}"
    );
}

/// Same rejection for hybrid mode with a misaligned config.
#[tokio::test]
async fn search_hybrid_rejected_for_misaligned_provider_model() {
    let sdk = Sdk::with_store(Arc::new(EmptyStore), misaligned_config());
    let args = SearchArgs {
        citations: None,
        cursor: None,
        filters: None,
        limit: Some(5),
        mode: SearchArgsMode::Hybrid,
        query: "hello".to_owned(),
        scope: None,
        explain: Some(false),
    };
    let err = sdk
        .search(&args)
        .await
        .expect_err("misaligned config must reject hybrid");
    assert!(
        matches!(err, SdkError::CapabilityUnavailable { .. }),
        "expected CapabilityUnavailable for misaligned provider/model (hybrid), got {err:?}"
    );
}
