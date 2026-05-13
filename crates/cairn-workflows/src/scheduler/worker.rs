//! One worker task. The Scheduler spawns N of these. Each loops:
//!   1. Try to lease a job.
//!   2. If leased, fork a heartbeat task and run the handler.
//!   3. On handler return, complete/fail; cancel heartbeat.
//!   4. If no job leased, sleep `poll_interval` (or exit on cancel).

use std::sync::Arc;
use std::time::Duration;

use cairn_core::contract::job_store::{FailDisposition, JobStore, LeasedJob};
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
#[instrument(skip(store, registry, clock, cancel), fields(owner = %owner))]
pub async fn run_worker(
    owner: String,
    store: Arc<dyn JobStore>,
    registry: HandlerRegistry,
    clock: Arc<dyn Clock>,
    cancel: CancellationToken,
    config: WorkerConfig,
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
        execute_one(&store, &registry, &clock, &cancel, &leased, &config).await;
    }
}

async fn execute_one(
    store: &Arc<dyn JobStore>,
    registry: &HandlerRegistry,
    clock: &Arc<dyn Clock>,
    cancel: &CancellationToken,
    leased: &LeasedJob,
    config: &WorkerConfig,
) {
    let handler = match registry.lookup(&leased.kind) {
        Ok(h) => h,
        Err(e) => {
            warn!(error = %e, job = %leased.job_id, "no handler; permanent fail");
            let now = clock.now_ms();
            let _ = store
                .fail(
                    &leased.job_id,
                    &leased.lease,
                    FailDisposition::Permanent,
                    &e.to_string(),
                    now,
                )
                .await;
            return;
        }
    };

    // Lease-loss propagation (round-1 adversarial review #4):
    // The heartbeat task notifies the main path via `lease_lost` whenever
    // `store.heartbeat` returns an error. The main path races the handler
    // future against `lease_lost.cancelled()` so a lease that expires
    // mid-handler causes us to abandon the in-flight execution rather
    // than letting it commit side effects whose lease no longer exists.
    // The handler future is dropped at its next .await point — workflow
    // authors that must guard side effects against replay still rely on
    // step-level idempotency, but at least we stop *adding* effects once
    // we know the lease is gone.
    let hb_token = CancellationToken::new();
    let lease_lost = CancellationToken::new();
    let hb_handle = {
        let store = store.clone();
        let clock = clock.clone();
        let lease = leased.lease.clone();
        let job_id = leased.job_id.clone();
        let token = hb_token.clone();
        let lost = lease_lost.clone();
        let interval_ms = u64::try_from(config.heartbeat_every_ms).unwrap_or(10_000);
        let lease_ms = config.lease_ms;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = token.cancelled() => return,
                    () = sleep(Duration::from_millis(interval_ms)) => {
                        let now = clock.now_ms();
                        let new_expires = now.saturating_add(lease_ms);
                        if let Err(e) = store.heartbeat(&job_id, &lease, now, new_expires).await {
                            warn!(error = %e, job = %job_id, "heartbeat lost");
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
        () = cancel.cancelled() => HandlerOutcome::Retry { reason: "scheduler shutdown".into() },
        () = lease_lost.cancelled() => HandlerOutcome::Retry {
            reason: "heartbeat lost — lease expired or stolen".into(),
        },
    };
    hb_token.cancel();
    let _ = timeout(Duration::from_secs(1), hb_handle).await;

    // If the lease is already known lost, skip the complete/fail call —
    // it would fail with LeaseLost anyway and add log noise. The reaper
    // or another worker will reclaim the row.
    if lease_lost.is_cancelled() {
        warn!(job = %leased.job_id, "abandoning execution after lease loss");
        return;
    }

    let now = clock.now_ms();
    let result = match outcome {
        HandlerOutcome::Done => store.complete(&leased.job_id, &leased.lease, now).await,
        HandlerOutcome::Retry { reason } => {
            store
                .fail(
                    &leased.job_id,
                    &leased.lease,
                    FailDisposition::Retry,
                    &reason,
                    now,
                )
                .await
        }
        HandlerOutcome::Permanent { reason } => {
            store
                .fail(
                    &leased.job_id,
                    &leased.lease,
                    FailDisposition::Permanent,
                    &reason,
                    now,
                )
                .await
        }
    };
    if let Err(e) = result {
        error!(error = %e, job = %leased.job_id, "worker finalize failed");
    }
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
        let handle = tokio::spawn(run_worker(
            "w-1".into(),
            store.clone(),
            registry,
            clock.clone(),
            token,
            config,
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
}
