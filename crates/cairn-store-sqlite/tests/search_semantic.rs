//! Integration tests for `do_search_semantic`.
//!
//! Covers:
//! 1. Capability gate — without an embedder the method returns
//!    `CapabilityUnavailable`.
//! 2. Hit path — after upsert, embed-on-write writes the vector row so a
//!    semantic search returns the record with `semantic_distance` populated.

use std::sync::Arc;

use cairn_core::contract::memory_store::{MemoryStore, SemanticSearchArgs};
use cairn_embeddings_local::{EmbeddingModel, EmbeddingModelKind, MockEmbedder};

use cairn_store_sqlite::open_in_memory_with_embedder;

fn make_embedder() -> Arc<dyn EmbeddingModel> {
    Arc::new(MockEmbedder::new(EmbeddingModelKind::BgeSmallEnV1_5))
}

/// Build a minimal record for test use.
fn make_record() -> cairn_core::domain::MemoryRecord {
    cairn_test_fixtures::sample_record(42)
}

#[tokio::test]
async fn search_semantic_capability_unavailable_without_embedder() {
    let store = open_in_memory_with_embedder(None).await.unwrap();
    assert!(!store.capabilities().vector);

    let result = store
        .search_semantic(&SemanticSearchArgs {
            query: "hello".into(),
            filter: None,
            visibility_allowlist: vec![],
            limit: 5,
            model_label: "bge-small-en-v1.5".into(),
        })
        .await;

    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("capability") || msg.contains("vector"),
        "expected CapabilityUnavailable, got: {msg}"
    );
}

#[tokio::test]
async fn search_semantic_returns_results_after_upsert() {
    let embedder = make_embedder();
    let store = open_in_memory_with_embedder(Some(embedder)).await.unwrap();
    assert!(
        store.capabilities().vector,
        "store must have vector capability"
    );

    let r = make_record();
    let outcome = store.upsert(&r).await.unwrap();

    // Embed-on-write (Task 8) means upsert now writes the vector row atomically.
    // A semantic search should return the record.
    let page = store
        .search_semantic(&SemanticSearchArgs {
            query: "hello".into(),
            filter: None,
            visibility_allowlist: vec![],
            limit: 10,
            model_label: EmbeddingModelKind::BgeSmallEnV1_5.as_str().into(),
        })
        .await
        .unwrap();

    assert!(
        !page.candidates.is_empty(),
        "embed-on-write should have written a vector row → expected non-empty page",
    );
    assert!(
        page.candidates[0].semantic_distance.is_some(),
        "semantic_distance must be set on ANN candidates",
    );
    assert_eq!(
        page.candidates[0].record_id, outcome.record_id,
        "returned record_id must match the upserted record",
    );
}

#[tokio::test]
async fn search_semantic_with_vector_returns_results() {
    let embedder = make_embedder();
    let store = open_in_memory_with_embedder(Some(Arc::clone(&embedder)))
        .await
        .unwrap();

    let r = make_record();
    let outcome = store.upsert(&r).await.unwrap();

    // Embed-on-write (Task 8) writes the vector row atomically during upsert.
    let page = store
        .search_semantic(&SemanticSearchArgs {
            query: "hello world".into(),
            filter: None,
            visibility_allowlist: vec![],
            limit: 10,
            model_label: "bge-small-en-v1.5".into(),
        })
        .await
        .unwrap();

    assert!(
        !page.candidates.is_empty(),
        "should return the upserted record via embed-on-write"
    );
    assert!(
        page.candidates[0].semantic_distance.is_some(),
        "semantic_distance must be set on ANN candidates"
    );
    assert_eq!(
        page.candidates[0].record_id, outcome.record_id,
        "returned record_id must match the upserted record"
    );
}
