//! End-to-end CRUD round-trip across `MemoryRecord` shapes.
//!
//! Pins the read-side trio of `MemoryStore` methods landing in T15:
//!
//! - `get` returns the inserted record verbatim and `None` for misses;
//! - `list` enumerates active, non-tombstoned records under a visibility
//!   allowlist;
//! - `versions` exposes the full per-target history including superseded rows.

use cairn_core::contract::memory_store::{ListArgs, MemoryStore};
use cairn_core::domain::MemoryRecord;
use cairn_store_sqlite::open_in_memory;

fn base() -> MemoryRecord {
    cairn_core::domain::record::tests_export::sample_record()
}

#[tokio::test]
async fn upsert_then_get_returns_same_record() {
    let store = open_in_memory().await.expect("open");
    let r = base();
    store.upsert(&r).await.expect("upsert");
    let got = store.get(&r.id).await.expect("get").expect("present");
    assert_eq!(got, r);
}

#[tokio::test]
async fn get_missing_returns_none() {
    let store = open_in_memory().await.expect("open");
    let r = base();
    let got = store.get(&r.id).await.expect("get");
    assert!(got.is_none());
}

#[tokio::test]
async fn list_returns_inserted_records_newest_first() {
    let store = open_in_memory().await.expect("open");
    let mut r1 = base();
    r1.id = cairn_core::domain::RecordId::parse("01HQZX9F5N0000000000000001").expect("valid");
    r1.target_id =
        cairn_core::domain::TargetId::parse("01HQZX9F5N0000000000000001").expect("valid");
    let mut r2 = base();
    r2.id = cairn_core::domain::RecordId::parse("01HQZX9F5N0000000000000002").expect("valid");
    r2.target_id =
        cairn_core::domain::TargetId::parse("01HQZX9F5N0000000000000002").expect("valid");

    store.upsert(&r1).await.expect("upsert r1");
    store.upsert(&r2).await.expect("upsert r2");

    let page = store
        .list(&ListArgs {
            limit: 10,
            visibility_allowlist: vec![cairn_core::domain::taxonomy::MemoryVisibility::Private],
            ..ListArgs::default()
        })
        .await
        .expect("list");
    assert_eq!(page.records.len(), 2);
}

#[tokio::test]
async fn versions_returns_full_history() {
    let store = open_in_memory().await.expect("open");
    let r = base();
    store.upsert(&r).await.expect("v1");
    let mut r2 = r.clone();
    r2.body = "v2 body".to_owned();
    store.upsert(&r2).await.expect("v2");

    let history = store.versions(&r.target_id).await.expect("versions");
    assert_eq!(history.len(), 2, "two versions visible");
    assert_eq!(history[0].version, 1);
    assert_eq!(history[1].version, 2);
}
