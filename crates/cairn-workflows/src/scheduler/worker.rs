//! One worker task. The Scheduler spawns N of these. Each loops:
//!   1. Try to lease a job.
//!   2. If leased, fork a heartbeat task and run the handler.
//!   3. On handler return, complete/fail; cancel heartbeat.
//!   4. If no job leased, sleep `poll_interval` (or exit on cancel).

use std::sync::Arc;
use std::time::Duration;

use cairn_core::contract::job_store::{FailDisposition, FailureClass, JobStore, LeasedJob};
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
    let _ = metrics
        .emit(MetricEvent::WorkflowJobStarted {
            ts_ms: started_at_ms,
            job_id: leased.job_id.to_string(),
            kind: leased.kind.to_string(),
            attempts: leased.attempts,
            queue_lag_ms: started_at_ms.saturating_sub(leased.not_before_ms),
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

    let outcome = tokio::select! {
        o = handler.handle(&leased.payload) => o,
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
            // Handlers must not return scheduler-only classes; the
            // shutdown arm uses Transient and the lease-loss arm uses
            // Timeout, which is scheduler-only but stamped here, not
            // by the handler. Anything else flowing through this arm
            // with a scheduler-only class is a bug.
            debug_assert!(
                !class.is_scheduler_only() || class == FailureClass::Timeout,
                "handler returned scheduler-only class {class:?}",
            );
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
            debug_assert!(
                !class.is_scheduler_only(),
                "handler returned scheduler-only class {class:?} on Permanent",
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
        // Next retry uses attempts+1 (the next execution try).
        let next_attempt = leased.attempts.saturating_add(1);
        let delay = i64::from(leased.retry.delay_for_attempt(next_attempt));
        Some(now_ms.saturating_add(delay))
    };
    let disposition_str = match disposition {
        FailDisposition::Retry => "retry",
        FailDisposition::Permanent => "permanent",
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
        async fn lease(&self, _: &str, _: i64, _: i64) -> Result<Option<LeasedJob>, JobStoreError> {
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
}
