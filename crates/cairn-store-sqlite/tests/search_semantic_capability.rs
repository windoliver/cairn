//! Capability-gate conformance: `search_semantic` returns `CapabilityUnavailable`
//! when vector capability is false.

use cairn_core::contract::memory_store::{MemoryStore, SemanticSearchArgs};
use cairn_store_sqlite::open_in_memory;

#[tokio::test]
async fn no_embedder_returns_capability_unavailable() {
    let store = open_in_memory().await.unwrap();
    assert!(!store.capabilities().vector);

    let err = store
        .search_semantic(&SemanticSearchArgs {
            query: "test".into(),
            filter: None,
            visibility_allowlist: vec![],
            limit: 5,
            model_label: "bge-small-en-v1.5".into(),
            with_explain: false,
        })
        .await
        .unwrap_err();

    let msg = err.to_string();
    assert!(
        msg.contains("capability") || msg.contains("vector"),
        "expected CapabilityUnavailable, got: {msg}"
    );
}
