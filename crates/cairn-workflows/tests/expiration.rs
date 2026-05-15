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
use cairn_core::contract::job_store::JobStore;
use cairn_core::contract::memory_store::{ListArgs, MemoryStore};
use cairn_test_fixtures::{memstore, sample_record};
use cairn_workflows::scheduler::{HandlerOutcome, JobHandler};
use cairn_workflows::{ExpirationHandler, ExpirationPayload, SqliteJobStore};
use tempfile::tempdir;

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
        cursor: None,
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
        cursor: None,
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
        cursor: None,
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
async fn capped_sweep_enqueues_continuation_to_drain_old_records() {
    // Round-8 adversarial review #1: a fresh-prefix vault with
    // more than `batch_size` records must NOT silently starve old
    // records. With `with_job_store` wired the handler enqueues a
    // continuation row whenever the inspect-cap fires before the
    // store cursor exhausts.
    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("cairn.db");
    let mem = Arc::new(
        cairn_store_sqlite::open(&db_path)
            .await
            .expect("open memory"),
    );
    let jobs_conn = cairn_store_sqlite::open_sync(&db_path).expect("open jobs conn");
    let jobs: Arc<dyn JobStore> = Arc::new(SqliteJobStore::new(jobs_conn).expect("jobs"));

    // Seed 5 records; batch_size = 2 so a single sweep cannot
    // exhaust the cursor.
    for i in 0..5_u64 {
        mem.upsert(&sample_record(200 + i)).await.expect("seed");
    }

    let dyn_store: Arc<dyn MemoryStore> = mem.clone();
    let handler = ExpirationHandler::with_job_store(
        dyn_store,
        ExpirationConfig {
            enabled: true,
            ttl_days: 365, // none of the seeds expire by TTL today
            salience_floor: 0.0,
            batch_size: 2,
        },
        jobs.clone(),
    );
    let payload = ExpirationPayload {
        now_ms: 1_777_215_600_000, // 2026-04-22T15:00:00Z — minutes after fixture stamp
        bound_scope: None,
        cursor: None,
    };
    let bytes = payload.to_bytes().expect("encode");
    let outcome = handler.handle(&bytes).await;
    assert!(matches!(outcome, HandlerOutcome::Done));

    // A continuation job must now be queued for the next sweep.
    let leased = jobs
        .lease("test-lease", 1_777_215_600_000, 30_000)
        .await
        .expect("lease");
    let leased = leased.expect("capped sweep must enqueue a continuation when JobStore is wired");
    assert_eq!(leased.kind.as_str(), "expiration.sweep");
    assert!(
        leased
            .job_id
            .as_str()
            .starts_with("expiration:continuation:"),
        "continuation job_id has stable prefix; got {}",
        leased.job_id.as_str()
    );
}

#[tokio::test]
async fn capped_sweep_without_job_store_is_documented_single_shot() {
    // Counterpart to the above: `new()` is single-shot. We assert
    // the documented behaviour: when the cap fires without a
    // JobStore the sweep still returns Done (no continuation
    // available). Production paths MUST use `with_job_store`.
    let store = Arc::new(memstore().await);
    for i in 0..3_u64 {
        store.upsert(&sample_record(300 + i)).await.expect("seed");
    }
    let dyn_store: Arc<dyn MemoryStore> = store.clone();
    let handler = ExpirationHandler::new(
        dyn_store,
        ExpirationConfig {
            enabled: true,
            ttl_days: 365,
            salience_floor: 0.0,
            batch_size: 1,
        },
    );
    let payload = ExpirationPayload {
        now_ms: 1_777_215_600_000,
        bound_scope: None,
        cursor: None,
    };
    let bytes = payload.to_bytes().expect("encode");
    let outcome = handler.handle(&bytes).await;
    assert!(matches!(outcome, HandlerOutcome::Done));
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
        cursor: None,
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
