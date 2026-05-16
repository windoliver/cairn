//! Issue #92 — the reaper (both startup and periodic) must report the
//! correct `WorkflowJobFailed.disposition` based on whether
//! `JobStore::reap_expired` terminated the row or requeued it. The
//! `ReclaimedRow.terminated` flag distinguishes:
//!
//! * `terminated = true`  → `disposition = "permanent"` + `will_retry_at_ms = None`
//! * `terminated = false` → `disposition = "retry"`     + `will_retry_at_ms = Some(now)`
//!
//! Two arms covered:
//!   1. Startup reap: a row whose worker crashed with retry budget
//!      remaining → requeue + disposition = "retry".
//!   2. Startup reap: a row whose worker crashed on its only allowed
//!      attempt → terminate + disposition = "permanent".

use std::sync::Arc;
use std::time::Duration;

use cairn_core::contract::job_store::{
    EnqueueRequest, FailureClass, JobId, JobKind, JobStore, RetryPolicy,
};
use cairn_core::contract::metrics::{CapturingMetricsSink, MetricsSink};
use cairn_core::domain::metrics::MetricEvent;
use cairn_workflows::SqliteJobStore;
use cairn_workflows::scheduler::{
    Clock, HandlerRegistry, MockClock, ReaperConfig, Scheduler, SchedulerConfig, WorkerConfig,
};
use rusqlite::Connection;

fn store() -> Arc<dyn JobStore> {
    let conn = Connection::open_in_memory().expect("open in-memory");
    cairn_workflows::sqlite_store::install_for_tests(&conn);
    Arc::new(SqliteJobStore::new(conn).expect("init store"))
}

