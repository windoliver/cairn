// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

//! `activate_version` for a non-existent version number must return
//! `StoreError::NotFound`.

use cairn_core::contract::memory_store::{
    apply::MemoryStoreApply, error::StoreError, types::TargetId,
};
use cairn_core::wal::test_apply_token;
use cairn_store_sqlite::SqliteMemoryStore;
use tempfile::tempdir;

/// Calling `activate_version` with a version number that has never been staged
/// must return `StoreError::NotFound`.
#[tokio::test]
async fn activate_nonexistent_version_returns_not_found() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    let target = TargetId::new("activate-nonexistent-target");

    let result = store
        .with_apply_tx(test_apply_token(), {
            let t = target.clone();
            move |tx| {
                // Version 999 was never staged.
                tx.activate_version(&t, 999, None, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))
            }
        })
        .await;

    assert!(
        matches!(result, Err(StoreError::NotFound(_))),
        "expected StoreError::NotFound for non-existent version; got: {result:?}"
    );
}
