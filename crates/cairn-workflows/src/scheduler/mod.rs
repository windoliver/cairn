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
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// Bundle of all scheduler tunables.
#[derive(Debug, Clone, Copy, Default)]
pub struct SchedulerConfig {
    /// Worker tunables (shared across workers).
    pub worker: WorkerConfig,
    /// Reaper tunables.
    pub reaper: ReaperConfig,
    /// How many concurrent workers to spawn.
    pub worker_count: u32,
}

impl SchedulerConfig {
    /// P0 default — 2 workers, 30s leases, 5s reap interval.
    #[must_use]
    pub const fn p0() -> Self {
        Self {
            worker: WorkerConfig {
                lease_ms: 30_000,
                heartbeat_every_ms: 10_000,
                idle_poll_ms: 200,
            },
            reaper: ReaperConfig { interval_ms: 5_000 },
            worker_count: 2,
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
    #[must_use]
    pub fn start(
        incarnation_id: &str,
        store: Arc<dyn JobStore>,
        registry: &HandlerRegistry,
        clock: Arc<dyn Clock>,
        config: SchedulerConfig,
    ) -> Self {
        let cancel = CancellationToken::new();
        let tracker = TaskTracker::new();
        for i in 0..config.worker_count.max(1) {
            let owner = format!("{incarnation_id}:w{i}");
            let t = cancel.clone();
            let s = store.clone();
            let r = registry.clone();
            let c = clock.clone();
            tracker.spawn(worker::run_worker(owner, s, r, c, t, config.worker));
        }
        let t = cancel.clone();
        tracker.spawn(reaper::run_reaper(store, clock, t, config.reaper));
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
        let s = Scheduler::start("inc-1", store, &registry, clock, SchedulerConfig::p0());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        s.shutdown().await;
    }
}