/// Poll the sink until predicate is satisfied or deadline expires; return final snapshot.
async fn wait_for(
    sink: &CapturingMetricsSink,
    predicate: impl Fn(&[MetricEvent]) -> bool,
    deadline: Duration,
) -> Vec<MetricEvent> {
    let start = std::time::Instant::now();
    loop {
        let snapshot = sink.snapshot().await;
        if predicate(&snapshot) || start.elapsed() >= deadline {
            return snapshot;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Arm 1: a row with retry budget remaining is requeued by the
/// reaper. The startup-reap emission must carry
/// `disposition = "retry"` + `will_retry_at_ms = Some(now)` +
/// `failure_class = "lease_lost"`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_reap_requeues_with_retry_disposition() {
    let store = store();

    // Enqueue + lease + heartbeat so the row is `leased`,
    // `lease_started = 1`, and has retry budget remaining.
    store
        .enqueue(EnqueueRequest {
            job_id: JobId::new("j-retry"),
            kind: JobKind::new("test.reap.retry"),
            payload: vec![],
            queue_key: None,
            dedupe_key: None,
            not_before_ms: 1_000,
            retry: RetryPolicy {
                max_attempts: 5,
                base_backoff_ms: 1,
                backoff_multiplier: 1,
                max_backoff_ms: 1,
            },
        })
        .await
        .expect("enqueue");
    let leased = store
        .lease("predecessor", 1_000, 100)
        .await
        .expect("lease")
        .expect("leased some");
    store
        .heartbeat(&leased.job_id, &leased.lease, 1_050, 1_100)
        .await
        .expect("heartbeat marks started");
    drop(leased); // simulate crash

    let sink = Arc::new(CapturingMetricsSink::new());
    let metrics: Arc<dyn MetricsSink> = sink.clone();
    let config = SchedulerConfig {
        worker_count: 0, // no workers — only the startup reap touches the row
        worker: WorkerConfig::default(),
        reaper: ReaperConfig {
            interval_ms: 60_000, // never tick within the test
        },
        metrics,
    };
    let registry = HandlerRegistry::default();
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(10_000));

    let scheduler = Scheduler::start("inc-retry", store.clone(), &registry, clock, config).await;

    let events = wait_for(
        sink.as_ref(),
        |evs| {
            evs.iter()
                .any(|e| matches!(e, MetricEvent::WorkflowJobFailed { .. }))
        },
        Duration::from_secs(2),
    )
    .await;
    scheduler.shutdown().await;

    let failed = events
        .iter()
        .find_map(|e| {
            if let MetricEvent::WorkflowJobFailed {
                disposition,
                failure_class,
                will_retry_at_ms,
                job_id,
                ..
            } = e
            {
                Some((
                    disposition.clone(),
                    failure_class.clone(),
                    *will_retry_at_ms,
                    job_id.clone(),
                ))
            } else {
                None
            }
        })
        .expect("WorkflowJobFailed must be emitted by the startup reap");

    assert_eq!(failed.0, "retry", "non-exhausted reclaim must be \"retry\"");
    assert_eq!(
        failed.1,
        FailureClass::LeaseLost.as_str(),
        "reaper must stamp lease_lost class"
    );
    assert!(
        failed.2.is_some(),
        "retry disposition must surface will_retry_at_ms"
    );
    assert_eq!(failed.3, "j-retry");
}

/// Arm 2: a row whose worker crashed on its only allowed attempt is
/// terminated by the reaper (transitions to `state = 'failed'`). The
/// startup-reap emission must carry `disposition = "permanent"` +
/// `will_retry_at_ms = None`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_reap_terminates_with_permanent_disposition() {
    let store = store();

    // Enqueue with `max_attempts = 1`; the heartbeat consumes the
    // only allowed attempt. When the lease then expires, the reaper
    // must terminate (not requeue).
    store
        .enqueue(EnqueueRequest {
            job_id: JobId::new("j-doomed"),
            kind: JobKind::new("test.reap.permanent"),
            payload: vec![],
            queue_key: None,
            dedupe_key: None,
            not_before_ms: 1_000,
            retry: RetryPolicy {
                max_attempts: 1,
                ..RetryPolicy::DEFAULT
            },
        })
        .await
        .expect("enqueue");
    let leased = store
        .lease("predecessor", 1_000, 100)
        .await
        .expect("lease")
        .expect("leased some");
    store
        .heartbeat(&leased.job_id, &leased.lease, 1_050, 1_100)
        .await
        .expect("heartbeat marks started (consumes attempt 1/1)");
    drop(leased); // simulate crash after start

    let sink = Arc::new(CapturingMetricsSink::new());
    let metrics: Arc<dyn MetricsSink> = sink.clone();
    let config = SchedulerConfig {
        worker_count: 0,
        worker: WorkerConfig::default(),
        reaper: ReaperConfig {
            interval_ms: 60_000,
        },
        metrics,
    };
    let registry = HandlerRegistry::default();
    // Clock far past lease expiry so reap_expired reclaims.
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(10_000));

    let scheduler =
        Scheduler::start("inc-permanent", store.clone(), &registry, clock, config).await;

    let events = wait_for(
        sink.as_ref(),
        |evs| {
            evs.iter()
                .any(|e| matches!(e, MetricEvent::WorkflowJobFailed { .. }))
        },
        Duration::from_secs(2),
    )
    .await;
    scheduler.shutdown().await;

    let failed = events
        .iter()
        .find_map(|e| {
            if let MetricEvent::WorkflowJobFailed {
                disposition,
                failure_class,
                will_retry_at_ms,
                job_id,
                ..
            } = e
            {
                Some((
                    disposition.clone(),
                    failure_class.clone(),
                    *will_retry_at_ms,
                    job_id.clone(),
                ))
            } else {
                None
            }
        })
        .expect("WorkflowJobFailed must be emitted by the startup reap");

    assert_eq!(
        failed.0, "permanent",
        "exhausted reclaim must be \"permanent\""
    );
    assert_eq!(
        failed.1,
        FailureClass::LeaseLost.as_str(),
        "reaper must stamp lease_lost class even on terminal reclaim"
    );
    assert!(
        failed.2.is_none(),
        "permanent disposition must NOT surface will_retry_at_ms (was {:?})",
        failed.2,
    );
    assert_eq!(failed.3, "j-doomed");
}

