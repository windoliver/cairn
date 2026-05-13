//! `with_tx` rolls back on `Err`, commits on `Ok`.

use cairn_core::contract::memory_store::MemoryStore;
use cairn_store_sqlite::{StoreError, open_in_memory};

#[tokio::test]
async fn ok_commits() {
    let store = open_in_memory().await.expect("open");
    let r = cairn_core::domain::record::tests_export::sample_record();
    store
        .with_tx(move |tx| {
            tx.upsert(&r)?;
            Ok::<_, StoreError>(())
        })
        .await
        .expect("with_tx");

    let r2 = cairn_core::domain::record::tests_export::sample_record();
    let got = store.get(&r2.id).await.expect("get");
    assert!(got.is_some(), "tx commit must persist the upsert");
}

#[tokio::test]
async fn err_rolls_back() {
    let store = open_in_memory().await.expect("open");
    let r = cairn_core::domain::record::tests_export::sample_record();
    let result = store
        .with_tx(move |tx| {
            tx.upsert(&r)?;
            Err::<(), _>(StoreError::Invariant {
                what: "test rollback".into(),
            })
        })
        .await;
    assert!(result.is_err());

    let r2 = cairn_core::domain::record::tests_export::sample_record();
    let got = store.get(&r2.id).await.expect("get");
    assert!(got.is_none(), "tx rollback must not persist the upsert");
}
