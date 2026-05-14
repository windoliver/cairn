// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use std::sync::{Arc, Mutex};

use cairn_core::domain::FlushPlan;
use cairn_core::generated::common::Ulid;
use cairn_core::generated::status::StatusResponseWorkflowsWorkflow;
use cairn_test_fixtures::flush_plan::sample_plan;
use cairn_workflows::{
    DrainStats, FlushPlanApply, FlushPlanApplyOutcome, Workflow, WorkflowContext, WorkflowDrainer,
    WorkflowError,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct StaticWorkflow {
    name: &'static str,
    plans: Vec<FlushPlan>,
}

#[async_trait::async_trait]
impl Workflow for StaticWorkflow {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn plan(&self, _ctx: &WorkflowContext) -> Result<Vec<FlushPlan>, WorkflowError> {
        Ok(self.plans.clone())
    }
}

#[derive(Default)]
struct RecordingApply {
    applied: Mutex<Vec<Ulid>>,
    already_applied: Mutex<Vec<Ulid>>,
}

#[async_trait::async_trait]
impl FlushPlanApply for RecordingApply {
    async fn apply(
        &self,
        workflow: &'static str,
        plan: FlushPlan,
    ) -> Result<FlushPlanApplyOutcome, WorkflowError> {
        assert_eq!(workflow, "expire");
        let id = plan.operation_id.clone();
        if self.already_applied.lock().expect("poisoned").contains(&id) {
            return Ok(FlushPlanApplyOutcome::AlreadyApplied);
        }
        self.applied.lock().expect("poisoned").push(id);
        Ok(FlushPlanApplyOutcome::Applied)
    }
}

struct CancellingApply {
    applied: Mutex<Vec<Ulid>>,
    cancel: CancellationToken,
}

#[async_trait::async_trait]
impl FlushPlanApply for CancellingApply {
    async fn apply(
        &self,
        _workflow: &'static str,
        plan: FlushPlan,
    ) -> Result<FlushPlanApplyOutcome, WorkflowError> {
        self.applied
            .lock()
            .expect("poisoned")
            .push(plan.operation_id.clone());
        self.cancel.cancel();
        Ok(FlushPlanApplyOutcome::Applied)
    }
}

#[tokio::test]
async fn drainer_applies_plans_in_order_and_reports_stats() {
    let plans = vec![
        plan("01HQZK000000000000000000V1"),
        plan("01HQZK000000000000000000V2"),
    ];
    let apply = Arc::new(RecordingApply::default());
    let drainer = WorkflowDrainer::new(WorkflowContext::default(), apply.clone());

    let stats = drainer
        .run(Arc::new(StaticWorkflow {
            name: "expire",
            plans,
        }))
        .await
        .expect("drain succeeds");

    assert_eq!(
        stats,
        DrainStats {
            workflow: "expire",
            planned: 2,
            applied: 2,
            already_applied: 0,
            cancelled: false,
        }
    );
    assert_eq!(
        *apply.applied.lock().expect("poisoned"),
        vec![
            ulid("01HQZK000000000000000000V1"),
            ulid("01HQZK000000000000000000V2")
        ]
    );
    let snapshots = drainer.status();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].workflow, "expire");
    assert_eq!(snapshots[0].drained_plans, 2);
    assert_eq!(snapshots[0].pending_plans, 0);
    assert!(snapshots[0].last_applied_at.is_some());
    let wire = WorkflowDrainer::status_to_wire(&snapshots);
    assert_eq!(wire[0].workflow, StatusResponseWorkflowsWorkflow::Expire);
    assert_eq!(wire[0].drained_plans, 2);
    assert_eq!(wire[0].pending_plans, 0);
}

#[tokio::test]
async fn drainer_stops_between_plans_when_cancelled() {
    let cancel = CancellationToken::new();
    let apply = Arc::new(CancellingApply {
        applied: Mutex::new(Vec::new()),
        cancel: cancel.clone(),
    });
    let drainer = WorkflowDrainer::new(WorkflowContext { cancel }, apply.clone());

    let stats = drainer
        .run(Arc::new(StaticWorkflow {
            name: "expire",
            plans: vec![
                plan("01HQZK000000000000000000V1"),
                plan("01HQZK000000000000000000V2"),
            ],
        }))
        .await
        .expect("drain succeeds");

    assert_eq!(stats.applied, 1);
    assert!(stats.cancelled);
    assert_eq!(
        *apply.applied.lock().expect("poisoned"),
        vec![ulid("01HQZK000000000000000000V1")]
    );
    let snapshots = drainer.status();
    assert_eq!(snapshots[0].drained_plans, 1);
    assert_eq!(snapshots[0].pending_plans, 1);
    assert!(snapshots[0].last_applied_at.is_some());
}

