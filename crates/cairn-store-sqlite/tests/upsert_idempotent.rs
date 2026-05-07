//! Upsert idempotency, version bumps, and content-changed accounting.
//!
//! Pins the three branches of the `do_upsert` state machine:
//!
//! 1. fresh `target_id` → version 1, `content_changed = true`, no prior hash;
//! 2. identical body re-submitted → no version bump, `content_changed = false`,
//!    `prior_hash` echoed back so callers can confirm the dedupe was made
//!    against the body they expected;
//! 3. body mutation → version bumped to 2, `content_changed = true`,
//!    `prior_hash` carries the previous active row's hash.

use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::domain::{BodyHash, MemoryRecord};
use cairn_store_sqlite::open_in_memory;

fn sample() -> MemoryRecord {
    cairn_core::domain::record::tests_export::sample_record()
}

#[tokio::test]
async fn first_upsert_is_v1() {
    let store = open_in_memory().await.expect("open");
    let r = sample();
    let out = store.upsert(&r).await.expect("upsert");
    assert_eq!(out.version, 1);
    assert!(out.content_changed);
    assert!(out.prior_hash.is_none());
    assert_eq!(out.record_id, r.id);
    assert_eq!(out.target_id, r.target_id);
}

#[tokio::test]
async fn second_upsert_same_body_is_noop() {
    let store = open_in_memory().await.expect("open");
    let r = sample();
    store.upsert(&r).await.expect("first");
    let out = store.upsert(&r).await.expect("second");
    assert_eq!(out.version, 1, "no version bump on identical body");
    assert!(!out.content_changed);
    assert_eq!(out.prior_hash, Some(BodyHash::compute(&r.body)));
}

#[tokio::test]
async fn upsert_with_different_body_bumps_version() {
    let store = open_in_memory().await.expect("open");
    let r = sample();
    store.upsert(&r).await.expect("first");
    let mut r2 = r.clone();
    r2.body = "second body".to_owned();
    let out = store.upsert(&r2).await.expect("second");
    assert_eq!(out.version, 2);
    assert!(out.content_changed);
    assert_eq!(out.prior_hash, Some(BodyHash::compute(&r.body)));
}
