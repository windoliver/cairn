//! `cairn admin workflow` — hidden developer/E2E diagnostic surface for
//! issue #92 verification. Drives the workflow scheduler against the
//! live vault DB so an operator can:
//!
//! 1. Force a dead-letter row (`run-failing`) — boots a scheduler with a
//!    handler that always returns `FailureClass::Transient` retry and
//!    a low `max_attempts`, then runs until the row dead-letters.
//! 2. Simulate a mid-flight crash (`simulate-crash`) — enqueues + leases
//!    + heartbeats a job, then exits without releasing the lease so the
//!    row stays in `state = 'leased'` with `lease_started = 1`.
//! 3. Reap an orphan from a previous run (`recover`) — boots a scheduler
//!    with no workers so the startup `reap_expired` pass runs once,
//!    proving crash-safety across restarts.
//!
//! All three paths use the same `Scheduler::start` + `JsonlMetricsSink`
//! wiring that `cairn mcp` uses, so the demonstrated behaviour matches
//! the production code path. Hidden from default `--help`.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use cairn_core::contract::job_store::{
    EnqueueRequest, FailureClass, JobId, JobKind, JobPayload, JobStore, RetryPolicy,
};
use cairn_core::contract::metrics::{MetricsSink, NoopMetricsSink};
use cairn_workflows::SqliteJobStore;
use cairn_workflows::scheduler::{
    Clock, HandlerOutcome, HandlerRegistryBuilder, JobHandler, ReaperConfig, Scheduler,
    SchedulerConfig, SystemClock, WorkerConfig,
};
use clap::ArgMatches;
use rusqlite::Connection;

/// Synthetic handler that always returns a `Transient` retry. Used by
/// `run-failing` to drive a job through `max_attempts` → dead-letter.
struct AlwaysRetryHandler {
    kind: JobKind,
    reason: String,
}

#[async_trait::async_trait]
impl JobHandler for AlwaysRetryHandler {
    fn kind(&self) -> JobKind {
        self.kind.clone()
    }
    async fn handle(&self, _payload: &JobPayload) -> HandlerOutcome {
        HandlerOutcome::Retry {
            reason: self.reason.clone(),
            class: FailureClass::Transient,
        }
    }
}

/// Synthetic handler that always succeeds. Used as a control for the
/// happy-path metrics demonstration if needed.
struct AlwaysDoneHandler {
    kind: JobKind,
}

#[async_trait::async_trait]
impl JobHandler for AlwaysDoneHandler {
    fn kind(&self) -> JobKind {
        self.kind.clone()
    }
    async fn handle(&self, _payload: &JobPayload) -> HandlerOutcome {
        HandlerOutcome::Done
    }
}

/// Build a `SqliteJobStore` against `<vault>/.cairn/cairn.db`. The DB
/// must already exist (any ingest run materializes it); callers should
/// have ensured this via the binding gate upstream.
fn open_job_store(vault_root: &Path) -> Result<Arc<dyn JobStore>, String> {
    let db_path = vault_root.join(".cairn/cairn.db");
    let conn =
        Connection::open(&db_path).map_err(|e| format!("open {}: {e}", db_path.display()))?;
    let store = SqliteJobStore::new(conn).map_err(|e| format!("init job store: {e}"))?;
    Ok(Arc::new(store))
}

/// Build a `JsonlMetricsSink` for the vault. Falls back to a no-op sink
/// on open failure, matching `cairn mcp`'s wiring.
async fn open_metrics(vault_root: &Path) -> Arc<dyn MetricsSink> {
    match crate::metrics::JsonlMetricsSink::open(vault_root).await {
        Ok(sink) => Arc::new(sink) as Arc<dyn MetricsSink>,
        Err(e) => {
            eprintln!("cairn admin workflow: metrics sink open failed: {e} — using no-op");
            Arc::new(NoopMetricsSink) as Arc<dyn MetricsSink>
        }
    }
}

