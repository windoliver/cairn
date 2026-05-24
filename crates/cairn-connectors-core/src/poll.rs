//! Per-connector polling. `PollScheduler::spawn` registers a callback that
//! the scheduler invokes every `interval` until the shared
//! [`CancellationToken`] fires.
//!
//! Each spawned task maintains its own cursor so that a connector can resume
//! exactly where it left off after a transient error. Errors are logged at
//! `warn` level and the loop continues — the scheduler never panics on behalf
//! of a connector.
//!
//! Issue #130, brief §9.1 source sensors.

use std::future::Future;
use std::time::Duration;

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::error::ConnectorError;

/// Result returned by one invocation of a poll callback.
///
/// The `next_cursor` value is persisted between ticks: the scheduler passes
/// the cursor from the previous successful tick as the `cursor` argument of
/// the next call.
#[derive(Debug)]
pub struct PollTick {
    /// Opaque cursor to resume from on the next tick, or `None` to restart
    /// from the beginning.
    pub next_cursor: Option<String>,
}

impl PollTick {
    /// Construct a [`PollTick`] with the given cursor.
    #[must_use]
    pub fn done(next_cursor: Option<String>) -> Self {
        Self { next_cursor }
    }
}

/// Drives one or more poll loops, each behind its own tokio task.
///
/// Spawn connectors with [`PollScheduler::spawn`]; each task runs until the
/// shared [`CancellationToken`] is cancelled. Call [`PollScheduler::shutdown`]
/// after cancelling to wait for all tasks to finish cleanly.
///
/// ```text
/// ┌─────────────┐  cancel()   ┌────────────────────────────┐
/// │  CancelToken│───────────► │  tokio::select!            │
/// └─────────────┘             │   cancelled() => break     │
///                             │   sleep(interval) => tick  │
///                             └────────────────────────────┘
/// ```
pub struct PollScheduler {
    token: CancellationToken,
    interval: Duration,
    tasks: JoinSet<()>,
}

impl PollScheduler {
    /// Create a new scheduler driven by `token` that fires every `interval`.
    #[must_use]
    pub fn new(token: CancellationToken, interval: Duration) -> Self {
        Self {
            token,
            interval,
            tasks: JoinSet::new(),
        }
    }

    /// Register a named poll callback.
    ///
    /// `tick` is called with the cursor from the previous successful call
    /// (or `None` on the first call). The returned [`PollTick::next_cursor`]
    /// becomes the cursor for the subsequent call.
    ///
    /// Errors from `tick` are logged at `warn` level; the loop continues.
    /// Cancellation is detected between ticks — the current tick is allowed
    /// to complete before the task exits.
    pub fn spawn<F, Fut>(&mut self, name: String, mut tick: F)
    where
        F: FnMut(Option<String>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<PollTick, ConnectorError>> + Send + 'static,
    {
        let token = self.token.clone();
        let interval = self.interval;
        self.tasks.spawn(async move {
            let mut cursor: Option<String> = None;
            loop {
                tokio::select! {
                    // Cancellation arm first so a cancelled token always
                    // wins even if the sleep would also be ready.
                    () = token.cancelled() => break,
                    () = tokio::time::sleep(interval) => {
                        match tick(cursor.clone()).await {
                            Ok(t) => cursor = t.next_cursor,
                            Err(err) => {
                                tracing::warn!(
                                    connector = %name,
                                    ?err,
                                    "poll tick returned an error; retrying on next interval",
                                );
                            }
                        }
                    }
                }
            }
        });
    }

    /// Cancel all outstanding poll tasks and wait for them to finish.
    ///
    /// Consumes `self` so the scheduler cannot be reused after shutdown.
    pub async fn shutdown(mut self) {
        while self.tasks.join_next().await.is_some() {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn poll_loop_invokes_callback_and_stops_on_cancel() {
        let count = Arc::new(AtomicUsize::new(0));
        let token = CancellationToken::new();
        let c = count.clone();
        let mut scheduler = PollScheduler::new(token.clone(), Duration::from_millis(20));
        scheduler.spawn("fixture".into(), move |_cursor| {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Ok::<_, crate::ConnectorError>(PollTick::done(None))
            }
        });
        tokio::time::sleep(Duration::from_millis(80)).await;
        token.cancel();
        scheduler.shutdown().await;
        let observed = count.load(Ordering::SeqCst);
        assert!(observed >= 2, "expected ≥2 ticks, got {observed}");
    }

    #[tokio::test]
    async fn multiple_connectors_can_run_in_one_scheduler() {
        let count_a = Arc::new(AtomicUsize::new(0));
        let count_b = Arc::new(AtomicUsize::new(0));
        let token = CancellationToken::new();

        let mut scheduler = PollScheduler::new(token.clone(), Duration::from_millis(20));

        let ca = count_a.clone();
        scheduler.spawn("connector-a".into(), move |_cursor| {
            let ca = ca.clone();
            async move {
                ca.fetch_add(1, Ordering::SeqCst);
                Ok::<_, crate::ConnectorError>(PollTick::done(None))
            }
        });

        let cb = count_b.clone();
        scheduler.spawn("connector-b".into(), move |_cursor| {
            let cb = cb.clone();
            async move {
                cb.fetch_add(1, Ordering::SeqCst);
                Ok::<_, crate::ConnectorError>(PollTick::done(None))
            }
        });

        tokio::time::sleep(Duration::from_millis(80)).await;
        token.cancel();
        scheduler.shutdown().await;

        let a = count_a.load(Ordering::SeqCst);
        let b = count_b.load(Ordering::SeqCst);
        assert!(a >= 1, "connector-a expected ≥1 tick, got {a}");
        assert!(b >= 1, "connector-b expected ≥1 tick, got {b}");
    }

    #[tokio::test]
    async fn cursor_is_threaded_between_ticks() {
        // Verify that the next_cursor from one tick is passed to the next.
        let cursors = Arc::new(std::sync::Mutex::new(Vec::<Option<String>>::new()));
        let token = CancellationToken::new();

        let mut scheduler = PollScheduler::new(token.clone(), Duration::from_millis(10));

        let cs = cursors.clone();
        let mut call_count = 0usize;
        scheduler.spawn("cursor-test".into(), move |cursor| {
            let cs = cs.clone();
            call_count += 1;
            let next = call_count; // borrow by copy
            async move {
                cs.lock().unwrap().push(cursor);
                Ok::<_, crate::ConnectorError>(PollTick::done(Some(format!("tick-{next}"))))
            }
        });

        // Wait for at least two ticks (10ms interval, wait 50ms).
        tokio::time::sleep(Duration::from_millis(50)).await;
        token.cancel();
        scheduler.shutdown().await;

        let observed = cursors.lock().unwrap();
        // First tick always sees None; second tick sees the cursor from tick 1.
        assert!(
            observed.len() >= 2,
            "expected ≥2 recorded cursors, got {}",
            observed.len()
        );
        assert_eq!(observed[0], None, "first tick must receive None cursor");
        assert_eq!(
            observed[1],
            Some("tick-1".to_string()),
            "second tick must receive cursor from first tick"
        );
    }
}
