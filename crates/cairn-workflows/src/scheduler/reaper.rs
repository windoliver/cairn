//! Periodic reaper. Calls `JobStore::reap_expired` so leases whose
//! workers crashed are reclaimed.

use std::sync::Arc;
use std::time::Duration;

use cairn_core::contract::job_store::JobStore;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{debug, instrument, warn};

use super::clock::Clock;

/// Reaper tunables.
#[derive(Debug, Clone, Copy)]
pub struct ReaperConfig {
    /// Wall time between scans, milliseconds.
    pub interval_ms: u64,
}

impl Default for ReaperConfig {
    fn default() -> Self {
        Self { interval_ms: 5_000 }
    }
}

/// Run the reaper forever (until `cancel` fires).
#[instrument(skip(store, clock, cancel))]
pub async fn run_reaper(
    store: Arc<dyn JobStore>,
    clock: Arc<dyn Clock>,
    cancel: CancellationToken,
    config: ReaperConfig,
) {
    loop {
        tokio::select! {
            () = cancel.cancelled() => { debug!("reaper cancelled"); return; }
            () = sleep(Duration::from_millis(config.interval_ms)) => {
                let now = clock.now_ms();
                match store.reap_expired(now).await {
                    Ok(n) if n > 0 => debug!(reclaimed = n, "reaper reclaimed orphan leases"),
                    Ok(_) => {}
                    Err(e) => warn!(error = %e, "reap failed"),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::MockClock;
    use crate::SqliteJobStore;
    use rusqlite::Connection;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reaper_ticks() {
        let conn = Connection::open_in_memory().expect("conn");
        crate::sqlite_store::install_for_tests(&conn);
        let store: Arc<dyn JobStore> = Arc::new(SqliteJobStore::new(conn).expect("store"));
        let clock: Arc<dyn Clock> = Arc::new(MockClock::at(1_000));
        let cancel = CancellationToken::new();
        let handle = tokio::spawn(run_reaper(
            store,
            clock,
            cancel.clone(),
            ReaperConfig { interval_ms: 10 },
        ));
        // Let at least one tick fire.
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(1), handle).await;
    }
}
