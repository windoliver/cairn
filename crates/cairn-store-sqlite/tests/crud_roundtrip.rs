//! End-to-end CRUD round-trip across `MemoryRecord` shapes.
//!
//! Pins the read-side trio of `MemoryStore` methods landing in T15:
//!
//! - `get` returns the inserted record verbatim and `None` for misses;
//! - `list` enumerates active, non-tombstoned records under a visibility
//!   allowlist;
//! - `versions` exposes the full per-target history including superseded rows.

use cairn_core::contract::memory_store::{ListArgs, MemoryStore};
use cairn_core::domain::{MemoryRecord, ScopeTuple};
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
    // After persistence, consent_model carries the resolved value
    // (the authoritative column), not the caller's `None` "store
    // decides" sentinel. Stamp the expected value onto the input
    // before equality to reflect the post-store canonical form
    // (Issue #253: record_json must agree with the hot column).
    let mut expected = r.clone();
    expected.consent_model = Some(cairn_core::domain::consent_timeline::ConsentModel::LegacyEvent);
    assert_eq!(got, expected);
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
async fn list_filters_by_scope_and_visibility_in_store_query() {
    let store = open_in_memory().await.expect("open");
    let mut in_scope = base();
    in_scope.id = cairn_core::domain::RecordId::parse("01HQZX9F5N0000000000000010").expect("valid");
    in_scope.target_id =
        cairn_core::domain::TargetId::parse("01HQZX9F5N0000000000000010").expect("valid");
    in_scope.scope = ScopeTuple {
        tenant: Some("default".to_owned()),
        workspace: Some("test-vault".to_owned()),
        entity: Some("ingest".to_owned()),
        ..ScopeTuple::default()
    };
    in_scope.visibility = cairn_core::domain::MemoryVisibility::Private;

    let mut wrong_scope = base();
    wrong_scope.id =
        cairn_core::domain::RecordId::parse("01HQZX9F5N0000000000000011").expect("valid");
    wrong_scope.target_id =
        cairn_core::domain::TargetId::parse("01HQZX9F5N0000000000000011").expect("valid");
    wrong_scope.scope = ScopeTuple {
        tenant: Some("other".to_owned()),
        workspace: Some("test-vault".to_owned()),
        entity: Some("ingest".to_owned()),
        ..ScopeTuple::default()
    };
    wrong_scope.visibility = cairn_core::domain::MemoryVisibility::Private;

    let mut wrong_visibility = base();
    wrong_visibility.id =
        cairn_core::domain::RecordId::parse("01HQZX9F5N0000000000000012").expect("valid");
    wrong_visibility.target_id =
        cairn_core::domain::TargetId::parse("01HQZX9F5N0000000000000012").expect("valid");
    wrong_visibility.scope = in_scope.scope.clone();
    wrong_visibility.visibility = cairn_core::domain::MemoryVisibility::Public;

    store.upsert(&in_scope).await.expect("upsert in_scope");
    store
        .upsert(&wrong_scope)
        .await
        .expect("upsert wrong_scope");
    store
        .upsert(&wrong_visibility)
        .await
        .expect("upsert wrong_visibility");

    let page = store
        .list(&ListArgs {
            scope: Some(in_scope.scope.clone()),
            visibility_allowlist: vec![cairn_core::domain::MemoryVisibility::Private],
            limit: 10,
            ..ListArgs::default()
        })
        .await
        .expect("list");

    assert_eq!(page.records.len(), 1);
    assert_eq!(page.records[0].id, in_scope.id);
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
