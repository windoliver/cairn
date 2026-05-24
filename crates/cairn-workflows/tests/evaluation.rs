//! Integration: `EvaluationHandler` runs the starter golden checks,
//! upserts a deterministic report record, and emits a single
//! `MetricEvent::EvaluationCompleted` per sweep. AC#3 of issue #91 —
//! "evaluation outputs are deterministic enough for release gating".

use std::sync::Arc;

use cairn_core::config::EvaluationConfig;
use cairn_core::contract::agent_provider::AgentBudgetConsumed;
use cairn_core::contract::memory_store::{ListArgs, MemoryStore};
use cairn_core::contract::metrics::{CapturingMetricsSink, MetricsSink};
use cairn_core::domain::metrics::MetricEvent;
use cairn_core::domain::taxonomy::MemoryKind;
use cairn_core::domain::{
    AgentWorkerAuditRecord, AgentWorkerFailureMode, AgentWorkerKind, AgentWorkerStatus, ScopeTuple,
};
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
        bound_scope: None,
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
        bound_scope: None,
    };

    let a = handler.run_once(&payload).await.expect("first sweep");
    let b = handler.run_once(&payload).await.expect("second sweep");
    assert_eq!(a.report_target_id, b.report_target_id);
    assert_eq!(a.checks_run, b.checks_run);
    assert_eq!(a.passed, b.passed);
    assert_eq!(a.failed, b.failed);

    // Brief §15 release gating semantics: at-least-once metric
    // emission with deterministic `report_target_id`. Downstream
    // consumers dedupe on `(report_target_id, ts_ms)` — the
    // alternative (skip-on-replay) loses the metric permanently
    // when the first emit fails (round-2 adversarial review #1).
    let events = sink.snapshot().await;
    assert_eq!(events.len(), 2, "two replays produce two metric lines");
    let target_ids: std::collections::BTreeSet<String> = events
        .iter()
        .filter_map(|e| match e {
            MetricEvent::EvaluationCompleted {
                report_target_id, ..
            } => Some(report_target_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        target_ids.len(),
        1,
        "both replays carry the same report_target_id so downstream can dedupe"
    );
}

#[tokio::test]
async fn audit_state_changes_persisted_outcome_hash() {
    let store = Arc::new(memstore().await);
    let sink = Arc::new(CapturingMetricsSink::new());
    let dyn_store: Arc<dyn MemoryStore> = store.clone();
    let payload = EvaluationPayload {
        ts_ms: 1_700_000_000_000,
        check_ids: vec![],
        bound_scope: None,
    };

    let without_audit = EvaluationHandler::new(
        dyn_store.clone(),
        sink.clone() as Arc<dyn MetricsSink>,
        default_golden_checks(),
        EvaluationConfig {
            enabled: true,
            write_report_record: true,
            ..EvaluationConfig::default()
        },
    );
    let with_audit = EvaluationHandler::new(
        dyn_store,
        sink.clone() as Arc<dyn MetricsSink>,
        default_golden_checks(),
        EvaluationConfig {
            enabled: true,
            write_report_record: true,
            ..EvaluationConfig::default()
        },
    )
    .with_agent_worker_audit(vec![AgentWorkerAuditRecord {
        operation_id: "op-agent-1".to_owned(),
        worker_kind: AgentWorkerKind::Dream,
        worker_name: "agent_dream".to_owned(),
        agent_identity: "agt:cairn-dream:v1".to_owned(),
        scope: Some(ScopeTuple {
            tenant: Some("tenant-a".to_owned()),
            agent: Some("agt:cairn-dream:v1".to_owned()),
            ..ScopeTuple::default()
        }),
        status: AgentWorkerStatus::Aborted,
        generated_candidates: 2,
        accepted_candidates: 1,
        budget_consumed: AgentBudgetConsumed {
            turns: 1,
            tool_calls: 2,
            cost_units: 99,
        },
        failure_mode: Some(AgentWorkerFailureMode::ProviderUnavailable),
        canary_label: Some("canary-05".to_owned()),
    }]);

    let a = without_audit.run_once(&payload).await.expect("first sweep");
    let b = with_audit.run_once(&payload).await.expect("second sweep");
    assert_ne!(a.report_target_id, b.report_target_id);

    let listed = store
        .list(&ListArgs {
            limit: 100,
            ..ListArgs::default()
        })
        .await
        .expect("list");
    let hashes: std::collections::BTreeSet<String> = listed
        .records
        .iter()
        .filter(|r| r.kind == MemoryKind::Reasoning && r.body.starts_with("# Evaluation report"))
        .filter_map(|r| {
            r.extra_frontmatter
                .get("evaluation")
                .and_then(|v| v.get("outcome_hash"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect();

    assert_eq!(
        hashes.len(),
        2,
        "audit state must participate in persisted outcome_hash"
    );

    let audit_report = listed
        .records
        .iter()
        .find(|r| r.body.contains("agent_dream"))
        .expect("audit report body includes per-worker details");
    assert!(audit_report.body.contains(
        "- dream `agent_dream` (canary-05): runs 1 (completed 0, failed 1), accepted candidates 1 / 2 (rate 0.500), turns 1, cost units 99, tool calls 2, failures provider_unavailable=1"
    ));
}

#[tokio::test]
async fn metric_id_is_deterministic_even_without_report_record() {
    // Round-3 adversarial review #2: when write_report_record=false,
    // the metric must still carry a stable, non-empty
    // `report_target_id` so downstream `(report_target_id, ts_ms)`
    // dedupe can collapse retries correctly.
    let store = Arc::new(memstore().await);
    let sink = Arc::new(CapturingMetricsSink::new());
    let dyn_store: Arc<dyn MemoryStore> = store.clone();
    let handler = EvaluationHandler::new(
        dyn_store,
        sink.clone() as Arc<dyn MetricsSink>,
        default_golden_checks(),
        EvaluationConfig {
            enabled: true,
            write_report_record: false,
            ..EvaluationConfig::default()
        },
    );
    let payload = EvaluationPayload {
        ts_ms: 1_700_000_000_000,
        check_ids: vec![],
        bound_scope: None,
    };
    handler.run_once(&payload).await.expect("sweep");
    let events = sink.snapshot().await;
    assert_eq!(events.len(), 1);
    match &events[0] {
        MetricEvent::EvaluationCompleted {
            report_target_id, ..
        } => {
            assert!(
                !report_target_id.is_empty(),
                "report_target_id must be deterministic even with no report record"
            );
        }
        _ => panic!("expected EvaluationCompleted"),
    }
}

#[tokio::test]
async fn unknown_check_id_returns_permanent() {
    // Round-2 adversarial review #3: a typo'd / removed check id
    // must NOT silently produce a successful zero-check sweep.
    let store = Arc::new(memstore().await);
    let sink = Arc::new(CapturingMetricsSink::new());
    let dyn_store: Arc<dyn MemoryStore> = store.clone();
    let handler = EvaluationHandler::new(
        dyn_store,
        sink.clone() as Arc<dyn MetricsSink>,
        default_golden_checks(),
        EvaluationConfig {
            enabled: true,
            ..EvaluationConfig::default()
        },
    );
    let payload = EvaluationPayload {
        ts_ms: 0,
        check_ids: vec!["this-check-does-not-exist".into()],
        bound_scope: None,
    };
    let bytes = payload.to_bytes().expect("encode");
    let outcome = handler.handle(&bytes).await;
    assert!(matches!(outcome, HandlerOutcome::Permanent { .. }));
    assert!(sink.snapshot().await.is_empty());
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
        bound_scope: None,
    };
    let bytes = payload.to_bytes().expect("encode");
    let outcome = handler.handle(&bytes).await;
    assert!(matches!(outcome, HandlerOutcome::Permanent { .. }));
    assert!(sink.snapshot().await.is_empty());
}
