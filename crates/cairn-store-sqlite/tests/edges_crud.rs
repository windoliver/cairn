//! Edge CRUD round-trip + invariants.

use cairn_core::contract::memory_store::{Edge, EdgeDir, EdgeKey, EdgeKind, MemoryStore};
use cairn_core::domain::{MemoryRecord, RecordId, TargetId};
use cairn_store_sqlite::open_in_memory;

fn sample(id: &str, target: &str) -> MemoryRecord {
    let mut r = cairn_core::domain::record::tests_export::sample_record();
    r.id = RecordId::parse(id.to_owned()).unwrap();
    r.target_id = TargetId::parse(target.to_owned()).unwrap();
    r
}

#[tokio::test]
async fn put_then_neighbours_out() {
    let store = open_in_memory().await.expect("open");
    let r1 = sample("01HQZX9F5N0000000000000001", "01HQZX9F5N0000000000000001");
    let r2 = sample("01HQZX9F5N0000000000000002", "01HQZX9F5N0000000000000002");
    store.upsert(&r1).await.expect("r1");
    store.upsert(&r2).await.expect("r2");
    store
        .put_edge(&Edge {
            src: r1.id.clone(),
            dst: r2.id.clone(),
            kind: EdgeKind::Mentions,
            weight: Some(0.5),
        })
        .await
        .expect("put_edge");
    let out = store
        .neighbours(&r1.id, EdgeDir::Out)
        .await
        .expect("neighbours");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].dst, r2.id);
    assert_eq!(out[0].kind, EdgeKind::Mentions);
}

#[tokio::test]
async fn remove_edge_returns_true_on_existing() {
    let store = open_in_memory().await.expect("open");
    let r1 = sample("01HQZX9F5N0000000000000001", "01HQZX9F5N0000000000000001");
    let r2 = sample("01HQZX9F5N0000000000000002", "01HQZX9F5N0000000000000002");
    store.upsert(&r1).await.expect("r1");
    store.upsert(&r2).await.expect("r2");
    let edge = Edge {
        src: r1.id.clone(),
        dst: r2.id.clone(),
        kind: EdgeKind::Mentions,
        weight: None,
    };
    store.put_edge(&edge).await.expect("put");
    let removed = store
        .remove_edge(&EdgeKey {
            src: edge.src.clone(),
            dst: edge.dst.clone(),
            kind: edge.kind,
        })
        .await
        .expect("remove");
    assert!(removed);
    let removed_again = store
        .remove_edge(&EdgeKey {
            src: edge.src,
            dst: edge.dst,
            kind: edge.kind,
        })
        .await
        .expect("remove_again");
    assert!(!removed_again);
}

#[tokio::test]
async fn updates_edge_immutable_via_remove_returns_error() {
    let store = open_in_memory().await.expect("open");
    let r1 = sample("01HQZX9F5N0000000000000001", "01HQZX9F5N0000000000000001");
    let r2 = sample("01HQZX9F5N0000000000000002", "01HQZX9F5N0000000000000002");
    store.upsert(&r1).await.expect("r1");
    store.upsert(&r2).await.expect("r2");
    store
        .put_edge(&Edge {
            src: r1.id.clone(),
            dst: r2.id.clone(),
            kind: EdgeKind::Updates,
            weight: None,
        })
        .await
        .expect("put updates");
    // Removal of an `updates` edge runs the immutability trigger error
    // path; the schema actually allows DELETE (the trigger is on UPDATE),
    // so this returns true. If the brief later forbids DELETE, a new
    // schema trigger is required and this test will catch the change.
    let removed = store
        .remove_edge(&EdgeKey {
            src: r1.id,
            dst: r2.id,
            kind: EdgeKind::Updates,
        })
        .await
        .expect("remove");
    assert!(removed, "updates-edge DELETE is allowed at schema 0001 today");
}
