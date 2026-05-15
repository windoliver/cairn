//! Tokio scheduler loop over [`cairn_core::contract::JobStore`].

pub mod clock;
pub mod handler;
pub mod reaper;
pub mod worker;

pub use clock::{Clock, MockClock, SystemClock};
pub use handler::{
    HandlerDispatchError, HandlerOutcome, HandlerRegistry, HandlerRegistryBuilder, JobHandler,
};
pub use reaper::{ReaperConfig, run_reaper};
pub use worker::{WorkerConfig, run_worker};

use cairn_core::contract::job_store::JobStore;
use cairn_core::contract::metrics::{MetricsSink, NoopMetricsSink};
use cairn_core::domain::metrics::MetricEvent;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// Bundle of all scheduler tunables.
#[derive(Clone)]
pub struct SchedulerConfig {
    /// Worker tunables (shared across workers).
    pub worker: WorkerConfig,
    /// Reaper tunables.
    pub reaper: ReaperConfig,
    /// How many concurrent workers to spawn.
    pub worker_count: u32,
    /// Metrics sink for `WorkflowJob{Started,Completed,Failed}`
    /// emissions (issue #92, spec §4.6). Defaults to
    /// [`NoopMetricsSink`] — callers wire a real sink (e.g.
    /// `JsonlMetricsSink`) when persistence is desired.
    pub metrics: Arc<dyn MetricsSink>,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            worker: WorkerConfig::default(),
            reaper: ReaperConfig::default(),
            worker_count: 1,
            metrics: Arc::new(NoopMetricsSink),
        }
    }
}

impl SchedulerConfig {
    /// P0 default — 2 workers, 30s leases, 5s reap interval, no-op metrics.
    #[must_use]
    pub fn p0() -> Self {
        Self {
            worker: WorkerConfig {
                lease_ms: 30_000,
                heartbeat_every_ms: 10_000,
                idle_poll_ms: 200,
            },
            reaper: ReaperConfig { interval_ms: 5_000 },
            worker_count: 2,
            metrics: Arc::new(NoopMetricsSink),
        }
    }
}

/// Running scheduler handle. Drop or call [`Self::shutdown`] to stop.
pub struct Scheduler {
    cancel: CancellationToken,
    tracker: TaskTracker,
}

impl Scheduler {
    /// Spawn N workers + 1 reaper and return a handle.
    ///
    /// Runs one best-effort `reap_expired(now)` before spawning workers
    /// so a crashed predecessor's expired leases are reclaimed without
    /// waiting for the periodic reaper tick (spec §4.7). The startup
    /// reap is best-effort: a backend failure logs at `warn` and the
    /// scheduler still spawns — the periodic reaper will catch up at
    /// the next tick.
    ///
    /// `worker_count = 0` is honored verbatim — no worker tasks are
    /// spawned. This is useful for tests that want to exercise the
    /// startup reap in isolation from the worker loop.
    #[must_use = "Scheduler must be retained (and `shutdown()` called) or its workers leak"]
    pub async fn start(
        incarnation_id: &str,
        store: Arc<dyn JobStore>,
        registry: &HandlerRegistry,
        clock: Arc<dyn Clock>,
        config: SchedulerConfig,
    ) -> Self {
        let now = clock.now_ms();
        match store.reap_expired(now).await {
            Ok(rows) if !rows.is_empty() => {
                tracing::info!(reclaimed = rows.len(), "startup reap reclaimed orphans");
                // Emit a WorkflowJobFailed event per reclaimed row so
                // `.cairn/metrics.jsonl` records the lease_lost
                // transition regardless of whether the orphan was
                // caught by the startup reap or a later periodic tick.
                // Mirrors the emission in `run_reaper` (spec §4.6,
                // §4.13). Best-effort: a sink error is ignored, same
                // as the periodic reaper.
                for r in rows {
                    let _ = config
                        .metrics
                        .emit(MetricEvent::WorkflowJobFailed {
                            ts_ms: now,
                            job_id: r.job_id.to_string(),
                            kind: r.kind.to_string(),
                            attempts: r.attempts,
                            disposition: "retry".into(),
                            failure_class: "lease_lost".into(),
                            last_error: "startup reap reclaimed expired lease".into(),
                            will_retry_at_ms: Some(now),
                        })
                        .await;
                }
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "startup reap failed; periodic reaper will recover"
                );
            }
        }
        let cancel = CancellationToken::new();
        let tracker = TaskTracker::new();
        for i in 0..config.worker_count {
            let owner = format!("{incarnation_id}:w{i}");
            tracker.spawn(worker::run_worker(
                owner,
                store.clone(),
                registry.clone(),
                clock.clone(),
                cancel.clone(),
                config.worker,
                config.metrics.clone(),
            ));
        }
        tracker.spawn(reaper::run_reaper(
            store,
            clock,
            cancel.clone(),
            config.reaper,
            config.metrics.clone(),
        ));
        tracker.close();
        Self { cancel, tracker }
    }

    /// Cancel all tasks and await them.
    pub async fn shutdown(self) {
        self.cancel.cancel();
        self.tracker.wait().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SqliteJobStore;
    use crate::sqlite_store::install_for_tests;
    use rusqlite::Connection;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_and_shutdown_idempotent() {
        let conn = Connection::open_in_memory().expect("conn");
        install_for_tests(&conn);
        let store: Arc<dyn JobStore> = Arc::new(SqliteJobStore::new(conn).expect("store"));
        let registry = HandlerRegistry::default();
        let clock: Arc<dyn Clock> = Arc::new(MockClock::at(1_000));
        let s = Scheduler::start("inc-1", store, &registry, clock, SchedulerConfig::p0()).await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        s.shutdown().await;
    }
}
