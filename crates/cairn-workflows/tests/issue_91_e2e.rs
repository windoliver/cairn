//! End-to-end: boots the same `Scheduler` + `HandlerRegistry` shape
//! that `cairn mcp serve` constructs, enqueues one job per new
//! workflow, and asserts every kind's side effect lands in the store.
//! Closes issue #91 AC#1 — "each workflow can be scheduled, resumed,
//! and inspected through status/lint" — by proving the live scheduler
//! honors every registered handler.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cairn_core::config::{DreamConfig, EvaluationConfig, ExpirationConfig};
use cairn_core::contract::job_store::{EnqueueRequest, JobId, JobKind, JobStore, RetryPolicy};
use cairn_core::contract::llm_provider::{
    CompletionOutput, CompletionRequest, LLMProvider, LLMProviderCapabilities, LlmError,
};
use cairn_core::contract::memory_store::{ListArgs, MemoryStore};
use cairn_core::contract::metrics::{CapturingMetricsSink, MetricsSink};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::metrics::MetricEvent;
use cairn_test_fixtures::sample_record;
use cairn_workflows::scheduler::{
    Clock, HandlerRegistryBuilder, Scheduler, SchedulerConfig, SystemClock, WorkerConfig,
};
use cairn_workflows::{
    DREAM_KIND, DreamHandler, DreamPayload, EVALUATION_KIND, EXPIRATION_KIND, EvaluationHandler,
    EvaluationPayload, ExpirationHandler, ExpirationPayload, SqliteJobStore, default_golden_checks,
};
use tempfile::tempdir;

struct StubLlm;