/// `cairn admin workflow run-failing` — enqueue one synthetic
/// always-retrying job and drive a real scheduler until the row
/// dead-letters. Prints the `job_id` so the caller can correlate the
/// resulting `workflow_jobs` row, `.cairn/metrics.jsonl` lines, and
/// `cairn lint` finding.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "hidden diagnostic verb: open store, enqueue, boot scheduler, poll for \
              dead-letter — splitting the linear flow into helpers hides the order in \
              which the e2e-visible side effects land"
)]
pub fn run_failing(sub: &ArgMatches, vault_root: &Path) -> ExitCode {
    let kind_str = sub
        .get_one::<String>("kind")
        .cloned()
        .unwrap_or_else(|| "test.e2e.always_retry".to_owned());
    let max_attempts: u32 = sub
        .get_one::<String>("max-attempts")
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);
    let deadline_secs: u64 = sub
        .get_one::<String>("deadline-secs")
        .and_then(|s| s.parse().ok())
        .unwrap_or(15);

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("cairn admin workflow run-failing: tokio runtime: {e}");
            return ExitCode::from(69);
        }
    };
    rt.block_on(async move {
        let store = match open_job_store(vault_root) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("cairn admin workflow run-failing: {e}");
                return ExitCode::from(69);
            }
        };
        let metrics = open_metrics(vault_root).await;

        // ULID-shaped job id so it survives the lint pretty-print
        // unchanged.
        let job_id_str = ulid::Ulid::new().to_string();
        let kind = JobKind::new(kind_str.clone());

        let handler = AlwaysRetryHandler {
            kind: kind.clone(),
            reason: format!("e2e synthetic transient failure ({kind_str})"),
        };
        let registry = HandlerRegistryBuilder::default()
            .with(Arc::new(handler))
            .build();

        // Aggressive tunables so the loop completes quickly: tight
        // backoff (1 ms × ×1) and a tight idle poll keep the scheduler
        // hot in this short-lived process.
        let policy = RetryPolicy {
            max_attempts,
            base_backoff_ms: 1,
            backoff_multiplier: 1,
            max_backoff_ms: 1,
        };
        let now_ms = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(i64::MAX);
        let req = EnqueueRequest {
            job_id: JobId::new(job_id_str.clone()),
            kind: kind.clone(),
            payload: vec![],
            queue_key: None,
            // Step-level idempotency tag so the lint Finding's
            // operation_id correlates back to the job_id.
            dedupe_key: Some(format!("e2e-{job_id_str}")),
            // Wall-clock so `WorkflowJobStarted.queue_lag_ms` is a real
            // measurement, not `now_ms - 0` ~= 1.7e12 (issue #92).
            not_before_ms: now_ms,
            retry: policy,
        };
        if let Err(e) = store.enqueue(req).await {
            eprintln!("cairn admin workflow run-failing: enqueue: {e}");
            return ExitCode::from(69);
        }

        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let sched_config = SchedulerConfig {
            worker_count: 1,
            worker: WorkerConfig {
                lease_ms: 2_000,
                heartbeat_every_ms: 500,
                idle_poll_ms: 20,
            },
            reaper: ReaperConfig { interval_ms: 1_000 },
            metrics: metrics.clone(),
        };
        let incarnation = format!("e2e-runfail-{}", ulid::Ulid::new());
        let scheduler =
            Scheduler::start(&incarnation, store.clone(), &registry, clock, sched_config).await;

        // Poll workflow_jobs until the row dead-letters or the
        // deadline elapses. We re-open a fresh connection each poll so
        // we don't fight the scheduler's exclusive connection.
        let deadline = std::time::Instant::now() + Duration::from_secs(deadline_secs);
        let db_path = vault_root.join(".cairn/cairn.db");
        let dead_lettered = loop {
            if std::time::Instant::now() >= deadline {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            let Ok(c) = Connection::open(&db_path) else {
                continue;
            };
            let row: rusqlite::Result<(String, Option<i64>, Option<String>)> = c.query_row(
                "SELECT state, dead_letter_at_ms, failure_class \
                 FROM workflow_jobs WHERE job_id = ?1",
                rusqlite::params![job_id_str.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            );
            if let Ok((state, dl_at, class)) = row
                && state == "failed"
                && dl_at.is_some()
            {
                println!(
                    "dead-lettered: job_id={job_id_str} state={state} \
                     failure_class={} dead_letter_at_ms={}",
                    class.as_deref().unwrap_or(""),
                    dl_at.unwrap_or(0),
                );
                break true;
            }
        };
        scheduler.shutdown().await;
        if !dead_lettered {
            eprintln!(
                "cairn admin workflow run-failing: timeout — job {job_id_str} did not dead-letter \
                 within {deadline_secs}s"
            );
            return ExitCode::from(69);
        }
        println!("job_id={job_id_str}");
        ExitCode::SUCCESS
    })
}

