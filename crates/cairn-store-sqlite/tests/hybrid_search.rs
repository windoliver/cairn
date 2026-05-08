//! Hybrid search integration test using a deterministic mock embedder.
//!
//! Covers the end-to-end hybrid path: open store with embedder, upsert two
//! distinguishable rows so embed-on-write populates `record_vectors`, then
//! issue a hybrid query and assert the keyword-matching row outranks the
//! non-matching one.

use std::sync::Arc;

use cairn_core::contract::memory_store::{HybridSearchArgs, MemoryStore, SemanticSearchArgs};
use cairn_core::domain::taxonomy::MemoryVisibility;
use cairn_core::domain::{RecordId, TargetId};
use cairn_embeddings_local::{EmbeddingModel, EmbeddingModelKind, MockEmbedder};
use cairn_store_sqlite::open_in_memory_with_embedder;

#[tokio::test]
async fn hybrid_returns_results() {
    let kind = EmbeddingModelKind::default();
    let embedder: Arc<dyn EmbeddingModel> = Arc::new(MockEmbedder::new(kind));
    let store = open_in_memory_with_embedder(Some(Arc::clone(&embedder)))
        .await
        .expect("open store with embedder");
    assert!(
        store.capabilities().vector,
        "store must have vector capability when embedder is wired",
    );

    // Two records with distinguishable bodies so the keyword leg can
    // distinguish them. Both share visibility = Private (sample_record default)
    // so the visibility allowlist below admits both.
    let mut a = cairn_core::domain::record::tests_export::sample_record();
    "alice chen worked at novapay".clone_into(&mut a.body);

    let mut b = cairn_core::domain::record::tests_export::sample_record();
    b.id = RecordId::parse("01HQZX9F5N00000000000000B1".to_owned()).expect("valid id");
    b.target_id = TargetId::parse("01HQZX9F5N00000000000000B2".to_owned()).expect("valid target");
    "carol nakamura runs mindbridge".clone_into(&mut b.body);

    store.upsert(&a).await.expect("upsert a");
    store.upsert(&b).await.expect("upsert b");

    // Sanity: semantic search with an embedder produces results too — guards
    // against silent misconfiguration of the test fixture.
    let sem = store
        .search_semantic(&SemanticSearchArgs {
            query: "alice".into(),
            filter: None,
            auth_scope: cairn_core::domain::ScopeTuple::default(),
            visibility_allowlist: vec![MemoryVisibility::Private],
            limit: 5,
            model_label: kind.as_str().to_owned(),
            with_explain: false,
        })
        .await
        .expect("semantic search");
    assert!(
        !sem.candidates.is_empty(),
        "semantic leg should produce candidates after embed-on-write",
    );

    let args = HybridSearchArgs {
        query: "alice".into(),
        filter: None,
        auth_scope: cairn_core::domain::ScopeTuple::default(),
        visibility_allowlist: vec![MemoryVisibility::Private],
        limit: 5,
        model_label: kind.as_str().to_owned(),
        blend: 0.7,
        rrf_k: 60,
        rerank_topk: 20,
        with_explain: false,
        confidence_floor: 1e-3,
        graph_confidence_min: 0.0,
    };
    let page = store.search_hybrid(&args).await.expect("hybrid search");
    assert!(!page.candidates.is_empty(), "hybrid returned 0 results");
    assert_eq!(
        page.candidates[0].record_id,
        a.id,
        "record A (matches keyword 'alice') must rank first; got {:?}",
        page.candidates
            .iter()
            .map(|c| c.record_id.as_str())
            .collect::<Vec<_>>(),
    );
}

#[tokio::test]
async fn hybrid_capability_unavailable_without_embedder() {
    let store = open_in_memory_with_embedder(None)
        .await
        .expect("open store without embedder");
    assert!(
        !store.capabilities().vector,
        "store without embedder must not advertise vector capability",
    );

    let args = HybridSearchArgs {
        query: "anything".into(),
        filter: None,
        auth_scope: cairn_core::domain::ScopeTuple::default(),
        visibility_allowlist: vec![MemoryVisibility::Private],
        limit: 5,
        model_label: EmbeddingModelKind::default().as_str().to_owned(),
        blend: 0.7,
        rrf_k: 60,
        rerank_topk: 20,
        with_explain: false,
        confidence_floor: 1e-3,
        graph_confidence_min: 0.0,
    };
    let result = store.search_hybrid(&args).await;
    assert!(result.is_err(), "expected CapabilityUnavailable");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("capability") || msg.contains("vector"),
        "expected CapabilityUnavailable, got: {msg}",
    );
}
