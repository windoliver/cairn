//! `cairn admin workflow` — hidden developer/E2E diagnostic surface for
//! issue #92 verification. Drives the workflow lifecycle against the
//! live vault DB so an operator can:
//!
//! 1. Force a dead-letter row (`run-failing`) — drives the lifecycle by
//!    hand (lease → assert → heartbeat → fail-with-`Transient`) until
//!    the row hits `max_attempts` and dead-letters.
//! 2. Force a happy-path completion (`run-succeeding`) — same manual
//!    drive but uses `complete()` so the synthetic row reaches
//!    `state = 'done'`.
//! 3. Simulate a mid-flight crash (`simulate-crash`) — enqueues + leases
//!    + heartbeats a job, then exits without releasing the lease so the
//!    row stays in `state = 'leased'` with `lease_started = 1`.
//! 4. Reap an orphan from a previous run (`recover`) — boots a scheduler
//!    with no workers so the startup `reap_expired` pass runs once,
//!    proving crash-safety across restarts.
//!
//! `run-failing` and `run-succeeding` deliberately do NOT boot a
//! production `Scheduler` against the live DB (round-3 finding 3.1):
//! booting one with a synthetic-only handler registry created a
//! race window where a concurrent `cairn mcp` enqueue would be
//! leased by the synthetic worker and permanently failed via the
//! missing-handler arm (`FailureClass::Validation`). The
//! `drive_synthetic_job` helper leases, ASSERTS the row id+kind
//! match what we enqueued, then heartbeats and fails/completes —
//! abandoning any misrouted row without a mutation so the reaper
//! returns it to the rightful worker.
//!
//! `simulate-crash` and `recover` still use the post-lease assertion
//! and `worker_count = 0` respectively, so neither leases a foreign
//! row. The `JsonlMetricsSink` wiring matches `cairn mcp`.
//! Hidden from default `--help`.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use cairn_core::contract::job_store::{
    EnqueueRequest, FailDisposition, FailureClass, JobId, JobKind, JobStore, LeasedJob, RetryPolicy,
};
use cairn_core::contract::metrics::{MetricsSink, NoopMetricsSink};
use cairn_core::domain::metrics::MetricEvent;
use cairn_workflows::SqliteJobStore;
use cairn_workflows::scheduler::{
    Clock, ReaperConfig, Scheduler, SchedulerConfig, SystemClock, WorkerConfig,
};
use clap::ArgMatches;
use rusqlite::Connection;

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

/// Prefix every synthetic job kind these diagnostics enqueue starts
/// with. The isolation precheck refuses to run if any `workflow_jobs`
/// row whose `kind` does NOT start with this prefix is currently in
/// `queued` or `leased` state — running a `Scheduler` whose registry
/// only knows synthetic handlers would otherwise lease that real row,
/// hit the `HandlerDispatchError::Unknown` path, and permanently fail
/// it as `FailureClass::Validation` (worker.rs missing-handler arm).
///
/// Operators who deliberately want to exercise the live queue should
/// shut down `cairn mcp` first; the precheck only blocks unsafe
/// concurrent operation.
const SYNTHETIC_KIND_PREFIX: &str = "test.e2e.";

/// Reject any caller-supplied `--kind` whose value does NOT start with
/// [`SYNTHETIC_KIND_PREFIX`]. The precheck only catches *pre-existing*
/// non-synthetic rows; without this guard a caller could enqueue brand
/// new rows tagged with a production kind (e.g.
/// `--kind dream.light`). Those rows would either be dead-lettered as
/// `Validation` by the synthetic-only handler registry (run-failing)
/// or — worse — completed with `state = 'done'`, satisfying the
/// `workflow_health` lint's `last_success_ms` check and masking real
/// production failures (run-succeeding). Apply this guard FIRST,
/// before opening the store or doing any other work.
fn validate_synthetic_kind(kind: &str) -> Result<(), String> {
    if !kind.starts_with(SYNTHETIC_KIND_PREFIX) {
        return Err(format!(
            "refused — `--kind {kind}` is not a synthetic kind. \
             Diagnostic kinds must start with `{SYNTHETIC_KIND_PREFIX}` \
             so production rows cannot be created or interfered with."
        ));
    }
    Ok(())
}

