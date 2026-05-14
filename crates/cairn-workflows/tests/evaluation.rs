//! Integration: EvaluationHandler runs the starter golden checks,
//! upserts a deterministic report record, and emits a single
//! `MetricEvent::EvaluationCompleted` per sweep. AC#3 of issue #91 —
//! "evaluation outputs are deterministic enough for release gating".

use std::sync::Arc;

use cairn_core::config::EvaluationConfig;
use cairn_core::contract::memory_store::{ListArgs, MemoryStore};
use cairn_core::contract::metrics::{CapturingMetricsSink, MetricsSink};
use cairn_core::domain::metrics::MetricEvent;
use cairn_core::domain::taxonomy::MemoryKind;
use cairn_test_fixtures::{memstore, sample_record};
use cairn_workflows::scheduler::{HandlerOutcome, JobHandler};
use cairn_workflows::{EvaluationHandler, EvaluationPayload, default_golden_checks};

#[tokio::test]
async fn sweep_emits_metric_and_report_record() {
    let store = Arc::new(memstore().await);
    store.upsert(&sample_record(1)).await.expect("seed record");

    let sink = Arc::new(CapturingMetricsSink::new());
    let dyn_store: Arc<dyn MemoryStore> = store.clone();
    let handler = EvaluationHandler::new(
        dyn_store,
        sink.clone() as Arc<dyn MetricsSink>,
        default_golden_checks(),
        EvaluationConfig {
            enabled: true,
            write_report_record: true,
            ..EvaluationConfig::default()
        },
    );
    let payload = EvaluationPayload {
        ts_ms: 1_700_000_000_000,
        check_ids: vec![],
    };
    let bytes = payload.to_bytes().expect("encode");
    let outcome = handler.handle(&bytes).await;
    assert!(matches!(outcome, HandlerOutcome::Done));

    // One metric line emitted.
    let events = sink.snapshot().await;
    assert_eq!(events.len(), 1);
    match &events[0] {
        MetricEvent::EvaluationCompleted {
            checks_run,
            passed,
            failed,
            report_target_id,
            ..
        } => {
            assert!(*checks_run >= 1);
            assert!(*passed >= 1);
            assert_eq!(*failed, 0);
            assert!(!report_target_id.is_empty(), "report target id must be set");
        }
        _ => panic!("expected EvaluationCompleted"),
    }

    // A reasoning report record now exists.
    let listed = store
        .list(&ListArgs {
            limit: 100,
            ..ListArgs::default()
        })
        .await
        .expect("list");
    let report = listed
        .records
        .iter()
        .find(|r| r.kind == MemoryKind::Reasoning && r.body.starts_with("# Evaluation report"));
    assert!(
        report.is_some(),
        "no evaluation report record found in: {:?}",
        listed.records.iter().map(|r| &r.body).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn replays_produce_byte_identical_report() {
    // Brief §15 release gating: same store snapshot + same payload →
    // same `report_target_id`. The store's body-hash dedupe makes the
    // second upsert a no-op, but the `target_id` and the metric must
    // both be stable.
    let store = Arc::new(memstore().await);
    store.upsert(&sample_record(2)).await.expect("seed");

    let sink = Arc::new(CapturingMetricsSink::new());
    let dyn_store: Arc<dyn MemoryStore> = store.clone();
    let handler = EvaluationHandler::new(
        dyn_store,
        sink.clone() as Arc<dyn MetricsSink>,
        default_golden_checks(),
        EvaluationConfig {
            enabled: true,
            write_report_record: true,
            ..EvaluationConfig::default()
        },
    );

    let payload = EvaluationPayload {
        ts_ms: 1_700_000_000_000,
        check_ids: vec![],
    };

    let a = handler.run_once(&payload).await.expect("first sweep");
    let b = handler.run_once(&payload).await.expect("second sweep");
    assert_eq!(a.report_target_id, b.report_target_id);
    assert_eq!(a.checks_run, b.checks_run);
    assert_eq!(a.passed, b.passed);
    assert_eq!(a.failed, b.failed);

    let events = sink.snapshot().await;
    assert_eq!(events.len(), 2, "one metric per sweep");
}

#[tokio::test]
async fn disabled_config_returns_permanent_and_emits_nothing() {
    let store = Arc::new(memstore().await);
    let sink = Arc::new(CapturingMetricsSink::new());
    let dyn_store: Arc<dyn MemoryStore> = store.clone();
    let handler = EvaluationHandler::new(
        dyn_store,
        sink.clone() as Arc<dyn MetricsSink>,
        default_golden_checks(),
        EvaluationConfig {
            enabled: false,
            ..EvaluationConfig::default()
        },
    );
    let payload = EvaluationPayload {
        ts_ms: 0,
        check_ids: vec![],
    };
    let bytes = payload.to_bytes().expect("encode");
    let outcome = handler.handle(&bytes).await;
    assert!(matches!(outcome, HandlerOutcome::Permanent { .. }));
    assert!(sink.snapshot().await.is_empty());
}
