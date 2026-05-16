//! One worker task. The Scheduler spawns N of these. Each loops:
//!   1. Try to lease a job.
//!   2. If leased, fork a heartbeat task and run the handler.
//!   3. On handler return, complete/fail; cancel heartbeat.
//!   4. If no job leased, sleep `poll_interval` (or exit on cancel).

use std::sync::Arc;
use std::time::Duration;

use cairn_core::contract::job_store::{FailDisposition, FailureClass, JobId, JobStore, LeasedJob};
use cairn_core::contract::metrics::MetricsSink;
use cairn_core::domain::metrics::MetricEvent;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, instrument, warn};

use super::clock::Clock;
use super::handler::{HandlerOutcome, HandlerRegistry};

/// Tunables for a worker loop.
#[derive(Debug, Clone, Copy)]
pub struct WorkerConfig {
    /// Lease duration handed to `JobStore::lease`.
    pub lease_ms: i64,
    /// Heartbeat extension cadence (`lease_ms / 3` is the rule of thumb).
    pub heartbeat_every_ms: i64,
    /// Sleep duration when no job is available.
    pub idle_poll_ms: u64,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            lease_ms: 30_000,
            heartbeat_every_ms: 10_000,
            idle_poll_ms: 200,
        }
    }
}

/// Run one worker forever (until `cancel` fires).
#[instrument(skip(store, registry, clock, cancel, metrics), fields(owner = %owner))]
pub async fn run_worker(
    owner: String,
    store: Arc<dyn JobStore>,
    registry: HandlerRegistry,
    clock: Arc<dyn Clock>,
    cancel: CancellationToken,
    config: WorkerConfig,
    metrics: Arc<dyn MetricsSink>,
) {
    loop {
        if cancel.is_cancelled() {
            debug!("worker cancelled");
            return;
        }
        let now = clock.now_ms();
        let leased = match store.lease(&owner, now, config.lease_ms).await {
            Ok(Some(job)) => job,
            Ok(None) => {
                tokio::select! {
                    () = sleep(Duration::from_millis(config.idle_poll_ms)) => continue,
                    () = cancel.cancelled() => return,
                }
            }
            Err(e) => {
                warn!(error = %e, "lease failed");
                sleep(Duration::from_millis(config.idle_poll_ms)).await;
                continue;
            }
        };
        execute_one(
            &store, &registry, &clock, &cancel, &leased, &config, &metrics,
        )
        .await;
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "heartbeat + watchdog + handler race is linear; extraction loses context"
)]
async fn execute_one(
    store: &Arc<dyn JobStore>,
    registry: &HandlerRegistry,
    clock: &Arc<dyn Clock>,
    cancel: &CancellationToken,
    leased: &LeasedJob,
    config: &WorkerConfig,
    metrics: &Arc<dyn MetricsSink>,
) {
    // Started emission — spec §4.6 / §4.13: the lease commit has
    // already landed (caller observed `Ok(Some(_))`), so emit before
    // any further store mutation. Sink errors are intentionally
    // swallowed; a missing metric must never abort a real job.
    let started_at_ms = clock.now_ms();
    // `not_before_ms == 0` is the "no scheduling constraint, lag is
    // unknown" sentinel — `now_ms - 0` is the full Unix epoch and a
    // garbage number on the wire. Clamp to 0 so dashboards never see
    // a 1.7e12 spike when an enqueue site forgot to stamp the field
    // (issue #92, spec §4.6).
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

    let handler = match registry.lookup(&leased.kind) {
        Ok(h) => h,
        Err(e) => {
            warn!(error = %e, job = %leased.job_id, "no handler; permanent fail");
            let now = clock.now_ms();
            let reason = e.to_string();
            let fail_res = store
                .fail(
                    &leased.job_id,
                    &leased.lease,
                    FailDisposition::Permanent,
                    FailureClass::Validation,
                    &reason,
                    now,
                )
                .await;
            if fail_res.is_ok() {
                // Emit AFTER the fail commit lands (spec §4.13). Validation
                // is non-retryable so `will_retry_at_ms` is None.
                let _ = metrics
                    .emit(MetricEvent::WorkflowJobFailed {
                        ts_ms: now,
                        job_id: leased.job_id.to_string(),
                        kind: leased.kind.to_string(),
                        attempts: leased.attempts,
                        disposition: "permanent".into(),
                        failure_class: FailureClass::Validation.as_str().to_owned(),
                        last_error: reason,
                        will_retry_at_ms: None,
                    })
                    .await;
            }
            return;
        }
    };

    // Lease-loss propagation:
    // Two independent triggers cancel `lease_lost` so the main task can
    // abandon execution before committing side effects whose lease no
    // longer exists:
    //
    // 1. (round-1) Heartbeat task observes `store.heartbeat` failure
    //    (LeaseLost or backend error) and fires `lost.cancel()`.
    // 2. (round-2 adversarial review #5) A deadline watchdog races the
    //    actual `expires_at_ms` of the current lease. The shared
    //    `deadline_ms` is bumped by the heartbeat task on every
    //    successful extension; if the heartbeat is stuck (long DB lock,
    //    runtime starvation) the watchdog still fires when the persisted
    //    lease expires. Without this the handler could complete side
    //    effects between expiry and the next heartbeat tick, then have
    //    `complete()` fail with LeaseLost and let a reaped duplicate
    //    re-run.
    let hb_token = CancellationToken::new();
    let lease_lost = CancellationToken::new();
    let deadline_ms = Arc::new(std::sync::atomic::AtomicI64::new(
        leased.lease.expires_at_ms,
    ));
    let hb_handle = {
        let store = store.clone();
        let clock = clock.clone();
        let lease = leased.lease.clone();
        let job_id = leased.job_id.clone();
        let token = hb_token.clone();
        let lost = lease_lost.clone();
        let deadline = deadline_ms.clone();
        let interval_ms = u64::try_from(config.heartbeat_every_ms).unwrap_or(10_000);
        let lease_ms = config.lease_ms;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = token.cancelled() => return,
                    () = sleep(Duration::from_millis(interval_ms)) => {
                        let now = clock.now_ms();
                        let new_expires = now.saturating_add(lease_ms);
                        match store.heartbeat(&job_id, &lease, now, new_expires).await {
                            Ok(()) => deadline.store(new_expires, std::sync::atomic::Ordering::SeqCst),
                            Err(e) => {
                                warn!(error = %e, job = %job_id, "heartbeat lost");
                                lost.cancel();
                                return;
                            }
                        }
                    }
                }
            }
        })
    };

    // Watchdog: poll the shared deadline at a cadence well under the
    // heartbeat interval so we catch a stuck heartbeat within ~one tick.
    let watchdog_handle = {
        let clock = clock.clone();
        let lost = lease_lost.clone();
        let token = hb_token.clone();
        let deadline = deadline_ms.clone();
        let poll_ms = u64::try_from(config.heartbeat_every_ms).unwrap_or(10_000) / 4 + 50;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = token.cancelled() => return,
                    () = lost.cancelled() => return,
                    () = sleep(Duration::from_millis(poll_ms)) => {
                        let now = clock.now_ms();
                        let d = deadline.load(std::sync::atomic::Ordering::SeqCst);
                        if now >= d {
                            warn!(now, deadline = d, "lease deadline exceeded — fencing handler");
                            lost.cancel();
                            return;
                        }
                    }
                }
            }
        })
    };

    // Round-3 finding 3.3: convert scheduler-only classes returned by
    // a buggy handler (`Timeout`, `LeaseLost`) into a terminal
    // `Validation` permanent failure at runtime. The previous version
    // relied on `debug_assert!` downstream of this `select!`, which
    // release builds skip — a release-mode handler that returns one
    // of those classes would persist it through `store.fail`,
    // breaking the spec §4.2 provenance invariant ("`Timeout` /
    // `LeaseLost` only come from the scheduler").
    //
    // The `cancel.cancelled()` and `lease_lost.cancelled()` arms
    // legitimately produce scheduler-only classes (`Transient` for
    // shutdown, `Timeout` for lease loss) and MUST NOT go through the
    // sanitizer — those *are* scheduler-internal.
    let outcome = tokio::select! {
        o = handler.handle(&leased.payload) => sanitize_handler_outcome(o, &leased.job_id),
        () = cancel.cancelled() => HandlerOutcome::Retry {
            reason: "scheduler shutdown".into(),
            class: FailureClass::Transient,
        },
        () = lease_lost.cancelled() => HandlerOutcome::Retry {
            reason: "heartbeat lost or lease deadline exceeded".into(),
            class: FailureClass::Timeout,
        },
    };
    hb_token.cancel();
    let _ = timeout(Duration::from_secs(1), hb_handle).await;
    let _ = timeout(Duration::from_secs(1), watchdog_handle).await;

    // If the lease is already known lost, skip the complete/fail call —
    // it would fail with LeaseLost anyway and add log noise. The reaper
    // or another worker will reclaim the row.
    if lease_lost.is_cancelled() {
        warn!(job = %leased.job_id, "abandoning execution after lease loss");
        return;
    }

    let now = clock.now_ms();
    match outcome {
        HandlerOutcome::Done => {
            match store.complete(&leased.job_id, &leased.lease, now).await {
                Ok(()) => {
                    // Emit AFTER the complete commit lands (spec §4.13).
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
                }
                Err(e) => {
                    error!(error = %e, job = %leased.job_id, "worker finalize failed");
                }
            }
        }
        HandlerOutcome::Retry { reason, class } => {
            // Handler-returned scheduler-only classes have already been
            // converted to `Permanent { class: Validation, ... }` by
            // `sanitize_handler_outcome` upstream. The two legitimate
            // scheduler-stamped arrivals here are:
            //   * `Transient` from the `cancel.cancelled()` arm
            //     (shutdown).
            //   * `Timeout` from the `lease_lost.cancelled()` arm
            //     (deadline/heartbeat-loss).
            // Both are correct — the sanitizer never sees those paths.
            //
            // Invariant: Validation/Poison force Permanent regardless of
            // handler-supplied disposition (spec §4.2).
            let disposition = if class.forces_permanent() {
                FailDisposition::Permanent
            } else {
                FailDisposition::Retry
            };
            let fail_res = store
                .fail(
                    &leased.job_id,
                    &leased.lease,
                    disposition,
                    class,
                    &reason,
                    now,
                )
                .await;
            match fail_res {
                Ok(()) => {
                    emit_failed(metrics, leased, disposition, class, &reason, now).await;
                }
                Err(e) => {
                    error!(error = %e, job = %leased.job_id, "worker finalize failed");
                }
            }
        }
        HandlerOutcome::Permanent { reason, class } => {
            // Sanitizer upstream already converted any handler-returned
            // scheduler-only class into a `Permanent { Validation, ... }`,
            // so reaching this arm with `class.is_scheduler_only()` true
            // would be a bug *in the sanitizer*. Belt-and-suspenders.
            debug_assert!(
                !class.is_scheduler_only(),
                "sanitizer bug: scheduler-only class {class:?} reached Permanent arm",
            );
            let fail_res = store
                .fail(
                    &leased.job_id,
                    &leased.lease,
                    FailDisposition::Permanent,
                    class,
                    &reason,
                    now,
                )
                .await;
            match fail_res {
                Ok(()) => {
                    emit_failed(
                        metrics,
                        leased,
                        FailDisposition::Permanent,
                        class,
                        &reason,
                        now,
                    )
                    .await;
                }
                Err(e) => {
                    error!(error = %e, job = %leased.job_id, "worker finalize failed");
                }
            }
        }
    }
}

