//! `upsert` rejects structurally malformed records before touching the row
//! store. Without this gate, malformed records persist and subsequent
//! `record_json` deserialization fails, leaving rows the store cannot
//! surface.

use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::domain::MemoryRecord;
use cairn_store_sqlite::{StoreError, open_in_memory};

fn sample() -> MemoryRecord {
    cairn_core::domain::record::tests_export::sample_record()
}

#[tokio::test]
async fn upsert_rejects_empty_body() {
    let store = open_in_memory().await.expect("open");
    let mut r = sample();
    r.body.clear();
    let err = store.upsert(&r).await.expect_err("must reject empty body");
    let downcast = err
        .downcast::<StoreError>()
        .expect("expected adapter StoreError");
    assert!(
        matches!(*downcast, StoreError::InvalidRecord(_)),
        "expected InvalidRecord, got {downcast:?}"
    );
}

#[tokio::test]
async fn upsert_rejects_out_of_range_salience() {
    let store = open_in_memory().await.expect("open");
    let mut r = sample();
    r.salience = 1.5;
    let err = store
        .upsert(&r)
        .await
        .expect_err("must reject salience > 1.0");
    let downcast = err
        .downcast::<StoreError>()
        .expect("expected adapter StoreError");
    assert!(
        matches!(*downcast, StoreError::InvalidRecord(_)),
        "expected InvalidRecord, got {downcast:?}"
    );
}

#[tokio::test]
async fn upsert_rejects_nan_confidence() {
    let store = open_in_memory().await.expect("open");
    let mut r = sample();
    r.confidence = f32::NAN;
    let err = store.upsert(&r).await.expect_err("must reject NaN");
    let downcast = err
        .downcast::<StoreError>()
        .expect("expected adapter StoreError");
    assert!(
        matches!(*downcast, StoreError::InvalidRecord(_)),
        "expected InvalidRecord, got {downcast:?}"
    );
}
