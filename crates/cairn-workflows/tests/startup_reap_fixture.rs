//! Issue #92 — `Scheduler::start` must run one `reap_expired` before
//! spawning workers so a crashed predecessor's expired lease is
//! reclaimed without waiting for the periodic reaper tick (spec §4.7).
//!
//! `worker_count = 0` forces zero worker tasks to spawn, so any
//! post-`start()` lease success proves the reap happened inside
//! `start()` itself rather than in the worker poll loop.

use std::sync::Arc;

use cairn_core::contract::job_store::{EnqueueRequest, JobId, JobKind, JobStore, RetryPolicy};
use cairn_workflows::SqliteJobStore;
use cairn_workflows::scheduler::{
    Clock, HandlerRegistry, MockClock, ReaperConfig, Scheduler, SchedulerConfig, WorkerConfig,
};
use rusqlite::Connection;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_reaps_expired_lease_before_workers_run() {
    let conn = Connection::open_in_memory().expect("open in-memory");
    cairn_workflows::sqlite_store::install_for_tests(&conn);
    let store: Arc<dyn JobStore> = Arc::new(SqliteJobStore::new(conn).expect("init store"));

    // Enqueue + lease + heartbeat so the row is in `state = 'leased'`
    // with a known expiry. We deliberately don't fail/complete: the
    // simulated crash is "the worker holding this lease vanished".
    store
        .enqueue(EnqueueRequest {
            job_id: JobId::new("j-orphan"),
            kind: JobKind::new("test.reap"),
            payload: vec![],
            queue_key: None,
            dedupe_key: None,
            not_before_ms: 0,
            retry: RetryPolicy::DEFAULT,
        })
        .await
        .expect("enqueue");
    let leased = store
        .lease("predecessor", 1_000, 100)
        .await
        .expect("lease")
        .expect("leased some");
    // Heartbeat is intentionally skipped so `lease_started = 0` —
    // this is the "crashed before first heartbeat" shape.

    // worker_count = 0: no workers spawn, so the only thing that can
    // touch the row inside Scheduler::start() is the startup reap.
    // reaper.interval_ms is set to a value far past the test's
    // lifetime so the periodic reaper never ticks either.
    let config = SchedulerConfig {
        worker_count: 0,
        worker: WorkerConfig::default(),
        reaper: ReaperConfig {
            interval_ms: 60_000,
        },
        ..SchedulerConfig::default()
    };
    let registry = HandlerRegistry::default();
    // Pick a wall-clock far past the lease expiry (1_000 + 100 = 1_100)
    // so `reap_expired(now)` reclaims the orphan.
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(10_000));
    let scheduler = Scheduler::start("inc-test", store.clone(), &registry, clock, config).await;

    // The row must be lease-eligible without any worker or reaper
    // tick — proves the reap ran inside start().
    let again = store
        .lease("post-reap-worker", 11_000, 100)
        .await
        .expect("lease");
    assert!(
        again.is_some(),
        "row should be lease-eligible after startup reap"
    );
    scheduler.shutdown().await;
    let _ = leased; // hold the lease token across the assertion
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_succeeds_even_when_no_orphans_exist() {
    // Steady-state guarantee: an empty job store doesn't break the
    // startup reap; `Scheduler::start` returns normally.
    let conn = Connection::open_in_memory().expect("open in-memory");
    cairn_workflows::sqlite_store::install_for_tests(&conn);
    let store: Arc<dyn JobStore> = Arc::new(SqliteJobStore::new(conn).expect("init store"));
    let config = SchedulerConfig {
        worker_count: 0,
        worker: WorkerConfig::default(),
        reaper: ReaperConfig {
            interval_ms: 60_000,
        },
        ..SchedulerConfig::default()
    };
    let registry = HandlerRegistry::default();
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(1));
    let scheduler = Scheduler::start("inc-empty", store, &registry, clock, config).await;
    scheduler.shutdown().await;
}
