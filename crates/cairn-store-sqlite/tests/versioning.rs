//! Multi-version semantics: only-one-active per target, history complete,
//! prior versions reachable.

use cairn_core::contract::memory_store::MemoryStore;
use cairn_store_sqlite::open_in_memory;

#[tokio::test]
async fn three_upserts_produce_one_active_three_history() {
    let store = open_in_memory().await.expect("open");
    let r = cairn_core::domain::record::tests_export::sample_record();
    store.upsert(&r).await.expect("v1");

    let mut r2 = r.clone();
    r2.body = "second".to_owned();
    let out2 = store.upsert(&r2).await.expect("v2");
    assert_eq!(out2.version, 2);

    let mut r3 = r.clone();
    r3.body = "third".to_owned();
    let out3 = store.upsert(&r3).await.expect("v3");
    assert_eq!(out3.version, 3);

    let history = store.versions(&r.target_id).await.expect("versions");
    assert_eq!(history.len(), 3);
    let active_count = history.iter().filter(|v| v.active).count();
    assert_eq!(active_count, 1, "exactly one active row per target");
    assert!(history[2].active, "newest is active");
}
