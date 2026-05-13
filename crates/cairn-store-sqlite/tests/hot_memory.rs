//! `SQLite` hot-memory integration tests.

use cairn_core::contract::memory_store::{
    HotMemoryInvalidationScope, HotMemoryRequest, MemoryStore,
};
use cairn_core::hot_memory::{
    HotMemoryCacheInfo, HotMemoryOptions, HotMemorySourceKind, assemble_hot_memory,
};
use cairn_store_sqlite::{HotRecordSeed, SqliteMemoryStore};

fn request() -> HotMemoryRequest {
    HotMemoryRequest {
        session_id: Some("session-a".to_owned()),
        agent_id: Some("agent-a".to_owned()),
        budget_bytes: 4096,
        config_fingerprint: "config-a".to_owned(),
        god_node_weight: 0.3,
    }
}

#[tokio::test]
async fn hot_memory_input_reads_vault_files_records_and_edges() {
    let store = SqliteMemoryStore::open_memory().expect("store");
    store
        .write_vault_file("purpose.md", "purpose text")
        .expect("purpose");
    store
        .write_vault_file("index.md", "index text")
        .expect("index");
    store
        .insert_hot_record(
            HotRecordSeed::new("01J0000000000000000000001", "user", "pinned text")
                .tag("pinned")
                .salience(0.9),
        )
        .expect("pinned");
    store
        .insert_hot_record(
            HotRecordSeed::new("01J0000000000000000000002", "playbook", "playbook text")
                .salience(0.8),
        )
        .expect("playbook");
    store
        .insert_entity_edge(
            "ImportantType",
            "ContainsOnly",
            "uses",
            "src/important.rs",
            None,
        )
        .expect("edge");

    let input = store.hot_memory_input(&request()).await.expect("input");

    assert!(
        input
            .sources
            .iter()
            .any(|s| s.kind == HotMemorySourceKind::Purpose && s.body.contains("purpose text"))
    );
    assert!(
        input
            .sources
            .iter()
            .any(|s| s.kind == HotMemorySourceKind::ProjectState && s.body.contains("index text"))
    );
    assert!(
        input
            .sources
            .iter()
            .any(|s| s.kind == HotMemorySourceKind::Pinned && s.body.contains("pinned text"))
    );
    assert!(
        input
            .sources
            .iter()
            .any(|s| s.kind == HotMemorySourceKind::Playbook && s.body.contains("playbook text"))
    );
    assert!(input.sources.iter().any(|s| s.centrality_score > 0.0));
}

#[tokio::test]
async fn hot_memory_cache_hits_and_invalidates_by_session() {
    let store = SqliteMemoryStore::open_memory().expect("store");
    store
        .write_vault_file("purpose.md", "purpose text")
        .expect("purpose");
    let req = request();
    let input = store.hot_memory_input(&req).await.expect("input");
    let key = store.hot_memory_cache_key(&req, &input).expect("key");
    let output = assemble_hot_memory(
        &input,
        HotMemoryOptions {
            budget_bytes: req.budget_bytes,
            god_node_weight: req.god_node_weight,
            cache: HotMemoryCacheInfo::refreshed(key.clone()),
        },
    );
    store
        .store_hot_memory_cache(&key, &output)
        .await
        .expect("store cache");
    assert!(
        store
            .load_hot_memory_cache(&key)
            .await
            .expect("load cache")
            .is_some()
    );
    let deleted = store
        .invalidate_hot_memory_cache(HotMemoryInvalidationScope::Session("session-a".to_owned()))
        .await
        .expect("invalidate");
    assert_eq!(deleted, 1);
    assert!(
        store
            .load_hot_memory_cache(&key)
            .await
            .expect("load cache")
            .is_none()
    );
}