/// Runtime guard on the spec §4.2 provenance invariant: handlers must
/// never return `FailureClass::Timeout` or `FailureClass::LeaseLost`
/// — those classes mean "the scheduler observed the lease expired"
/// and are stamped by the scheduler itself on the
/// `lease_lost.cancelled()` arm of `execute_one`. A handler that
/// returns one of them anyway is buggy: persisting that class through
/// `store.fail` would lie about provenance to dashboards, alerts, and
/// the `workflow_health` lint.
///
/// This sits between `handler.handle()` and the result handling in
/// `execute_one`. Release builds can't rely on a `debug_assert!` to
/// catch this — the assertion is compiled out. So instead we convert
/// the outcome to `Permanent { class: Validation, reason: "..." }`
/// and `warn!` so the operator can locate the buggy handler.
///
/// **Not** applied to scheduler-internal arms — `cancel.cancelled()`
/// and `lease_lost.cancelled()` legitimately produce scheduler-only
/// classes and call this function NOT from the handler future.
#[must_use]
fn sanitize_handler_outcome(outcome: HandlerOutcome, job_id: &JobId) -> HandlerOutcome {
    let class = match &outcome {
        HandlerOutcome::Done => return outcome,
        HandlerOutcome::Retry { class, .. } | HandlerOutcome::Permanent { class, .. } => *class,
    };
    if !class.is_scheduler_only() {
        return outcome;
    }
    let orig_reason = match &outcome {
        HandlerOutcome::Retry { reason, .. } | HandlerOutcome::Permanent { reason, .. } => {
            reason.clone()
        }
        HandlerOutcome::Done => String::new(),
    };
    warn!(
        job = %job_id,
        returned_class = ?class,
        reason = %orig_reason,
        "handler returned scheduler-only failure class; coercing to Permanent/Validation"
    );
    HandlerOutcome::Permanent {
        reason: format!("handler returned scheduler-only class {class:?}: {orig_reason}"),
        class: FailureClass::Validation,
    }
}

