// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::contract::memory_store::MemoryStore as _;
use cairn_core::domain::Identity;
use cairn_core::generated::status::StatusResponseWorkflowsWorkflow;
use cairn_test_fixtures::{memstore, sample_record};
use cairn_workflows::{
    ExpirePlanSource, ExpireWorkflow, SqliteFlushPlanApply, WorkflowContext, WorkflowDrainer,
};

#[tokio::test]
async fn expire_workflow_drains_planned_flushes_into_sqlite_store_end_to_end() {
    let store = Arc::new(memstore().await);
    let mut low = sample_record(1);
    low.salience = 0.1;
    let mut high = sample_record(2);
    high.salience = 0.9;
    store.upsert(&low).await.expect("seed low salience");
    store.upsert(&high).await.expect("seed high salience");

    let source = Arc::new(ExpirePlanSource::new(
        store.clone(),
        Identity::parse("agt:cairn-workflows:expire:v1").expect("valid issuer"),
        0.2,
    ));
    let drainer = WorkflowDrainer::new(
        WorkflowContext::default(),
        Arc::new(SqliteFlushPlanApply::new(store.clone())),
    );

    let first = drainer
        .run(Arc::new(ExpireWorkflow::new(source.clone())))
        .await
        .expect("first expire drain");

    assert_eq!(first.workflow, "expire");
    assert_eq!(first.planned, 1);
    assert_eq!(first.applied, 1);
    assert_eq!(first.already_applied, 0);
    assert!(!first.cancelled);
    assert!(
        store
            .get_active_by_target(&low.target_id)
            .await
            .expect("get low target")
            .is_none()
    );
    assert!(
        store
            .get_active_by_target(&high.target_id)
            .await
            .expect("get high target")
            .is_some()
    );

    let status = drainer.status();
    assert_eq!(status.len(), 1);
    assert_eq!(status[0].workflow, "expire");
    assert_eq!(status[0].drained_plans, 1);
    assert_eq!(status[0].pending_plans, 0);
    assert!(status[0].last_applied_at.is_some());

    let wire = WorkflowDrainer::status_to_wire(&status);
    assert_eq!(wire.len(), 1);
    assert_eq!(wire[0].workflow, StatusResponseWorkflowsWorkflow::Expire);
    assert_eq!(wire[0].drained_plans, 1);
    assert_eq!(wire[0].pending_plans, 0);
    assert!(wire[0].last_applied_at.is_some());

    let second = drainer
        .run(Arc::new(ExpireWorkflow::new(source)))
        .await
        .expect("second expire drain");

    assert_eq!(second.workflow, "expire");
    assert_eq!(second.planned, 0);
    assert_eq!(second.applied, 0);
    assert_eq!(second.already_applied, 0);
    assert!(!second.cancelled);
    let status = drainer.status();
    assert_eq!(status[0].drained_plans, 1);
    assert_eq!(status[0].pending_plans, 0);
}