/// `cairn admin workflow simulate-crash` — enqueue a synthetic job,
/// lease + heartbeat it once, then exit without releasing. The orphan
/// row remains in `state = 'leased'` with `lease_started = 1` and an
/// expired `lease_expires_at`, simulating a worker that crashed
/// mid-execution. The next `cairn admin workflow recover` (or any
/// `cairn mcp`) should pick it up via the startup reap.
#[must_use]
pub fn simulate_crash(sub: &ArgMatches, vault_root: &Path) -> ExitCode {
    let kind_str = sub
        .get_one::<String>("kind")
        .cloned()
        .unwrap_or_else(|| "test.e2e.orphan".to_owned());
    let lease_ms: i64 = sub
        .get_one::<String>("lease-ms")
        .and_then(|s| s.parse().ok())
        .unwrap_or(100);

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("cairn admin workflow simulate-crash: tokio runtime: {e}");
            return ExitCode::from(69);
        }
    };
    rt.block_on(async move {
        let store = match open_job_store(vault_root) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("cairn admin workflow simulate-crash: {e}");
                return ExitCode::from(69);
            }
        };
        let now_ms = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(i64::MAX);

        let job_id_str = ulid::Ulid::new().to_string();
        let req = EnqueueRequest {
            job_id: JobId::new(job_id_str.clone()),
            kind: JobKind::new(kind_str.clone()),
            payload: vec![],
            queue_key: None,
            dedupe_key: Some(format!("e2e-crash-{job_id_str}")),
            // Stamp wall-clock so `WorkflowJobStarted.queue_lag_ms`
            // reports a meaningful number on the demo metrics stream
            // (issue #92).
            not_before_ms: now_ms,
            retry: RetryPolicy::DEFAULT,
        };
        if let Err(e) = store.enqueue(req).await {
            eprintln!("cairn admin workflow simulate-crash: enqueue: {e}");
            return ExitCode::from(69);
        }
        let leased = match store.lease("e2e-crash-worker", now_ms, lease_ms).await {
            Ok(Some(j)) => j,
            Ok(None) => {
                eprintln!("cairn admin workflow simulate-crash: lease returned None");
                return ExitCode::from(69);
            }
            Err(e) => {
                eprintln!("cairn admin workflow simulate-crash: lease: {e}");
                return ExitCode::from(69);
            }
        };
        // Heartbeat so lease_started flips to 1 — this is the "worker
        // started executing then crashed" shape (consumes an attempt
        // on reap).
        if let Err(e) = store
            .heartbeat(&leased.job_id, &leased.lease, now_ms + 1, now_ms + lease_ms)
            .await
        {
            eprintln!("cairn admin workflow simulate-crash: heartbeat: {e}");
            return ExitCode::from(69);
        }
        println!(
            "orphaned: job_id={job_id_str} lease_owner=e2e-crash-worker \
             lease_expires_at={}",
            now_ms + lease_ms,
        );
        // Drop the store handle (and thus the rusqlite Connection)
        // without calling fail() or complete(). The row stays leased.
        drop(leased);
        drop(store);
        ExitCode::SUCCESS
    })
}

/// `cairn admin workflow recover` — boots a `Scheduler` with
/// `worker_count = 0` so the startup `reap_expired` runs once, then
/// shuts down. Any orphan row whose `lease_expires_at <= now` is
/// returned to the queue (or dead-lettered if `attempts == max_attempts`).
/// Reports the count of reclaimed rows.
#[must_use]
pub fn recover(_sub: &ArgMatches, vault_root: &Path) -> ExitCode {
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("cairn admin workflow recover: tokio runtime: {e}");
            return ExitCode::from(69);
        }
    };
    rt.block_on(async move {
        // Snapshot pre-recovery state for the report.
        let db_path = vault_root.join(".cairn/cairn.db");
        let leased_before = count_leased(&db_path).unwrap_or(0);

        let store = match open_job_store(vault_root) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("cairn admin workflow recover: {e}");
                return ExitCode::from(69);
            }
        };
        let metrics = open_metrics(vault_root).await;
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let config = SchedulerConfig {
            worker_count: 0,
            worker: WorkerConfig::default(),
            reaper: ReaperConfig {
                interval_ms: 60_000,
            },
            metrics,
        };
        let registry = cairn_workflows::scheduler::HandlerRegistry::default();
        let scheduler = Scheduler::start("e2e-recover", store, &registry, clock, config).await;
        // Give the reap a tiny window to land its writes.
        tokio::time::sleep(Duration::from_millis(50)).await;
        scheduler.shutdown().await;

        let leased_after = count_leased(&db_path).unwrap_or(0);
        let reclaimed = leased_before.saturating_sub(leased_after);
        println!(
            "reclaimed: leased_before={leased_before} leased_after={leased_after} \
             reclaimed_orphans={reclaimed}"
        );
        ExitCode::SUCCESS
    })
}