/// Emit a `WorkflowJobFailed` after a successful `store.fail` call.
/// Computes `will_retry_at_ms` from the policy when the store kept
/// the row in `Queued`; `None` when the row terminated.
async fn emit_failed(
    metrics: &Arc<dyn MetricsSink>,
    leased: &LeasedJob,
    disposition: FailDisposition,
    class: FailureClass,
    reason: &str,
    now_ms: i64,
) {
    let will_terminate = matches!(disposition, FailDisposition::Permanent)
        || class.forces_permanent()
        || leased.attempts >= leased.retry.max_attempts;
    let will_retry_at_ms = if will_terminate {
        None
    } else {
        // Match `SqliteJobStore::cas_fail`'s effective-attempt calc:
        // after a successful first heartbeat (the only path where we
        // get here, since pre-heartbeat crashes go through the reaper),
        // the store uses `delay_for_attempt(attempts)` — i.e. the
        // attempt that just failed, NOT attempts+1. Using +1 here would
        // overshoot the persisted `next_run_at` by one backoff step.
        let delay = i64::from(leased.retry.delay_for_attempt(leased.attempts));
        Some(now_ms.saturating_add(delay))
    };
    // Wire disposition reflects the row's on-disk state after the
    // store.fail commit landed: a row that just terminated (because
    // attempts hit the cap, the disposition was Permanent, or the
    // class forces permanent) must report `"permanent"` even when
    // the worker asked for `Retry` — otherwise the metric claims a
    // retry will occur for a row that's now `state = 'failed'`
    // (issue #92, e2e gap).
    let disposition_str = if will_terminate {
        "permanent"
    } else {
        match disposition {
            FailDisposition::Retry => "retry",
            FailDisposition::Permanent => "permanent",
        }
    };
    let _ = metrics
        .emit(MetricEvent::WorkflowJobFailed {
            ts_ms: now_ms,
            job_id: leased.job_id.to_string(),
            kind: leased.kind.to_string(),
            attempts: leased.attempts,
            disposition: disposition_str.to_owned(),
            failure_class: class.as_str().to_owned(),
            last_error: reason.to_owned(),
            will_retry_at_ms,
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SqliteJobStore;
    use crate::scheduler::{HandlerRegistryBuilder, JobHandler, MockClock};
    use cairn_core::contract::job_store::{
        EnqueueRequest, JobId, JobKind, JobPayload, RetryPolicy,
    };
    use rusqlite::Connection;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counter(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl JobHandler for Counter {
        fn kind(&self) -> JobKind {
            JobKind::new("counter")
        }
        async fn handle(&self, _: &JobPayload) -> HandlerOutcome {
            self.0.fetch_add(1, Ordering::SeqCst);
            HandlerOutcome::Done
        }
    }

    fn mem_store() -> Arc<SqliteJobStore> {
        let conn = Connection::open_in_memory().expect("conn");
        crate::sqlite_store::install_for_tests(&conn);
        Arc::new(SqliteJobStore::new(conn).expect("store"))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn worker_runs_handler_once_and_completes() {
        let counter = Arc::new(AtomicUsize::new(0));
        let registry = HandlerRegistryBuilder::default()
            .with(Arc::new(Counter(counter.clone())))
            .build();
        let store = mem_store() as Arc<dyn JobStore>;
        let clock = Arc::new(MockClock::at(1_000)) as Arc<dyn Clock>;
        let cancel = CancellationToken::new();

        store
            .enqueue(EnqueueRequest {
                job_id: JobId::new("j-1"),
                kind: JobKind::new("counter"),
                payload: vec![],
                queue_key: None,
                dedupe_key: None,
                not_before_ms: 0,
                retry: RetryPolicy::DEFAULT,
            })
            .await
            .unwrap();

        let token = cancel.clone();
        let config = WorkerConfig {
            lease_ms: 30_000,
            heartbeat_every_ms: 10_000,
            idle_poll_ms: 50,
        };
        let metrics: Arc<dyn cairn_core::contract::metrics::MetricsSink> =
            Arc::new(cairn_core::contract::metrics::NoopMetricsSink);
        let handle = tokio::spawn(run_worker(
            "w-1".into(),
            store.clone(),
            registry,
            clock.clone(),
            token,
            config,
            metrics,
        ));
        // Poll until the handler has been called, with a generous timeout.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while counter.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        cancel.cancel();
        let _ = timeout(Duration::from_secs(2), handle).await;
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    // ---- class-override invariant suite (spec §4.2) -----------------
    //
    // `(disposition × class)` matrix: when a handler returns
    // `HandlerOutcome::Retry { class }`, the scheduler must convert
    // disposition to `Permanent` iff `class.forces_permanent()` is true,
    // otherwise keep disposition `Retry`. The class itself is always
    // forwarded to the store unchanged.
    use cairn_core::contract::job_store::{
        EnqueueRequest as ER, FailureClass, JobStoreError, LeaseToken, ReclaimedRow,
    };
    use rstest::rstest;
    use std::sync::Mutex;

    /// In-memory capturing `JobStore` that records every `fail` call's
    /// `(disposition, class)` pair so the test can assert the invariant
    /// converted disposition appropriately.
    struct CapturingStore {
        fails: Mutex<Vec<(FailDisposition, FailureClass)>>,
        completes: Mutex<usize>,
    }

    impl CapturingStore {
        fn new() -> Self {
            Self {
                fails: Mutex::new(vec![]),
                completes: Mutex::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl JobStore for CapturingStore {
        async fn enqueue(&self, _: ER) -> Result<(), JobStoreError> {
            Ok(())
        }
        async fn enqueue_leased(
            &self,
            _: ER,
            _: &str,
            _: i64,
            _: i64,
        ) -> Result<LeasedJob, JobStoreError> {
            // Not exercised by the worker unit tests — the worker
            // never calls enqueue_leased, only the synthetic-diagnostic
            // path does. Surface a Backend error so an accidental call
            // is easy to spot, matching the "stub method" pattern other
            // trait methods in this test fixture follow.
            Err(JobStoreError::Backend(
                "CapturingStore::enqueue_leased not implemented".into(),
            ))
        }
        async fn lease(&self, _: &str, _: i64, _: i64) -> Result<Option<LeasedJob>, JobStoreError> {
            Ok(None)
        }
        async fn lease_specific(
            &self,
            _: &JobId,
            _: &JobKind,
            _: &str,
            _: i64,
            _: i64,
        ) -> Result<Option<LeasedJob>, JobStoreError> {
            Ok(None)
        }
        async fn heartbeat(
            &self,
            _: &JobId,
            _: &LeaseToken,
            _: i64,
            _: i64,
        ) -> Result<(), JobStoreError> {
            Ok(())
        }
        async fn complete(&self, _: &JobId, _: &LeaseToken, _: i64) -> Result<(), JobStoreError> {
            // Preserve poisoned-data semantics (matches the `fails`
            // arm below and the production paths in `sqlite_store.rs`):
            // the previous version reset the counter to 0 on poison,
            // which masked test bugs by erasing the running count.
            let mut g = match self.completes.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            *g = g.saturating_add(1);
            Ok(())
        }
        async fn fail(
            &self,
            _: &JobId,
            _: &LeaseToken,
            disposition: FailDisposition,
            class: FailureClass,
            _: &str,
            _: i64,
        ) -> Result<(), JobStoreError> {
            match self.fails.lock() {
                Ok(mut g) => g.push((disposition, class)),
                Err(p) => p.into_inner().push((disposition, class)),
            }
            Ok(())
        }
        async fn reap_expired(&self, _: i64) -> Result<Vec<ReclaimedRow>, JobStoreError> {
            Ok(Vec::new())
        }
    }

    /// Handler that always returns `Retry` with the configured class —
    /// used to drive `execute_one` through every `(class)` branch.
    struct ClassRetryHandler(FailureClass);
    #[async_trait::async_trait]
    impl JobHandler for ClassRetryHandler {
        fn kind(&self) -> JobKind {
            JobKind::new("test.class")
        }
        async fn handle(&self, _: &JobPayload) -> HandlerOutcome {
            HandlerOutcome::Retry {
                reason: "x".into(),
                class: self.0,
            }
        }
    }

    #[rstest]
    #[case(FailureClass::Transient, FailDisposition::Retry)]
    #[case(FailureClass::Validation, FailDisposition::Permanent)]
    #[case(FailureClass::Poison, FailDisposition::Permanent)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handler_retry_with_class_forces_permanent_when_required(
        #[case] class: FailureClass,
        #[case] expected: FailDisposition,
    ) {
        let store_cap = Arc::new(CapturingStore::new());
        let store_dyn: Arc<dyn JobStore> = store_cap.clone();
        let registry = HandlerRegistryBuilder::default()
            .with(Arc::new(ClassRetryHandler(class)))
            .build();
        let clock = Arc::new(MockClock::at(1_000)) as Arc<dyn Clock>;
        let cancel = CancellationToken::new();
        let leased = LeasedJob {
            job_id: JobId::new("j-1"),
            kind: JobKind::new("test.class"),
            payload: vec![],
            attempts: 1,
            retry: RetryPolicy::DEFAULT,
            lease: LeaseToken {
                owner: "w-0".into(),
                nonce: "n-0".into(),
                expires_at_ms: 30_000,
            },
            failure_class: None,
            not_before_ms: 0,
            dedupe_key: None,
        };
        let config = WorkerConfig {
            lease_ms: 30_000,
            heartbeat_every_ms: 10_000,
            idle_poll_ms: 50,
        };
        let metrics: Arc<dyn cairn_core::contract::metrics::MetricsSink> =
            Arc::new(cairn_core::contract::metrics::NoopMetricsSink);
        execute_one(
            &store_dyn, &registry, &clock, &cancel, &leased, &config, &metrics,
        )
        .await;
        let fails = match store_cap.fails.lock() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        };
        assert_eq!(fails.len(), 1, "exactly one fail call expected");
        let (got_disposition, got_class) = fails[0];
        assert_eq!(got_class, class, "class is forwarded unchanged");
        assert_eq!(
            got_disposition, expected,
            "class {class:?} must produce disposition {expected:?}",
        );
    }

    // ---- round-3 finding 3.3: sanitizer suite -----------------------
    //
    // Handlers must NEVER return `FailureClass::Timeout` or
    // `FailureClass::LeaseLost`; both are scheduler-only (spec §4.2).
    // Round 2 enforced this with a `debug_assert!`, which release
    // builds skip. The round-3 sanitizer converts these to
    // `Permanent { class: Validation, ... }` at runtime so a buggy
    // handler can't break the on-disk provenance invariant.

    /// Handler that returns `HandlerOutcome::Retry { class }` —
    /// reused below for both the round-2 `Retry` cases (above) and the
    /// round-3 sanitizer cases (below).
    #[rstest]
    #[case(FailureClass::Timeout)]
    #[case(FailureClass::LeaseLost)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sanitizer_converts_handler_scheduler_only_class_on_retry(
        #[case] bogus_class: FailureClass,
    ) {
        // Pre-condition: this test exercises the sanitizer; the input
        // class MUST be scheduler-only or the test would be vacuous.
        assert!(
            bogus_class.is_scheduler_only(),
            "test setup error: {bogus_class:?} must be scheduler-only",
        );
        let store_cap = Arc::new(CapturingStore::new());
        let store_dyn: Arc<dyn JobStore> = store_cap.clone();
        let registry = HandlerRegistryBuilder::default()
            .with(Arc::new(ClassRetryHandler(bogus_class)))
            .build();
        let clock = Arc::new(MockClock::at(1_000)) as Arc<dyn Clock>;
        let cancel = CancellationToken::new();
        let leased = LeasedJob {
            job_id: JobId::new("j-1"),
            kind: JobKind::new("test.class"),
            payload: vec![],
            attempts: 1,
            retry: RetryPolicy::DEFAULT,
            lease: LeaseToken {
                owner: "w-0".into(),
                nonce: "n-0".into(),
                expires_at_ms: 30_000,
            },
            failure_class: None,
            not_before_ms: 0,
            dedupe_key: None,
        };
        let config = WorkerConfig {
            lease_ms: 30_000,
            heartbeat_every_ms: 10_000,
            idle_poll_ms: 50,
        };
        let metrics: Arc<dyn cairn_core::contract::metrics::MetricsSink> =
            Arc::new(cairn_core::contract::metrics::NoopMetricsSink);
        execute_one(
            &store_dyn, &registry, &clock, &cancel, &leased, &config, &metrics,
        )
        .await;
        let fails = match store_cap.fails.lock() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        };
        assert_eq!(fails.len(), 1, "exactly one fail call expected");
        let (got_disposition, got_class) = fails[0];
        assert_eq!(
            got_disposition,
            FailDisposition::Permanent,
            "scheduler-only class from handler must terminate the row",
        );
        assert_eq!(
            got_class,
            FailureClass::Validation,
            "sanitizer must coerce scheduler-only class to Validation",
        );
    }

    /// Handler that always returns `Permanent` with the configured
    /// class — for sanitizer coverage of the `Permanent` outcome arm.
    struct ClassPermanentHandler(FailureClass);
    #[async_trait::async_trait]
    impl JobHandler for ClassPermanentHandler {
        fn kind(&self) -> JobKind {
            JobKind::new("test.class")
        }
        async fn handle(&self, _: &JobPayload) -> HandlerOutcome {
            HandlerOutcome::Permanent {
                reason: "x".into(),
                class: self.0,
            }
        }
    }

    #[rstest]
    #[case(FailureClass::Timeout)]
    #[case(FailureClass::LeaseLost)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sanitizer_converts_handler_scheduler_only_class_on_permanent(
        #[case] bogus_class: FailureClass,
    ) {
        // Same as above but the handler returns `Permanent`; sanitizer
        // must catch both `Retry` and `Permanent` variants.
        let store_cap = Arc::new(CapturingStore::new());
        let store_dyn: Arc<dyn JobStore> = store_cap.clone();
        let registry = HandlerRegistryBuilder::default()
            .with(Arc::new(ClassPermanentHandler(bogus_class)))
            .build();
        let clock = Arc::new(MockClock::at(1_000)) as Arc<dyn Clock>;
        let cancel = CancellationToken::new();
        let leased = LeasedJob {
            job_id: JobId::new("j-1"),
            kind: JobKind::new("test.class"),
            payload: vec![],
            attempts: 1,
            retry: RetryPolicy::DEFAULT,
            lease: LeaseToken {
                owner: "w-0".into(),
                nonce: "n-0".into(),
                expires_at_ms: 30_000,
            },
            failure_class: None,
            not_before_ms: 0,
            dedupe_key: None,
        };
        let config = WorkerConfig {
            lease_ms: 30_000,
            heartbeat_every_ms: 10_000,
            idle_poll_ms: 50,
        };
        let metrics: Arc<dyn cairn_core::contract::metrics::MetricsSink> =
            Arc::new(cairn_core::contract::metrics::NoopMetricsSink);
        execute_one(
            &store_dyn, &registry, &clock, &cancel, &leased, &config, &metrics,
        )
        .await;
        let fails = match store_cap.fails.lock() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        };
        assert_eq!(fails.len(), 1, "exactly one fail call expected");
        let (got_disposition, got_class) = fails[0];
        assert_eq!(got_disposition, FailDisposition::Permanent);
        assert_eq!(
            got_class,
            FailureClass::Validation,
            "sanitizer must coerce scheduler-only class to Validation",
        );
    }

    /// Pure-function check that the sanitizer:
    /// 1. Passes through legal handler outcomes unchanged.
    /// 2. Embeds the original reason in the rewritten error message
    ///    so the operator can locate the buggy handler from the
    ///    on-disk `last_error`.
    #[test]
    fn sanitize_handler_outcome_pure_behaviour() {
        // Legal Done — pass-through.
        let job = JobId::new("j-1");
        let out = sanitize_handler_outcome(HandlerOutcome::Done, &job);
        assert!(matches!(out, HandlerOutcome::Done));

        // Legal handler class (Transient) — pass-through.
        let out = sanitize_handler_outcome(
            HandlerOutcome::Retry {
                reason: "blip".into(),
                class: FailureClass::Transient,
            },
            &job,
        );
        match out {
            HandlerOutcome::Retry { class, reason } => {
                assert_eq!(class, FailureClass::Transient);
                assert_eq!(reason, "blip");
            }
            other => panic!("expected Retry pass-through, got {other:?}"),
        }

        // Legal Validation Permanent — pass-through.
        let out = sanitize_handler_outcome(
            HandlerOutcome::Permanent {
                reason: "bad schema".into(),
                class: FailureClass::Validation,
            },
            &job,
        );
        assert!(matches!(
            out,
            HandlerOutcome::Permanent {
                class: FailureClass::Validation,
                ..
            }
        ));

        // Bug: handler returned Timeout on Retry → Permanent/Validation.
        let out = sanitize_handler_outcome(
            HandlerOutcome::Retry {
                reason: "i.am.buggy".into(),
                class: FailureClass::Timeout,
            },
            &job,
        );
        match out {
            HandlerOutcome::Permanent { reason, class } => {
                assert_eq!(class, FailureClass::Validation);
                assert!(
                    reason.contains("scheduler-only"),
                    "rewritten reason must mention scheduler-only: {reason}"
                );
                assert!(
                    reason.contains("Timeout"),
                    "rewritten reason must mention the original class: {reason}"
                );
                assert!(
                    reason.contains("i.am.buggy"),
                    "rewritten reason must embed the original handler reason: {reason}"
                );
            }
            other => panic!("expected Permanent/Validation, got {other:?}"),
        }

        // Bug: handler returned LeaseLost on Permanent → Permanent/Validation.
        let out = sanitize_handler_outcome(
            HandlerOutcome::Permanent {
                reason: "p.bug".into(),
                class: FailureClass::LeaseLost,
            },
            &job,
        );
        match out {
            HandlerOutcome::Permanent { reason, class } => {
                assert_eq!(class, FailureClass::Validation);
                assert!(reason.contains("LeaseLost"), "got: {reason}");
                assert!(reason.contains("p.bug"), "got: {reason}");
            }
            other => panic!("expected Permanent/Validation, got {other:?}"),
        }
    }
}
