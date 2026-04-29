//! Regression: a `Default`-constructed `SqliteMemoryStore` is a probe
//! used by `register_plugin!` for capability/manifest discovery only. It
//! must reject every storage-touching trait call, so production code that
//! mistakenly resolves the registry-bound `Arc<dyn MemoryStore>` cannot
//! silently get an in-memory database whose state evaporates on restart.

use cairn_core::contract::memory_store::{ListQuery, MemoryStore, StoreError, TargetId};
use cairn_core::domain::{Principal, identity::Identity};
use cairn_store_sqlite::SqliteMemoryStore;

fn principal() -> Principal {
    let id = Identity::parse("agt:claude-code:opus-4-7:main:v1").expect("identity");
    Principal::from_identity(id)
}

fn target() -> TargetId {
    TargetId::new("rec/probe-test")
}

#[tokio::test]
async fn probe_get_rejects_with_invariant() {
    let store = SqliteMemoryStore::default();
    let err = store
        .get(&principal(), &target())
        .await
        .expect_err("probe must reject get");
    assert!(matches!(err, StoreError::Invariant(_)), "got {err:?}");
}

#[tokio::test]
async fn probe_list_rejects_with_invariant() {
    let store = SqliteMemoryStore::default();
    let q = ListQuery::new(principal());
    let err = store.list(&q).await.expect_err("probe must reject list");
    assert!(matches!(err, StoreError::Invariant(_)), "got {err:?}");
}

#[tokio::test]
async fn probe_version_history_rejects_with_invariant() {
    let store = SqliteMemoryStore::default();
    let err = store
        .version_history(&principal(), &target())
        .await
        .expect_err("probe must reject version_history");
    assert!(matches!(err, StoreError::Invariant(_)), "got {err:?}");
}
