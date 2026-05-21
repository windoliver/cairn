//! Issue #92 — the worker emits `WorkflowJobStarted` then
//! `WorkflowJobCompleted` for a `HandlerOutcome::Done`, and
//! `WorkflowJobStarted` then `WorkflowJobFailed` (with `failure_class
//! = "transient"`) for a `HandlerOutcome::Retry { class: Transient }`.
//!
//! Spec §4.6 wire shape, §4.13 emit-after-store-mutation ordering.

use std::sync::Arc;
use std::time::{Duration, Instant};

use cairn_core::contract::job_store::{
    EnqueueRequest, FailureClass, JobId, JobKind, JobPayload, JobStore, RetryPolicy,
};
use cairn_core::contract::metrics::{CapturingMetricsSink, MetricsSink};
use cairn_core::domain::metrics::MetricEvent;
use cairn_workflows::SqliteJobStore;
use cairn_workflows::scheduler::{
    Clock, HandlerOutcome, HandlerRegistryBuilder, JobHandler, MockClock, ReaperConfig, Scheduler,
    SchedulerConfig, WorkerConfig,
};
use rusqlite::Connection;

const HAPPY_KIND: &str = "test.metrics.happy";
const RETRY_KIND: &str = "test.metrics.retry";

struct DoneHandler;
#[async_trait::async_trait]
impl JobHandler for DoneHandler {
    fn kind(&self) -> JobKind {
        JobKind::new(HAPPY_KIND)
    }
    async fn handle(&self, _: &JobPayload) -> HandlerOutcome {
        HandlerOutcome::Done
    }
}

struct RetryHandler;
#[async_trait::async_trait]
impl JobHandler for RetryHandler {
    fn kind(&self) -> JobKind {
        JobKind::new(RETRY_KIND)
    }
    async fn handle(&self, _: &JobPayload) -> HandlerOutcome {
        HandlerOutcome::Retry {
            reason: "synthetic transient failure".into(),
            class: FailureClass::Transient,
        }
    }
}

fn store() -> Arc<dyn JobStore> {
    let conn = Connection::open_in_memory().expect("open in-memory");
    cairn_workflows::sqlite_store::install_for_tests(&conn);
    Arc::new(SqliteJobStore::new(conn).expect("init store"))
}