/// Refuse to run when production workflow rows are in flight, so the
/// synthetic scheduler can never accidentally lease and dead-letter
/// them. Returns `Ok(())` when the queue contains no non-synthetic
/// rows in `queued`/`leased` state; otherwise returns a descriptive
/// error listing the offending kinds and an operator-facing hint.
fn ensure_no_live_production_rows(vault_root: &Path) -> Result<(), String> {
    let db_path = vault_root.join(".cairn/cairn.db");
    let conn =
        Connection::open(&db_path).map_err(|e| format!("open {}: {e}", db_path.display()))?;
    let mut stmt = conn
        .prepare(
            "SELECT kind, COUNT(*) FROM workflow_jobs \
               WHERE state IN ('queued','leased') \
                 AND kind NOT LIKE ?1 \
               GROUP BY kind ORDER BY kind",
        )
        .map_err(|e| format!("prepare precheck: {e}"))?;
    let pattern = format!("{SYNTHETIC_KIND_PREFIX}%");
    let rows: Vec<(String, i64)> = stmt
        .query_map(rusqlite::params![pattern], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| format!("query precheck: {e}"))?
        .collect::<rusqlite::Result<_>>()
        .map_err(|e| format!("collect precheck: {e}"))?;
    if rows.is_empty() {
        return Ok(());
    }
    let summary = rows
        .iter()
        .map(|(k, n)| format!("{k} ({n})"))
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "refused — live production workflow rows present (queued|leased): {summary}. \
         Stop `cairn mcp` and rerun, or wait for the queue to drain. \
         These diagnostics use a synthetic-only handler registry; running while real \
         rows are scheduling would permanently fail them through the missing-handler path."
    ))
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

/// Outcome of a single manual lifecycle step driven by
/// `drive_synthetic_job`. Used by the run-failing / run-succeeding
/// loops to decide whether to keep leasing or stop.
#[derive(Debug, PartialEq, Eq)]
enum DriveOutcome {
    /// The synthetic row reached its terminal state on disk
    /// (`failed` for run-failing, `done` for run-succeeding).
    Terminal,
    /// The synthetic row was failed-with-retry but is not yet
    /// terminal; caller should continue leasing.
    Requeued,
    /// `store.lease()` returned `None` — no eligible row right now.
    /// Caller should sleep a tick and retry until the deadline.
    NoLeaseAvailable,
    /// `store.lease()` returned a row that is NOT the synthetic
    /// row we enqueued (different `job_id` OR non-synthetic
    /// `kind`). The caller must NOT heartbeat/fail/complete this
    /// row — abandon it and let the lease expire so the reaper
    /// reclaims it for the rightful worker. Returned as a hard
    /// error: subcommands exit `EX_UNAVAILABLE` immediately.
    Misrouted {
        /// Job id we got but did not enqueue.
        got_job_id: String,
        /// Kind we got but did not enqueue.
        got_kind: String,
    },
    /// Backend error from the store; subcommands map to
    /// `EX_UNAVAILABLE` and report.
    StoreError(String),
}