/// Unit-level: `ReclaimedRow.terminated` round-trips through the
/// `SQLite` adapter for both arms. Belt-and-suspenders against future
/// drift between the store and the scheduler's disposition mapping.
#[tokio::test]
async fn reap_expired_surfaces_terminated_flag_per_row() {
    let store = store();

    // Two rows: one with budget remaining (will requeue), one exhausted.
    store
        .enqueue(EnqueueRequest {
            job_id: JobId::new("j-budget"),
            kind: JobKind::new("test.flag"),
            payload: vec![],
            queue_key: None,
            dedupe_key: None,
            not_before_ms: 1_000,
            retry: RetryPolicy {
                max_attempts: 5,
                base_backoff_ms: 1,
                backoff_multiplier: 1,
                max_backoff_ms: 1,
            },
        })
        .await
        .expect("enqueue budget");
    store
        .enqueue(EnqueueRequest {
            job_id: JobId::new("j-doomed-2"),
            kind: JobKind::new("test.flag"),
            payload: vec![],
            queue_key: None,
            dedupe_key: None,
            not_before_ms: 1_000,
            retry: RetryPolicy {
                max_attempts: 1,
                ..RetryPolicy::DEFAULT
            },
        })
        .await
        .expect("enqueue doomed");

    // Lease + heartbeat both so `lease_started = 1` and (for j-doomed)
    // attempts consumed. Heartbeats must never shrink the deadline, so
    // pick deadlines comfortably past the prior lease expiry.
    let leased_budget = store
        .lease("w", 1_000, 50)
        .await
        .expect("lease 1")
        .expect("budget");
    store
        .heartbeat(&leased_budget.job_id, &leased_budget.lease, 1_010, 1_200)
        .await
        .expect("hb budget");
    let leased_doom = store
        .lease("w", 1_010, 50)
        .await
        .expect("lease 2")
        .expect("doom");
    store
        .heartbeat(&leased_doom.job_id, &leased_doom.lease, 1_020, 1_200)
        .await
        .expect("hb doom");
    drop((leased_budget, leased_doom));

    let mut reclaimed = store.reap_expired(10_000).await.expect("reap");
    reclaimed.sort_by(|a, b| a.job_id.as_str().cmp(b.job_id.as_str()));

    assert_eq!(reclaimed.len(), 2, "both leases must reap: {reclaimed:?}");
    let budget = reclaimed
        .iter()
        .find(|r| r.job_id.as_str() == "j-budget")
        .expect("budget row");
    let doomed = reclaimed
        .iter()
        .find(|r| r.job_id.as_str() == "j-doomed-2")
        .expect("doomed row");
    assert!(
        !budget.terminated,
        "row with remaining attempts must NOT be terminated: {budget:?}"
    );
    assert!(
        doomed.terminated,
        "exhausted row MUST be terminated: {doomed:?}"
    );
}

/// Regression for Codex adversarial-review HIGH finding: a terminal
/// reap (exhausted retries → `state = 'failed'`) must also stamp the
/// dead-letter columns (`failure_class`, `dead_letter_at_ms`) so the
/// row is visible to `WorkflowJobsReader::dead_letter_rows` and, via
/// it, the `workflow_health` lint check. Without this stamp the
/// reaper-terminated row would silently miss the lint surface even
/// though the `WorkflowJobFailed("permanent")` metric event fires —
/// violating the spec §4.10 contract that "repeated failures become
/// visible and actionable" through BOTH metrics AND lint.
#[tokio::test]
async fn terminal_reap_stamps_dead_letter_columns() {
    use cairn_core::contract::workflow_jobs::WorkflowJobsReader;
    // Use a tmpfile so the reader can open its own connection — both
    // the JobStore writer and the WorkflowJobsReader (which lint
    // consumes) must observe the same row through the same file.
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("cairn.db");
    let conn = Connection::open(&db_path).expect("open file db");
    cairn_workflows::sqlite_store::install_for_tests(&conn);
    let store = Arc::new(SqliteJobStore::new(conn).expect("init store"));
    let dyn_store: Arc<dyn JobStore> = store.clone();

    dyn_store
        .enqueue(EnqueueRequest {
            job_id: JobId::new("j-terminal-reap"),
            kind: JobKind::new("kind.reaped"),
            payload: vec![],
            queue_key: None,
            dedupe_key: None,
            not_before_ms: 0,
            retry: RetryPolicy {
                max_attempts: 1,
                ..RetryPolicy::DEFAULT
            },
        })
        .await
        .expect("enqueue");
    let leased = dyn_store
        .lease("w", 1_000, 50)
        .await
        .expect("lease")
        .expect("leased some");
    // Heartbeat so attempts == 1 == max_attempts → next reap terminates.
    dyn_store
        .heartbeat(&leased.job_id, &leased.lease, 1_010, 1_100)
        .await
        .expect("hb");
    drop(leased);

    let reclaimed = dyn_store.reap_expired(10_000).await.expect("reap");
    assert_eq!(reclaimed.len(), 1);
    assert!(reclaimed[0].terminated, "must be terminal: {reclaimed:?}");
    assert!(
        reclaimed[0].next_run_at_ms.is_none(),
        "terminal reaps must surface no next_run_at: {reclaimed:?}"
    );

    let reader_conn = rusqlite::Connection::open(&db_path).expect("reopen");
    let reader = cairn_store_sqlite::SqliteWorkflowJobsReader::new(reader_conn)
        .expect("reader needs migration 0062");
    let rows = reader.dead_letter_rows(10);
    assert_eq!(
        rows.len(),
        1,
        "terminal reap must produce exactly one dead-letter row: {rows:?}"
    );
    assert_eq!(rows[0].job_id.as_str(), "j-terminal-reap");
    assert_eq!(rows[0].failure_class, FailureClass::LeaseLost);
    assert!(
        rows[0].dead_letter_at_ms > 0,
        "dead_letter_at_ms must be populated: {rows:?}"
    );
    assert_eq!(reader.dead_letter_count(None), 1);
}
