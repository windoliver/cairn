//! Integration: `ExpirationHandler` tombstones TTL-aged records and
//! leaves fresh records active. AC#2 of issue #91 — "expiration marks
//! records retired and removes them from default reads" (the
//! `MemoryStore::list` contract filters tombstoned rows for us).
//!
//! `sample_record` from `cairn-test-fixtures` stamps every record at
//! `2026-04-22T14:05:11Z`. We pick a `now_ms` further in the future
//! and a tight TTL so the sample's `updated_at` is past expiry without
//! having to backdate the timestamps (which would trip
//! `provenance.created_at <= updated_at` validation).

use std::sync::Arc;

use cairn_core::config::ExpirationConfig;
use cairn_core::contract::memory_store::{ListArgs, MemoryStore};
use cairn_test_fixtures::{memstore, sample_record};
use cairn_workflows::scheduler::{HandlerOutcome, JobHandler};
use cairn_workflows::{ExpirationHandler, ExpirationPayload};

/// `2027-01-01T00:00:00Z` in epoch millis — well past every
/// `sample_record`'s stamped `updated_at` (`2026-04-22T14:05:11Z`).
const FUTURE_NOW_MS: i64 = 1_798_761_600_000;

#[tokio::test]
async fn ttl_sweep_tombstones_aged_records() {
    let store = Arc::new(memstore().await);
    // Seed three records — all from the fixture (so all have the same
    // `2026-04-22T14:05:11Z` updated_at). With `ttl_days = 1` and
    // `now_ms = 2027-01-01` every one of them expires.
    store.upsert(&sample_record(1)).await.expect("seed 1");
    store.upsert(&sample_record(2)).await.expect("seed 2");
    store.upsert(&sample_record(3)).await.expect("seed 3");

    let dyn_store: Arc<dyn MemoryStore> = store.clone();
    let handler = ExpirationHandler::new(
        dyn_store,
        ExpirationConfig {
            enabled: true,
            ttl_days: 1,
            salience_floor: 0.0,
            batch_size: 16,
        },
    );
    let payload = ExpirationPayload {
        now_ms: FUTURE_NOW_MS,
        bound_scope: None,
    };
    let bytes = payload.to_bytes().expect("encode");
    let outcome = handler.handle(&bytes).await;
    assert!(
        matches!(outcome, HandlerOutcome::Done),
        "expected Done, got {outcome:?}"
    );

    // Every aged record is now tombstoned — default reads must skip
    // them. The `list` contract filters tombstoned rows for us.
    let listed = store
        .list(&ListArgs {
            limit: 100,
            ..ListArgs::default()
        })
        .await
        .expect("list");
    let bodies: Vec<&str> = listed.records.iter().map(|r| r.body.as_str()).collect();
    assert!(
        bodies.is_empty(),
        "every aged record must be excluded; still active: {bodies:?}"
    );
}

#[tokio::test]
async fn fresh_records_survive_a_long_ttl_sweep() {
    // Same fixture, but with `ttl_days = 365` and `now_ms` just after
    // the fixture's `updated_at` — every record is well within the
    // TTL window so none expire.
    let store = Arc::new(memstore().await);
    store.upsert(&sample_record(4)).await.expect("seed 4");
    store.upsert(&sample_record(5)).await.expect("seed 5");

    let dyn_store: Arc<dyn MemoryStore> = store.clone();
    let handler = ExpirationHandler::new(
        dyn_store,
        ExpirationConfig {
            enabled: true,
            ttl_days: 365,
            salience_floor: 0.0,
            batch_size: 16,
        },
    );
    // 2026-04-22T15:00:00Z — minutes after the fixture's stamp.
    let payload = ExpirationPayload {
        now_ms: 1_777_215_600_000,
        bound_scope: None,
    };
    let bytes = payload.to_bytes().expect("encode");
    let outcome = handler.handle(&bytes).await;
    assert!(matches!(outcome, HandlerOutcome::Done));

    let listed = store
        .list(&ListArgs {
            limit: 100,
            ..ListArgs::default()
        })
        .await
        .expect("list");
    assert_eq!(listed.records.len(), 2, "fresh records must survive");
}

#[tokio::test]
async fn sweep_is_idempotent_when_replayed() {
    let store = Arc::new(memstore().await);
    store.upsert(&sample_record(7)).await.expect("seed aged");

    let dyn_store: Arc<dyn MemoryStore> = store.clone();
    let handler = ExpirationHandler::new(
        dyn_store,
        ExpirationConfig {
            enabled: true,
            ttl_days: 1,
            salience_floor: 0.0,
            batch_size: 16,
        },
    );
    let payload = ExpirationPayload {
        now_ms: FUTURE_NOW_MS,
        bound_scope: None,
    };
    let bytes = payload.to_bytes().expect("encode");

    let first = handler.handle(&bytes).await;
    let second = handler.handle(&bytes).await;
    assert!(matches!(first, HandlerOutcome::Done));
    // Second sweep against an already-empty active set is also Done —
    // tombstone is idempotent per the MemoryStore contract.
    assert!(matches!(second, HandlerOutcome::Done));

    let listed = store
        .list(&ListArgs {
            limit: 100,
            ..ListArgs::default()
        })
        .await
        .expect("list");
    assert!(
        listed.records.is_empty(),
        "every aged row must be tombstoned"
    );
}

#[tokio::test]
async fn disabled_config_keeps_records_alive() {
    let store = Arc::new(memstore().await);
    store.upsert(&sample_record(11)).await.expect("seed aged");

    let dyn_store: Arc<dyn MemoryStore> = store.clone();
    let handler = ExpirationHandler::new(
        dyn_store,
        ExpirationConfig {
            enabled: false,
            ttl_days: 1,
            salience_floor: 0.0,
            batch_size: 16,
        },
    );
    let payload = ExpirationPayload {
        now_ms: FUTURE_NOW_MS,
        bound_scope: None,
    };
    let bytes = payload.to_bytes().expect("encode");
    let outcome = handler.handle(&bytes).await;
    assert!(matches!(outcome, HandlerOutcome::Permanent { .. }));

    let listed = store
        .list(&ListArgs {
            limit: 100,
            ..ListArgs::default()
        })
        .await
        .expect("list");
    assert!(
        !listed.records.is_empty(),
        "disabled workflow must not tombstone"
    );
}