/// Manual single-step lifecycle drive. Replaces the production
/// `Scheduler` worker loop for the synthetic e2e diagnostics
/// (round-3 finding 3.1).
///
/// Why: booting a production `Scheduler` against the live DB with a
/// synthetic-only handler registry meant any production row enqueued
/// in the precheck → lease race window would be leased by our worker
/// and permanently failed via the missing-handler path
/// (`FailureClass::Validation`). Driving lease → assert → heartbeat
/// → fail/complete by hand lets us refuse — without mutating —
/// any row that isn't the one we just enqueued.
///
/// Steps (matches `worker.rs::execute_one` for the metric emission
/// shape, but never dispatches to a handler):
///
/// 1. `store.lease_specific(&our_job_id, &our_kind, owner, now_ms, lease_ms)`
///    — atomically claim ONLY the row we enqueued. Round-4 finding:
///    using `lease()` would let a precheck→lease race hand back a
///    production row, and the very act of leasing that row already
///    bumps its `delivery_count` and parks it in `state='leased'`
///    until the lease expires (potentially tripping the poison guard
///    on repeated diagnostic runs). `lease_specific` constrains the
///    UPDATE's WHERE to `(job_id, kind)` so non-matching rows stay
///    untouched on disk.
/// 2. If `lease_specific` still somehow returns a row whose id+kind
///    don't match what we expected: bail with `Misrouted`. **No**
///    heartbeat, **no** fail/complete. (Belt-and-suspenders — the SQL
///    WHERE already proved the match for production stores.)
/// 3. Emit `WorkflowJobStarted` to the metrics sink.
/// 4. Heartbeat once so `lease_started = 1` (mirrors what a real
///    worker does between lease and finalize).
/// 5. Call `store.fail(...)` or `store.complete(...)` per `mode`.
/// 6. Emit `WorkflowJobFailed` / `WorkflowJobCompleted` after the
///    store mutation commits (spec §4.13).
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "single linear lifecycle drive: lease → assert → emit started → \
              heartbeat → fail/complete → emit terminal. Extracting helpers hides \
              the ordering of the e2e-visible side effects."
)]
async fn drive_synthetic_job(
    store: &Arc<dyn JobStore>,
    metrics: &Arc<dyn MetricsSink>,
    expected_job_id: &str,
    expected_kind: &JobKind,
    owner: &str,
    lease_ms: i64,
    mode: DriveMode<'_>,
) -> DriveOutcome {
    let now_ms = wall_ms();
    let our_job_id = JobId::new(expected_job_id.to_owned());
    let leased: LeasedJob = match store
        .lease_specific(&our_job_id, expected_kind, owner, now_ms, lease_ms)
        .await
    {
        Ok(Some(j)) => j,
        Ok(None) => return DriveOutcome::NoLeaseAvailable,
        Err(e) => return DriveOutcome::StoreError(format!("lease_specific: {e}")),
    };

    // Belt-and-suspenders post-lease assertion. With `lease_specific`,
    // a real `JobStore` can ONLY return a row matching the supplied
    // `(job_id, kind)` — the WHERE clause guarantees it. We keep the
    // assertion both for defense-in-depth and so a buggy mock /
    // future store can't silently regress the synthetic diagnostic
    // into mutating a foreign row.
    if leased.job_id.as_str() != expected_job_id
        || leased.kind.as_str() != expected_kind.as_str()
        || !leased.kind.as_str().starts_with(SYNTHETIC_KIND_PREFIX)
    {
        return DriveOutcome::Misrouted {
            got_job_id: leased.job_id.to_string(),
            got_kind: leased.kind.to_string(),
        };
    }

    // Started emission (spec §4.6 / §4.13) — mirror worker.rs's clamp
    // for the `not_before_ms == 0` sentinel.
    let started_at_ms = wall_ms();
    let queue_lag_ms = if leased.not_before_ms == 0 {
        0
    } else {
        started_at_ms.saturating_sub(leased.not_before_ms)
    };
    let _ = metrics
        .emit(MetricEvent::WorkflowJobStarted {
            ts_ms: started_at_ms,
            job_id: leased.job_id.to_string(),
            kind: leased.kind.to_string(),
            attempts: leased.attempts,
            queue_lag_ms,
            dedupe_key: leased.dedupe_key.clone(),
        })
        .await;

    // Heartbeat once: flips `lease_started = 1` and extends the
    // deadline. Without this, a `fail()` on attempt 1 wouldn't count
    // toward `max_attempts` (pre-heartbeat reaps are "free"). The
    // synthetic loop *wants* the attempt to count so it can reach
    // `max_attempts` and dead-letter quickly.
    let hb_now = wall_ms();
    if let Err(e) = store
        .heartbeat(&leased.job_id, &leased.lease, hb_now, hb_now + lease_ms)
        .await
    {
        return DriveOutcome::StoreError(format!("heartbeat: {e}"));
    }

    match mode {
        DriveMode::Fail { reason, class } => {
            let now = wall_ms();
            let disposition = if class.forces_permanent() {
                FailDisposition::Permanent
            } else {
                FailDisposition::Retry
            };
            if let Err(e) = store
                .fail(
                    &leased.job_id,
                    &leased.lease,
                    disposition,
                    class,
                    reason,
                    now,
                )
                .await
            {
                return DriveOutcome::StoreError(format!("fail: {e}"));
            }
            // Mirror worker.rs::emit_failed for terminal/retry
            // bookkeeping so the metric on the wire matches the
            // row's on-disk state.
            let will_terminate = matches!(disposition, FailDisposition::Permanent)
                || class.forces_permanent()
                || leased.attempts >= leased.retry.max_attempts;
            let will_retry_at_ms = if will_terminate {
                None
            } else {
                let delay = i64::from(leased.retry.delay_for_attempt(leased.attempts));
                Some(now.saturating_add(delay))
            };
            let disposition_str = if will_terminate { "permanent" } else { "retry" };
            let _ = metrics
                .emit(MetricEvent::WorkflowJobFailed {
                    ts_ms: now,
                    job_id: leased.job_id.to_string(),
                    kind: leased.kind.to_string(),
                    attempts: leased.attempts,
                    disposition: disposition_str.to_owned(),
                    failure_class: class.as_str().to_owned(),
                    last_error: reason.to_owned(),
                    will_retry_at_ms,
                })
                .await;
            if will_terminate {
                DriveOutcome::Terminal
            } else {
                DriveOutcome::Requeued
            }
        }
        DriveMode::Complete => {
            let now = wall_ms();
            if let Err(e) = store.complete(&leased.job_id, &leased.lease, now).await {
                return DriveOutcome::StoreError(format!("complete: {e}"));
            }
            let duration_ms =
                u64::try_from(now.saturating_sub(started_at_ms).max(0)).unwrap_or(u64::MAX);
            let _ = metrics
                .emit(MetricEvent::WorkflowJobCompleted {
                    ts_ms: now,
                    job_id: leased.job_id.to_string(),
                    kind: leased.kind.to_string(),
                    attempts: leased.attempts,
                    duration_ms,
                })
                .await;
            DriveOutcome::Terminal
        }
    }
}