/// Count rows currently in `state = 'leased'`. Read-only — used for
/// the recovery report.
fn count_leased(db_path: &Path) -> Option<i64> {
    let c = Connection::open(db_path).ok()?;
    c.query_row(
        "SELECT count(*) FROM workflow_jobs WHERE state = 'leased'",
        [],
        |r| r.get::<_, i64>(0),
    )
    .ok()
}

/// `cairn admin workflow run-succeeding` — enqueue one synthetic
/// always-succeeding job and drive a real scheduler until the row
/// completes. Prints the resulting `job_id`. Used to verify the
/// `workflow_job_completed` metric line is emitted on the happy path.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "e2e demo: enqueue + scheduler boot + polling drain + shutdown sequence \
              is intentionally linear so the happy-path lifecycle is visible in one block"
)]
pub fn run_succeeding(sub: &ArgMatches, vault_root: &Path) -> ExitCode {
    let kind_str = sub
        .get_one::<String>("kind")
        .cloned()
        .unwrap_or_else(|| "test.e2e.always_done".to_owned());
    let deadline_secs: u64 = sub
        .get_one::<String>("deadline-secs")
        .and_then(|s| s.parse().ok())
        .unwrap_or(15);

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("cairn admin workflow run-succeeding: tokio runtime: {e}");
            return ExitCode::from(69);
        }
    };
    rt.block_on(async move {
        let store = match open_job_store(vault_root) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("cairn admin workflow run-succeeding: {e}");
                return ExitCode::from(69);
            }
        };
        let metrics = open_metrics(vault_root).await;

        let job_id_str = ulid::Ulid::new().to_string();
        let kind = JobKind::new(kind_str.clone());

        let registry = HandlerRegistryBuilder::default()
            .with(Arc::new(AlwaysDoneHandler { kind: kind.clone() }))
            .build();

        let now_ms = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(i64::MAX);
        let req = EnqueueRequest {
            job_id: JobId::new(job_id_str.clone()),
            kind: kind.clone(),
            payload: vec![],
            queue_key: None,
            dedupe_key: Some(format!("e2e-done-{job_id_str}")),
            // Wall-clock so `WorkflowJobStarted.queue_lag_ms` is a real
            // measurement, not `now_ms - 0` ~= 1.7e12 (issue #92).
            not_before_ms: now_ms,
            retry: RetryPolicy::DEFAULT,
        };
        if let Err(e) = store.enqueue(req).await {
            eprintln!("cairn admin workflow run-succeeding: enqueue: {e}");
            return ExitCode::from(69);
        }

        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let sched_config = SchedulerConfig {
            worker_count: 1,
            worker: WorkerConfig {
                lease_ms: 2_000,
                heartbeat_every_ms: 500,
                idle_poll_ms: 20,
            },
            reaper: ReaperConfig { interval_ms: 1_000 },
            metrics: metrics.clone(),
        };
        let incarnation = format!("e2e-runok-{}", ulid::Ulid::new());
        let scheduler =
            Scheduler::start(&incarnation, store.clone(), &registry, clock, sched_config).await;

        let deadline = std::time::Instant::now() + Duration::from_secs(deadline_secs);
        let db_path = vault_root.join(".cairn/cairn.db");
        let done = loop {
            if std::time::Instant::now() >= deadline {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            let Ok(c) = Connection::open(&db_path) else {
                continue;
            };
            let row: rusqlite::Result<(String, Option<i64>)> = c.query_row(
                "SELECT state, completed_at_ms FROM workflow_jobs WHERE job_id = ?1",
                rusqlite::params![job_id_str.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            );
            if let Ok((state, completed_at)) = row
                && state == "done"
                && completed_at.is_some()
            {
                println!(
                    "completed: job_id={job_id_str} state={state} completed_at_ms={}",
                    completed_at.unwrap_or(0),
                );
                break true;
            }
        };
        scheduler.shutdown().await;
        if !done {
            eprintln!(
                "cairn admin workflow run-succeeding: timeout — job {job_id_str} did not \
                 complete within {deadline_secs}s"
            );
            return ExitCode::from(69);
        }
        println!("job_id={job_id_str}");
        ExitCode::SUCCESS
    })
}