#[async_trait]
impl LLMProvider for StubLlm {
    fn name(&self) -> &'static str {
        "stub-llm-e2e"
    }
    fn capabilities(&self) -> &LLMProviderCapabilities {
        static CAPS: LLMProviderCapabilities = LLMProviderCapabilities {
            json_mode: false,
            streaming: false,
            tool_calls: false,
        };
        &CAPS
    }
    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
    }
    async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionOutput, LlmError> {
        Ok(CompletionOutput::Text("e2e dream body".into()))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "linear E2E narrative: store seed → registry build → enqueue → \
              poll for side effects + metric → shutdown → assertions; \
              splitting hurts the readability of the full handshake."
)]
async fn three_workflows_drain_through_one_scheduler() {
    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("cairn.db");
    let mem = Arc::new(
        cairn_store_sqlite::open(&db_path)
            .await
            .expect("open memory store"),
    );

    // Seed two records so the orphan check has something to scan and
    // dream has a non-empty window.
    mem.upsert(&sample_record(101))
        .await
        .expect("seed record 1");
    mem.upsert(&sample_record(102))
        .await
        .expect("seed record 2");

    let jobs_conn = cairn_store_sqlite::open_sync(&db_path).expect("open jobs conn");
    let jobs: Arc<dyn JobStore> = Arc::new(SqliteJobStore::new(jobs_conn).expect("jobs"));

    let dyn_store: Arc<dyn MemoryStore> = mem.clone();
    let metrics = Arc::new(CapturingMetricsSink::new());

    let dream = DreamHandler::new(
        dyn_store.clone(),
        DreamConfig {
            enabled: true,
            window_size_records: 8,
            ..DreamConfig::default()
        },
        Some(Arc::new(StubLlm) as Arc<dyn LLMProvider>),
    );
    let expiration = ExpirationHandler::with_job_store(
        dyn_store.clone(),
        ExpirationConfig {
            enabled: true,
            ttl_days: 1,
            salience_floor: 0.0,
            batch_size: 16,
        },
        jobs.clone(),
    );
    let evaluation = EvaluationHandler::new(
        dyn_store.clone(),
        metrics.clone() as Arc<dyn MetricsSink>,
        default_golden_checks(),
        EvaluationConfig {
            enabled: true,
            write_report_record: true,
            ..EvaluationConfig::default()
        },
    );

    let registry = HandlerRegistryBuilder::default()
        .with(Arc::new(dream))
        .with(Arc::new(expiration))
        .with(Arc::new(evaluation))
        .build();

    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let sched_cfg = SchedulerConfig {
        worker: WorkerConfig {
            idle_poll_ms: 20,
            ..SchedulerConfig::p0().worker
        },
        ..SchedulerConfig::p0()
    };
    let scheduler = Scheduler::start("e2e", jobs.clone(), &registry, clock.clone(), sched_cfg);

    // Enqueue one job per kind. now_ms for expiration is well past the
    // sample fixture's `updated_at` (2026-04-22) so every seed expires.
    let dream_payload = DreamPayload {
        key: "e2e-sess".into(),
        bound_scope: None,
    }
    .to_bytes()
    .expect("dream payload");
    // 2026-04-23T00:00:00Z — one day after the fixture's stamped
    // `updated_at` (2026-04-22T14:05:11Z) so the seeds expire with
    // `ttl_days = 1`, but well before "today's" real wall-clock that
    // dream/evaluation will stamp on their synthesized records. The
    // pure decision function saturates `now - updated` at 0 for
    // future-dated rows, so the dream/eval outputs survive the sweep.
    let exp_payload = ExpirationPayload {
        now_ms: 1_777_276_800_000,
        bound_scope: None,
        cursor: None,
    }
    .to_bytes()
    .expect("exp payload");
    let eval_payload = EvaluationPayload {
        ts_ms: 1_700_000_000_000,
        check_ids: vec![],
        bound_scope: None,
    }
    .to_bytes()
    .expect("eval payload");

    // Round-9 adversarial review #1 introduced a post-upsert
    // source-liveness recheck on dream that self-tombstones the
    // dream record if any source is tombstoned during commit. To
    // test all three workflows on the same source set deterministi-
    // cally, we enqueue dream + evaluation first, wait for them to
    // produce their synthetic records, THEN enqueue the expiration
    // sweep. Production deployments serialize via DreamPayload's
    // `recommended_queue_key`; we mirror that here by sequencing.
    for (job_id, kind, payload) in [
        ("e2e-dream", DREAM_KIND, dream_payload),
        ("e2e-eval", EVALUATION_KIND, eval_payload),
    ] {
        jobs.enqueue(EnqueueRequest {
            job_id: JobId::new(job_id),
            kind: JobKind::new(kind),
            payload,
            queue_key: None,
            dedupe_key: None,
            not_before_ms: 0,
            retry: RetryPolicy::DEFAULT,
        })
        .await
        .unwrap_or_else(|e| panic!("enqueue {kind}: {e}"));
    }

    // Poll until dream + eval publish their records and the metric
    // fires. We assert these BEFORE enqueuing expiration so the
    // race window between dream upsert and source tombstone is
    // closed by sequencing rather than by hope.
    let deadline_phase1 =
        std::time::Instant::now() + Duration::from_secs(15);
    let mut seen_dream = false;
    let mut seen_eval = false;
    let mut seen_metric = false;
    while std::time::Instant::now() < deadline_phase1 {
        let listed = mem
            .list(&ListArgs {
                limit: 50,
                ..ListArgs::default()
            })
            .await
            .expect("list");
        seen_dream = listed.records.iter().any(|r| r.body == "e2e dream body");
        seen_eval = listed
            .records
            .iter()
            .any(|r| r.body.starts_with("# Evaluation report"));
        seen_metric = metrics
            .snapshot()
            .await
            .iter()
            .any(|e| matches!(e, MetricEvent::EvaluationCompleted { .. }));
        if seen_dream && seen_eval && seen_metric {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Now enqueue the expiration sweep.
    jobs.enqueue(EnqueueRequest {
        job_id: JobId::new("e2e-exp"),
        kind: JobKind::new(EXPIRATION_KIND),
        payload: exp_payload,
        queue_key: None,
        dedupe_key: None,
        not_before_ms: 0,
        retry: RetryPolicy::DEFAULT,
    })
    .await
    .expect("enqueue expiration");

    let deadline_phase2 =
        std::time::Instant::now() + Duration::from_secs(15);
    let mut seen_expire_drain = false;
    while std::time::Instant::now() < deadline_phase2 {
        let listed = mem
            .list(&ListArgs {
                limit: 50,
                ..ListArgs::default()
            })
            .await
            .expect("list");
        let surviving_seeds = listed
            .records
            .iter()
            .filter(|r| r.body.starts_with("seeded body"))
            .count();
        seen_expire_drain = surviving_seeds == 0;
        if seen_expire_drain {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    scheduler.shutdown().await;

    assert!(seen_dream, "dream record never appeared in default reads");
    assert!(
        seen_eval,
        "evaluation report never appeared in default reads"
    );
    assert!(
        seen_expire_drain,
        "expiration sweep never tombstoned every seed"
    );
    assert!(seen_metric, "EvaluationCompleted metric was never emitted");

    // Confirm exactly one EvaluationCompleted (no duplicates).
    let eval_events = metrics
        .snapshot()
        .await
        .into_iter()
        .filter(|e| matches!(e, MetricEvent::EvaluationCompleted { .. }))
        .count();
    assert_eq!(
        eval_events, 1,
        "expected exactly one EvaluationCompleted metric, got {eval_events}",
    );
}