/// What the manual lifecycle step should do to its leased row.
#[derive(Debug, Clone, Copy)]
enum DriveMode<'a> {
    /// Call `store.fail` with the given `reason` and `class`. The
    /// `(disposition)` is derived by the helper from
    /// `class.forces_permanent()` so the spec §4.2 invariant
    /// holds in this hand-rolled path the same way it does in
    /// `worker.rs::execute_one`.
    Fail {
        reason: &'a str,
        class: FailureClass,
    },
    /// Call `store.complete`.
    Complete,
}

/// Wall-clock millis since the UNIX epoch, saturating on overflow.
fn wall_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX)
}

/// `cairn admin workflow run-failing` — enqueue one synthetic
/// always-retrying job and drive its lifecycle by hand until the row
/// dead-letters. Prints the `job_id` so the caller can correlate the
/// resulting `workflow_jobs` row, `.cairn/metrics.jsonl` lines, and
/// `cairn lint` finding.
///
/// Round-3 finding 3.1: this command no longer boots a production
/// `Scheduler` worker. Booting one against the live DB with a
/// synthetic-only handler registry meant any concurrent `cairn mcp`
/// enqueue could land between our precheck and our `lease()` call,
/// be leased by our worker, hit the missing-handler arm, and be
/// permanently failed as `FailureClass::Validation`. Driving the
/// lifecycle manually (lease → assert id+kind → heartbeat → fail
/// → emit) lets us refuse any row that isn't ours WITHOUT calling
/// `fail`/`complete`/`heartbeat` on it.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "hidden diagnostic verb: open store, enqueue, drive lifecycle manually until \
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
    // Reject non-synthetic kinds BEFORE opening the store, the runtime,
    // or anything else — finding 3.2 (round-3). Otherwise an operator
    // could enqueue a production-kind row that the synthetic registry
    // would dead-letter as `Validation` (run-failing) or, worse,
    // complete as `done` and mask a real failure (run-succeeding).
    if let Err(e) = validate_synthetic_kind(&kind_str) {
        eprintln!("cairn admin workflow run-failing: {e}");
        return ExitCode::from(69);
    }
    rt.block_on(async move {
        if let Err(e) = ensure_no_live_production_rows(vault_root) {
            eprintln!("cairn admin workflow run-failing: {e}");
            return ExitCode::from(69);
        }
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

        // Aggressive tunables so the loop completes quickly: tight
        // backoff (1 ms × ×1) keeps the row eligible immediately
        // after each retry-fail.
        let policy = RetryPolicy {
            max_attempts,
            base_backoff_ms: 1,
            backoff_multiplier: 1,
            max_backoff_ms: 1,
        };
        let now_ms = wall_ms();
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

        let owner = format!("e2e-runfail-{}", ulid::Ulid::new());
        let lease_ms: i64 = 2_000;
        let reason = format!("e2e synthetic transient failure ({kind_str})");
        let mode = DriveMode::Fail {
            reason: reason.as_str(),
            class: FailureClass::Transient,
        };

        // Manual lifecycle loop. Bounded by `deadline_secs` so a
        // race-and-bail outcome doesn't hang the diagnostic.
        let deadline = std::time::Instant::now() + Duration::from_secs(deadline_secs);
        let terminal = loop {
            if std::time::Instant::now() >= deadline {
                break false;
            }
            match drive_synthetic_job(
                &store,
                &metrics,
                &job_id_str,
                &kind,
                &owner,
                lease_ms,
                mode,
            )
            .await
            {
                DriveOutcome::Terminal => break true,
                DriveOutcome::Requeued => {
                    // Backoff is 1ms so the row should be re-eligible
                    // immediately; a tiny sleep avoids a hot CPU spin
                    // while the store commits.
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                DriveOutcome::NoLeaseAvailable => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                DriveOutcome::Misrouted { got_job_id, got_kind } => {
                    eprintln!(
                        "cairn admin workflow run-failing: refused — lease returned non-synthetic \
                         row (job_id={got_job_id} kind={got_kind}); abandoned WITHOUT heartbeat/fail \
                         so the lease will expire and the reaper reclaims it. Expected job_id={job_id_str}."
                    );
                    return ExitCode::from(69);
                }
                DriveOutcome::StoreError(msg) => {
                    eprintln!("cairn admin workflow run-failing: {msg}");
                    return ExitCode::from(69);
                }
            }
        };
        if !terminal {
            eprintln!(
                "cairn admin workflow run-failing: timeout — job {job_id_str} did not dead-letter \
                 within {deadline_secs}s"
            );
            return ExitCode::from(69);
        }

        // Confirm on-disk state for the operator-facing line; this
        // also surfaces the persisted `failure_class` which the e2e
        // lint check depends on.
        let db_path = vault_root.join(".cairn/cairn.db");
        let row: Option<(String, Option<i64>, Option<String>)> = Connection::open(&db_path)
            .ok()
            .and_then(|c| {
                c.query_row(
                    "SELECT state, dead_letter_at_ms, failure_class \
                     FROM workflow_jobs WHERE job_id = ?1",
                    rusqlite::params![job_id_str.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .ok()
            });
        if let Some((state, dl_at, class)) = row {
            println!(
                "dead-lettered: job_id={job_id_str} state={state} \
                 failure_class={} dead_letter_at_ms={}",
                class.as_deref().unwrap_or(""),
                dl_at.unwrap_or(0),
            );
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
    // Reject non-synthetic kinds before any side effects — finding 3.2.
    if let Err(e) = validate_synthetic_kind(&kind_str) {
        eprintln!("cairn admin workflow simulate-crash: {e}");
        return ExitCode::from(69);
    }
    rt.block_on(async move {
        if let Err(e) = ensure_no_live_production_rows(vault_root) {
            eprintln!("cairn admin workflow simulate-crash: {e}");
            return ExitCode::from(69);
        }
        let store = match open_job_store(vault_root) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("cairn admin workflow simulate-crash: {e}");
                return ExitCode::from(69);
            }
        };
        let now_ms = wall_ms();

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
        // Use lease_specific so the UPDATE's WHERE constrains to
        // (job_id, kind). Round 4 finding: plain store.lease() is
        // already a mutation (bumps delivery_count, sets state='leased')
        // BEFORE the post-lease assertion can refuse — so a misrouted
        // production row had its delivery budget consumed. lease_specific
        // atomically refuses (returns Ok(None)) on any non-matching row.
        let synthetic_kind = JobKind::new(kind_str.clone());
        let synthetic_id = JobId::new(job_id_str.clone());
        let leased = match store
            .lease_specific(
                &synthetic_id,
                &synthetic_kind,
                "e2e-crash-worker",
                now_ms,
                lease_ms,
            )
            .await
        {
            Ok(Some(j)) => j,
            Ok(None) => {
                eprintln!(
                    "cairn admin workflow simulate-crash: lease_specific returned None — \
                     synthetic row {job_id_str} was not lease-eligible (state mismatch?)"
                );
                return ExitCode::from(69);
            }
            Err(e) => {
                eprintln!("cairn admin workflow simulate-crash: lease_specific: {e}");
                return ExitCode::from(69);
            }
        };
        // Belt-and-suspenders: lease_specific's WHERE already enforced
        // the match, so this should never fire. Keep it as a tripwire
        // against a future regression that loosens the SQL predicate.
        debug_assert_eq!(leased.job_id.as_str(), job_id_str);
        debug_assert!(leased.kind.as_str().starts_with(SYNTHETIC_KIND_PREFIX));
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
/// always-succeeding job and drive its lifecycle by hand until the
/// row completes. Prints the resulting `job_id`. Used to verify the
/// `workflow_job_completed` metric line is emitted on the happy path.
///
/// Round-3 finding 3.1: see `run_failing` for the rationale — this
/// command no longer boots a production `Scheduler`. The hand-rolled
/// `drive_synthetic_job` step asserts the leased row's `job_id` AND
/// `kind` match our synthetic row before mutating anything. A
/// production row that lands in the precheck → lease race window is
/// abandoned without a heartbeat/complete, so the reaper reclaims it
/// untouched. Critically: a race-completed `done` row tagged with a
/// production `kind` (e.g. `dream.light`) would otherwise update
/// `last_success_ms` on the `workflow_health` lint and mask real
/// failures.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "hidden diagnostic verb: open store, enqueue, drive lifecycle manually \
              until complete — keeping the linear flow inline makes the order of \
              the e2e-visible side effects unambiguous"
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
    // Reject non-synthetic kinds before any side effects — finding 3.2.
    if let Err(e) = validate_synthetic_kind(&kind_str) {
        eprintln!("cairn admin workflow run-succeeding: {e}");
        return ExitCode::from(69);
    }
    rt.block_on(async move {
        if let Err(e) = ensure_no_live_production_rows(vault_root) {
            eprintln!("cairn admin workflow run-succeeding: {e}");
            return ExitCode::from(69);
        }
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

        let now_ms = wall_ms();
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

        let owner = format!("e2e-runok-{}", ulid::Ulid::new());
        let lease_ms: i64 = 2_000;
        let deadline = std::time::Instant::now() + Duration::from_secs(deadline_secs);
        let done = loop {
            if std::time::Instant::now() >= deadline {
                break false;
            }
            match drive_synthetic_job(
                &store,
                &metrics,
                &job_id_str,
                &kind,
                &owner,
                lease_ms,
                DriveMode::Complete,
            )
            .await
            {
                DriveOutcome::Terminal => break true,
                // `Requeued` is unreachable for `DriveMode::Complete`
                // — `complete` is terminal. Fold into NoLease for
                // exhaustiveness.
                DriveOutcome::Requeued | DriveOutcome::NoLeaseAvailable => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                DriveOutcome::Misrouted { got_job_id, got_kind } => {
                    eprintln!(
                        "cairn admin workflow run-succeeding: refused — lease returned non-synthetic \
                         row (job_id={got_job_id} kind={got_kind}); abandoned WITHOUT heartbeat/complete \
                         so the lease will expire and the reaper reclaims it. Expected job_id={job_id_str}."
                    );
                    return ExitCode::from(69);
                }
                DriveOutcome::StoreError(msg) => {
                    eprintln!("cairn admin workflow run-succeeding: {msg}");
                    return ExitCode::from(69);
                }
            }
        };
        if !done {
            eprintln!(
                "cairn admin workflow run-succeeding: timeout — job {job_id_str} did not \
                 complete within {deadline_secs}s"
            );
            return ExitCode::from(69);
        }

        let db_path = vault_root.join(".cairn/cairn.db");
        let row: Option<(String, Option<i64>)> = Connection::open(&db_path).ok().and_then(|c| {
            c.query_row(
                "SELECT state, completed_at_ms FROM workflow_jobs WHERE job_id = ?1",
                rusqlite::params![job_id_str.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok()
        });
        if let Some((state, completed_at)) = row {
            println!(
                "completed: job_id={job_id_str} state={state} completed_at_ms={}",
                completed_at.unwrap_or(0),
            );
        }
        println!("job_id={job_id_str}");
        ExitCode::SUCCESS
    })
}

#[cfg(test)]
mod tests {
    //! Unit tests for the round-3 fixes (issue #92).
    //!
    //! - `validate_synthetic_kind_*` cover finding 3.2.
    //! - `drive_synthetic_job_*` cover finding 3.1 — specifically that a
    //!   foreign row leased between the precheck and lease is abandoned
    //!   WITHOUT a heartbeat / fail / complete call.

    use super::*;
    use cairn_core::contract::job_store::{
        EnqueueRequest, JobId, JobKind, JobStoreError, LeaseToken, ReclaimedRow,
    };
    use std::sync::Mutex;

    // -----------------------------------------------------------------
    // 3.2 — validate_synthetic_kind
    // -----------------------------------------------------------------

    #[test]
    fn validate_synthetic_kind_accepts_test_e2e_prefix() {
        assert!(validate_synthetic_kind("test.e2e.always_retry").is_ok());
        assert!(validate_synthetic_kind("test.e2e.always_done").is_ok());
        assert!(validate_synthetic_kind("test.e2e.").is_ok());
        assert!(validate_synthetic_kind("test.e2e.custom").is_ok());
    }

    #[test]
    fn validate_synthetic_kind_rejects_production_kinds() {
        let e = validate_synthetic_kind("dream.light").expect_err("must reject");
        assert!(e.contains("refused"), "error text: {e}");
        assert!(e.contains("synthetic"), "error text: {e}");
        assert!(e.contains("test.e2e."), "error text: {e}");

        for k in &[
            "",
            "dream",
            "test",
            "test.e2e",
            "evaluation.batch",
            "consolidation",
        ] {
            assert!(
                validate_synthetic_kind(k).is_err(),
                "expected `{k}` to be rejected",
            );
        }
    }

    // -----------------------------------------------------------------
    // 3.1 — drive_synthetic_job misrouted-row handling
    // -----------------------------------------------------------------

    /// Record of every `JobStore` method call. Used by the test to
    /// assert the Misrouted path does NOT call heartbeat / fail /
    /// complete on the foreign row.
    #[derive(Default)]
    struct CallLog {
        lease_calls: usize,
        heartbeats: usize,
        fails: usize,
        completes: usize,
    }

    /// `JobStore` that returns a configurable `LeasedJob` from `lease()`
    /// and records every call to every method.
    struct ScriptedStore {
        log: Mutex<CallLog>,
        next_lease: Mutex<Option<LeasedJob>>,
    }

    impl ScriptedStore {
        fn new(lease: LeasedJob) -> Self {
            Self {
                log: Mutex::new(CallLog::default()),
                next_lease: Mutex::new(Some(lease)),
            }
        }

        fn snapshot(&self) -> CallLog {
            let g = match self.log.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            CallLog {
                lease_calls: g.lease_calls,
                heartbeats: g.heartbeats,
                fails: g.fails,
                completes: g.completes,
            }
        }
    }

    #[async_trait::async_trait]
    impl JobStore for ScriptedStore {
        async fn enqueue(&self, _: EnqueueRequest) -> Result<(), JobStoreError> {
            Ok(())
        }
        async fn lease(&self, _: &str, _: i64, _: i64) -> Result<Option<LeasedJob>, JobStoreError> {
            let mut g = match self.log.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            g.lease_calls += 1;
            drop(g);
            let mut slot = match self.next_lease.lock() {
                Ok(s) => s,
                Err(p) => p.into_inner(),
            };
            Ok(slot.take())
        }
        async fn lease_specific(
            &self,
            _: &JobId,
            _: &JobKind,
            _: &str,
            _: i64,
            _: i64,
        ) -> Result<Option<LeasedJob>, JobStoreError> {
            // Scripted store ignores the (job_id, kind) constraint and
            // hands back whatever `next_lease` holds: the diagnostic's
            // post-lease assertion is what we want to exercise from the
            // CLI verbs' perspective, mirroring the legacy `lease()`
            // hook so existing misrouted-row tests still cover the
            // belt-and-suspenders path.
            let mut g = match self.log.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            g.lease_calls += 1;
            drop(g);
            let mut slot = match self.next_lease.lock() {
                Ok(s) => s,
                Err(p) => p.into_inner(),
            };
            Ok(slot.take())
        }
        async fn heartbeat(
            &self,
            _: &JobId,
            _: &LeaseToken,
            _: i64,
            _: i64,
        ) -> Result<(), JobStoreError> {
            let mut g = match self.log.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            g.heartbeats += 1;
            Ok(())
        }
        async fn complete(&self, _: &JobId, _: &LeaseToken, _: i64) -> Result<(), JobStoreError> {
            let mut g = match self.log.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            g.completes += 1;
            Ok(())
        }
        async fn fail(
            &self,
            _: &JobId,
            _: &LeaseToken,
            _: FailDisposition,
            _: FailureClass,
            _: &str,
            _: i64,
        ) -> Result<(), JobStoreError> {
            let mut g = match self.log.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            g.fails += 1;
            Ok(())
        }
        async fn reap_expired(&self, _: i64) -> Result<Vec<ReclaimedRow>, JobStoreError> {
            Ok(Vec::new())
        }
    }

    fn synthetic_leased(job_id: &str, kind: &str) -> LeasedJob {
        LeasedJob {
            job_id: JobId::new(job_id),
            kind: JobKind::new(kind),
            payload: vec![],
            attempts: 1,
            retry: RetryPolicy::DEFAULT,
            lease: LeaseToken {
                owner: "w-test".into(),
                nonce: "n-test".into(),
                expires_at_ms: 9_999_999_999,
            },
            failure_class: None,
            not_before_ms: 0,
            dedupe_key: None,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drive_synthetic_job_bails_on_foreign_job_id() {
        // Lease returns a row whose job_id does NOT match what we
        // expect. The drive must return Misrouted and NEVER call
        // heartbeat/fail/complete on it.
        let foreign = synthetic_leased(
            "01JZ_FOREIGN_JOB_ULID",
            "test.e2e.always_retry", // kind LOOKS synthetic
        );
        let store_concrete = Arc::new(ScriptedStore::new(foreign));
        let store: Arc<dyn JobStore> = store_concrete.clone();
        let metrics: Arc<dyn MetricsSink> = Arc::new(NoopMetricsSink);

        let outcome = drive_synthetic_job(
            &store,
            &metrics,
            "01JZ_OURS_JOB_ULID", // expected != got
            &JobKind::new("test.e2e.always_retry"),
            "test-owner",
            2_000,
            DriveMode::Fail {
                reason: "synthetic",
                class: FailureClass::Transient,
            },
        )
        .await;

        match outcome {
            DriveOutcome::Misrouted {
                got_job_id,
                got_kind,
            } => {
                assert_eq!(got_job_id, "01JZ_FOREIGN_JOB_ULID");
                assert_eq!(got_kind, "test.e2e.always_retry");
            }
            other => panic!("expected Misrouted, got {other:?}"),
        }

        let log = store_concrete.snapshot();
        assert_eq!(log.lease_calls, 1, "exactly one lease call");
        assert_eq!(log.heartbeats, 0, "no heartbeat on foreign row");
        assert_eq!(log.fails, 0, "no fail on foreign row");
        assert_eq!(log.completes, 0, "no complete on foreign row");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drive_synthetic_job_bails_on_non_synthetic_kind() {
        // Lease returns a row whose job_id MATCHES (improbable but
        // possible in the race) BUT whose kind is a production kind.
        // The kind check must catch this even when the id collides.
        let job_id = "01JZ_OURS_JOB_ULID";
        let foreign = synthetic_leased(job_id, "dream.light");
        let store_concrete = Arc::new(ScriptedStore::new(foreign));
        let store: Arc<dyn JobStore> = store_concrete.clone();
        let metrics: Arc<dyn MetricsSink> = Arc::new(NoopMetricsSink);

        let outcome = drive_synthetic_job(
            &store,
            &metrics,
            job_id,
            // Caller's expected kind is the synthetic one; the mock
            // hands back a production-kind row to exercise the
            // belt-and-suspenders assertion.
            &JobKind::new("test.e2e.always_done"),
            "test-owner",
            2_000,
            DriveMode::Complete,
        )
        .await;

        match outcome {
            DriveOutcome::Misrouted {
                got_job_id,
                got_kind,
            } => {
                assert_eq!(got_job_id, job_id);
                assert_eq!(got_kind, "dream.light");
            }
            other => panic!("expected Misrouted, got {other:?}"),
        }

        let log = store_concrete.snapshot();
        assert_eq!(log.heartbeats, 0, "no heartbeat on production-kind row");
        assert_eq!(log.fails, 0, "no fail on production-kind row");
        assert_eq!(log.completes, 0, "no complete on production-kind row");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drive_synthetic_job_no_lease_available_when_lease_returns_none() {
        // ScriptedStore::next_lease defaults to a row once; after we
        // exhaust it the next lease() returns None.
        let leased = synthetic_leased("01JZ_ID", "test.e2e.always_retry");
        let store_concrete = Arc::new(ScriptedStore::new(leased));
        // Drain the one queued lease so the next call returns None.
        {
            let mut slot = match store_concrete.next_lease.lock() {
                Ok(s) => s,
                Err(p) => p.into_inner(),
            };
            *slot = None;
        }
        let store: Arc<dyn JobStore> = store_concrete.clone();
        let metrics: Arc<dyn MetricsSink> = Arc::new(NoopMetricsSink);

        let outcome = drive_synthetic_job(
            &store,
            &metrics,
            "01JZ_ID",
            &JobKind::new("test.e2e.always_retry"),
            "test-owner",
            2_000,
            DriveMode::Complete,
        )
        .await;

        assert!(
            matches!(outcome, DriveOutcome::NoLeaseAvailable),
            "expected NoLeaseAvailable, got {outcome:?}"
        );

        let log = store_concrete.snapshot();
        assert_eq!(log.heartbeats, 0);
        assert_eq!(log.fails, 0);
        assert_eq!(log.completes, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drive_synthetic_job_complete_path_calls_heartbeat_then_complete() {
        // Sanity check the happy path so the test suite documents the
        // expected mutation order: heartbeat exactly once, then
        // complete exactly once.
        let job_id = "01JZ_ID";
        let leased = synthetic_leased(job_id, "test.e2e.always_done");
        let store_concrete = Arc::new(ScriptedStore::new(leased));
        let store: Arc<dyn JobStore> = store_concrete.clone();
        let metrics: Arc<dyn MetricsSink> = Arc::new(NoopMetricsSink);

        let outcome = drive_synthetic_job(
            &store,
            &metrics,
            job_id,
            &JobKind::new("test.e2e.always_done"),
            "test-owner",
            2_000,
            DriveMode::Complete,
        )
        .await;
        assert_eq!(outcome, DriveOutcome::Terminal);
        let log = store_concrete.snapshot();
        assert_eq!(log.heartbeats, 1, "exactly one heartbeat on happy path");
        assert_eq!(log.completes, 1, "exactly one complete on happy path");
        assert_eq!(log.fails, 0, "no fail on happy path");
    }
}
