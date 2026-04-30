//! Integration tests for `do_search_semantic`.
//!
//! Covers:
//! 1. Capability gate — without an embedder the method returns
//!    `CapabilityUnavailable`.
//! 2. Empty-result path — store with vector capability but no embedded rows
//!    returns an empty page rather than an error.
//! 3. Hit path — a manually inserted vector row is returned by the ANN query,
//!    with `semantic_distance` populated.

use std::sync::Arc;

use cairn_core::contract::memory_store::{MemoryStore, SemanticSearchArgs};
use cairn_embeddings_local::{EmbeddingModel, EmbeddingModelKind, MockEmbedder, mock_vector};
use rusqlite::params;

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
async fn search_semantic_returns_empty_when_no_vectors_exist() {
    let embedder = make_embedder();
    let store = open_in_memory_with_embedder(Some(embedder)).await.unwrap();
    assert!(store.capabilities().vector, "store must have vector capability");

    let r = make_record();
    store.upsert(&r).await.unwrap();

    // No vectors have been written yet (embed-on-write is Task 8).
    // Expect empty results, not an error.
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
        page.candidates.is_empty(),
        "no vectors written → expected empty page, got {:?}",
        page.candidates.len()
    );
}

#[tokio::test]
async fn search_semantic_with_manual_vector_returns_results() {
    let embedder = make_embedder();
    let store =
        open_in_memory_with_embedder(Some(Arc::clone(&embedder))).await.unwrap();

    let r = make_record();
    let outcome = store.upsert(&r).await.unwrap();
    let rid = outcome.record_id.as_str().to_owned();

    // Manually insert a vector row — embed-on-write arrives in Task 8.
    let conn = store.raw_conn().unwrap().clone();
    let rid2 = rid.clone();
    let vec_bytes: Vec<u8> = mock_vector("hello world")
        .iter()
        .flat_map(|&f| f.to_le_bytes())
        .collect();
    conn.call(move |c| {
        c.execute(
            "INSERT INTO record_vectors(record_id, embedding, model)
               VALUES (?, ?, ?)",
            params![rid2, vec_bytes, "bge-small-en-v1.5"],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .unwrap();

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
        "should return the manually-inserted record"
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