#[tokio::test]
async fn drainer_counts_already_applied_plan_as_success_without_reapply() {
    let apply = Arc::new(RecordingApply::default());
    apply
        .already_applied
        .lock()
        .expect("poisoned")
        .push(ulid("01HQZK000000000000000000V1"));
    let drainer = WorkflowDrainer::new(WorkflowContext::default(), apply.clone());

    let stats = drainer
        .run(Arc::new(StaticWorkflow {
            name: "expire",
            plans: vec![
                plan("01HQZK000000000000000000V1"),
                plan("01HQZK000000000000000000V2"),
            ],
        }))
        .await
        .expect("drain succeeds");

    assert_eq!(stats.applied, 1);
    assert_eq!(stats.already_applied, 1);
    assert_eq!(
        *apply.applied.lock().expect("poisoned"),
        vec![ulid("01HQZK000000000000000000V2")]
    );
    let snapshots = drainer.status();
    assert_eq!(snapshots[0].drained_plans, 2);
    assert_eq!(snapshots[0].pending_plans, 0);
    assert!(snapshots[0].last_applied_at.is_some());
}

#[tokio::test]
async fn drainer_reports_idle_workflow_with_no_last_applied_time() {
    let apply = Arc::new(RecordingApply::default());
    let drainer = WorkflowDrainer::new(WorkflowContext::default(), apply);

    let stats = drainer
        .run(Arc::new(StaticWorkflow {
            name: "expire",
            plans: Vec::new(),
        }))
        .await
        .expect("drain succeeds");

    assert_eq!(stats.planned, 0);
    assert!(!stats.cancelled);
    let snapshots = drainer.status();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].workflow, "expire");
    assert_eq!(snapshots[0].drained_plans, 0);
    assert_eq!(snapshots[0].pending_plans, 0);
    assert!(snapshots[0].last_applied_at.is_none());
}

#[tokio::test]
async fn drainer_reports_pending_plans_when_cancelled_before_first_apply() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let apply = Arc::new(RecordingApply::default());
    let drainer = WorkflowDrainer::new(WorkflowContext { cancel }, apply.clone());

    let stats = drainer
        .run(Arc::new(StaticWorkflow {
            name: "expire",
            plans: vec![
                plan("01HQZK000000000000000000V1"),
                plan("01HQZK000000000000000000V2"),
            ],
        }))
        .await
        .expect("drain succeeds");

    assert_eq!(stats.planned, 2);
    assert_eq!(stats.applied, 0);
    assert!(stats.cancelled);
    assert!(apply.applied.lock().expect("poisoned").is_empty());
    let snapshots = drainer.status();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].drained_plans, 0);
    assert_eq!(snapshots[0].pending_plans, 2);
    assert!(snapshots[0].last_applied_at.is_none());
}

#[tokio::test]
async fn drainer_keeps_cumulative_drained_count_across_runs() {
    let apply = Arc::new(RecordingApply::default());
    let drainer = WorkflowDrainer::new(WorkflowContext::default(), apply);

    drainer
        .run(Arc::new(StaticWorkflow {
            name: "expire",
            plans: vec![
                plan("01HQZK000000000000000000V1"),
                plan("01HQZK000000000000000000V2"),
            ],
        }))
        .await
        .expect("first drain succeeds");
    let first_last_applied_at = drainer.status()[0].last_applied_at.clone();

    drainer
        .run(Arc::new(StaticWorkflow {
            name: "expire",
            plans: Vec::new(),
        }))
        .await
        .expect("idle drain succeeds");

    let snapshots = drainer.status();
    assert_eq!(snapshots[0].drained_plans, 2);
    assert_eq!(snapshots[0].pending_plans, 0);
    assert_eq!(snapshots[0].last_applied_at, first_last_applied_at);
}

#[tokio::test]
async fn drainer_rejects_unknown_workflow_names_before_status_recording() {
    let apply = Arc::new(RecordingApply::default());
    let drainer = WorkflowDrainer::new(WorkflowContext::default(), apply);

    let err = drainer
        .run(Arc::new(StaticWorkflow {
            name: "custom",
            plans: Vec::new(),
        }))
        .await
        .expect_err("unknown workflow name must fail closed");

    assert!(err.to_string().contains("unknown workflow `custom`"));
    assert!(drainer.status().is_empty());
}

fn plan(id: &str) -> FlushPlan {
    sample_plan(id, cairn_core::domain::FlushMode::Autonomous)
}

fn ulid(id: &str) -> Ulid {
    Ulid(id.to_owned())
}