/// Poll the sink until `predicate` is satisfied or the deadline expires.
/// Returns the final snapshot regardless — callers assert on it.
async fn wait_for(
    sink: &CapturingMetricsSink,
    predicate: impl Fn(&[MetricEvent]) -> bool,
    deadline: Duration,
) -> Vec<MetricEvent> {
    let start = Instant::now();
    loop {
        let snapshot = sink.snapshot().await;
        if predicate(&snapshot) || start.elapsed() >= deadline {
            return snapshot;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn happy_path_emits_started_then_completed() {
    let store = store();
    let sink = Arc::new(CapturingMetricsSink::new());
    let registry = HandlerRegistryBuilder::default()
        .with(Arc::new(DoneHandler))
        .build();
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(1_000));
    let metrics: Arc<dyn MetricsSink> = sink.clone();
    let config = SchedulerConfig {
        worker_count: 1,
        worker: WorkerConfig {
            lease_ms: 5_000,
            heartbeat_every_ms: 1_000,
            idle_poll_ms: 20,
        },
        reaper: ReaperConfig {
            interval_ms: 60_000,
        },
        metrics,
    };

    store
        .enqueue(EnqueueRequest {
            job_id: JobId::new("j-happy"),
            kind: JobKind::new(HAPPY_KIND),
            payload: vec![],
            queue_key: None,
            dedupe_key: Some("op-happy".into()),
            not_before_ms: 0,
            retry: RetryPolicy::DEFAULT,
        })
        .await
        .expect("enqueue");

    let scheduler = Scheduler::start("inc-happy", store, &registry, clock, config).await;
    let events = wait_for(
        sink.as_ref(),
        |evs| {
            evs.iter()
                .any(|e| matches!(e, MetricEvent::WorkflowJobCompleted { .. }))
        },
        Duration::from_secs(5),
    )
    .await;
    scheduler.shutdown().await;

    let started_idx = events
        .iter()
        .position(|e| matches!(e, MetricEvent::WorkflowJobStarted { .. }))
        .expect("Started must be emitted before Completed");
    let completed_idx = events
        .iter()
        .position(|e| matches!(e, MetricEvent::WorkflowJobCompleted { .. }))
        .expect("Completed must be emitted after Done");
    assert!(
        started_idx < completed_idx,
        "Started must precede Completed in emission order: {events:#?}"
    );

    match &events[started_idx] {
        MetricEvent::WorkflowJobStarted {
            job_id,
            kind,
            attempts,
            dedupe_key,
            ..
        } => {
            assert_eq!(job_id, "j-happy");
            assert_eq!(kind, HAPPY_KIND);
            assert_eq!(*attempts, 1);
            assert_eq!(dedupe_key.as_deref(), Some("op-happy"));
        }
        other => panic!("expected Started, got {other:?}"),
    }

    match &events[completed_idx] {
        MetricEvent::WorkflowJobCompleted {
            job_id,
            kind,
            attempts,
            ..
        } => {
            assert_eq!(job_id, "j-happy");
            assert_eq!(kind, HAPPY_KIND);
            assert_eq!(*attempts, 1);
        }
        other => panic!("expected Completed, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retry_path_emits_started_then_failed_with_transient_class() {
    let store = store();
    let sink = Arc::new(CapturingMetricsSink::new());
    let registry = HandlerRegistryBuilder::default()
        .with(Arc::new(RetryHandler))
        .build();
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(1_000));
    let metrics: Arc<dyn MetricsSink> = sink.clone();
    let config = SchedulerConfig {
        worker_count: 1,
        worker: WorkerConfig {
            lease_ms: 5_000,
            heartbeat_every_ms: 1_000,
            idle_poll_ms: 20,
        },
        reaper: ReaperConfig {
            interval_ms: 60_000,
        },
        metrics,
    };

    store
        .enqueue(EnqueueRequest {
            job_id: JobId::new("j-retry"),
            kind: JobKind::new(RETRY_KIND),
            payload: vec![],
            queue_key: None,
            dedupe_key: None,
            not_before_ms: 0,
            // Long backoff so a second delivery does not race the test.
            retry: RetryPolicy {
                max_attempts: 5,
                base_backoff_ms: 60_000,
                backoff_multiplier: 2,
                max_backoff_ms: 60_000,
            },
        })
        .await
        .expect("enqueue");

    let scheduler = Scheduler::start("inc-retry", store, &registry, clock, config).await;
    let events = wait_for(
        sink.as_ref(),
        |evs| {
            evs.iter()
                .any(|e| matches!(e, MetricEvent::WorkflowJobFailed { .. }))
        },
        Duration::from_secs(5),
    )
    .await;
    scheduler.shutdown().await;

    let started_idx = events
        .iter()
        .position(|e| matches!(e, MetricEvent::WorkflowJobStarted { .. }))
        .expect("Started must be emitted before Failed");
    let failed_idx = events
        .iter()
        .position(|e| matches!(e, MetricEvent::WorkflowJobFailed { .. }))
        .expect("Failed must be emitted after retry-class outcome");
    assert!(
        started_idx < failed_idx,
        "Started must precede Failed in emission order: {events:#?}"
    );

    match &events[failed_idx] {
        MetricEvent::WorkflowJobFailed {
            job_id,
            kind,
            disposition,
            failure_class,
            will_retry_at_ms,
            ..
        } => {
            assert_eq!(job_id, "j-retry");
            assert_eq!(kind, RETRY_KIND);
            assert_eq!(disposition, "retry");
            assert_eq!(failure_class, "transient");
            assert!(
                will_retry_at_ms.is_some(),
                "transient retry must surface a will_retry_at_ms"
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}
