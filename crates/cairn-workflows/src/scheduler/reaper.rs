//! Periodic reaper. Calls `JobStore::reap_expired` so leases whose
//! workers crashed are reclaimed.

use std::sync::Arc;
use std::time::Duration;

use cairn_core::contract::job_store::{FailureClass, JobStore};
use cairn_core::contract::metrics::MetricsSink;
use cairn_core::domain::metrics::MetricEvent;
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
#[instrument(skip(store, clock, cancel, metrics))]
pub async fn run_reaper(
    store: Arc<dyn JobStore>,
    clock: Arc<dyn Clock>,
    cancel: CancellationToken,
    config: ReaperConfig,
    metrics: Arc<dyn MetricsSink>,
) {
    loop {
        tokio::select! {
            () = cancel.cancelled() => { debug!("reaper cancelled"); return; }
            () = sleep(Duration::from_millis(config.interval_ms)) => {
                let now = clock.now_ms();
                match store.reap_expired(now).await {
                    Ok(rows) => {
                        if !rows.is_empty() {
                            debug!(reclaimed = rows.len(), "reaper reclaimed orphan leases");
                        }
                        // Emit AFTER the reap commit lands (spec §4.13).
                        // One Failed metric per reclaimed row — the row
                        // is now queued (or terminal-failed) so the
                        // mutation has already been persisted. The
                        // `terminated` flag distinguishes the two
                        // outcomes so the wire-level disposition
                        // matches the on-disk state (issue #92).
                        for r in rows {
                            let (disposition, will_retry_at_ms) = if r.terminated {
                                ("permanent", None)
                            } else {
                                // Use the row's persisted next_run_at so
                                // the metric agrees with the on-disk
                                // backoff schedule (fall back to `now`
                                // if the adapter didn't surface one,
                                // preserving prior behavior).
                                ("retry", r.next_run_at_ms.or(Some(now)))
                            };
                            let _ = metrics.emit(MetricEvent::WorkflowJobFailed {
                                ts_ms: now,
                                job_id: r.job_id.to_string(),
                                kind: r.kind.to_string(),
                                attempts: r.attempts,
                                disposition: disposition.into(),
                                failure_class: FailureClass::LeaseLost.as_str().into(),
                                last_error: "reaper reclaimed expired lease".into(),
                                will_retry_at_ms,
                            }).await;
                        }
                    }
                    Err(e) => warn!(error = %e, "reap failed"),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SqliteJobStore;
    use crate::scheduler::MockClock;
    use rusqlite::Connection;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reaper_ticks() {
        let conn = Connection::open_in_memory().expect("conn");
        crate::sqlite_store::install_for_tests(&conn);
        let store: Arc<dyn JobStore> = Arc::new(SqliteJobStore::new(conn).expect("store"));
        let clock: Arc<dyn Clock> = Arc::new(MockClock::at(1_000));
        let cancel = CancellationToken::new();
        let metrics: Arc<dyn MetricsSink> =
            Arc::new(cairn_core::contract::metrics::NoopMetricsSink);
        let handle = tokio::spawn(run_reaper(
            store,
            clock,
            cancel.clone(),
            ReaperConfig { interval_ms: 10 },
            metrics,
        ));
        // Let at least one tick fire.
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(1), handle).await;
    }
}
