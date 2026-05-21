//! Tombstone records each `TombstoneReason` distinctly and is idempotent.
//!
//! Pins three behaviours of `MemoryStore::tombstone`:
//!
//! - the persisted `tombstone_reason` round-trips back through `versions`,
//! - re-tombstoning the same row is idempotent (no extra version row),
//! - tombstoned rows are hidden from `get`.

use cairn_core::contract::memory_store::{MemoryStore, TombstoneReason};
use cairn_store_sqlite::open_in_memory;

#[tokio::test]
async fn tombstone_records_reason() {
    let store = open_in_memory().await.expect("open");
    let r = cairn_core::domain::record::tests_export::sample_record();
    store.upsert(&r).await.expect("upsert");
    store
        .tombstone(&r.id, TombstoneReason::Forget)
        .await
        .expect("tombstone");
    let history = store.versions(&r.target_id).await.expect("versions");
    assert_eq!(history.len(), 1);
    assert!(history[0].tombstoned);
    assert_eq!(history[0].tombstone_reason, Some(TombstoneReason::Forget));
}

#[tokio::test]
async fn tombstone_is_idempotent() {
    let store = open_in_memory().await.expect("open");
    let r = cairn_core::domain::record::tests_export::sample_record();
    store.upsert(&r).await.expect("upsert");
    store
        .tombstone(&r.id, TombstoneReason::Update)
        .await
        .expect("first");
    store
        .tombstone(&r.id, TombstoneReason::Update)
        .await
        .expect("second");
    let history = store.versions(&r.target_id).await.expect("versions");
    assert_eq!(history.len(), 1);
}

#[tokio::test]
async fn get_returns_none_for_tombstoned() {
    let store = open_in_memory().await.expect("open");
    let r = cairn_core::domain::record::tests_export::sample_record();
    store.upsert(&r).await.expect("upsert");
    store
        .tombstone(&r.id, TombstoneReason::Expire)
        .await
        .expect("tombstone");
    let got = store.get(&r.id).await.expect("get");
    assert!(got.is_none(), "tombstoned rows must not be returned by get");
}
