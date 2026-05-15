// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::contract::memory_store::MemoryStore as _;
use cairn_core::domain::{Identity, MemoryKind};
use cairn_core::generated::status::StatusResponseWorkflowsWorkflow;
use cairn_test_fixtures::{memstore, sample_record};
use cairn_workflows::{
    ConsolidatePlanSource, ConsolidateWorkflow, ExpirePlanSource, ExpireWorkflow,
    PromotePlanSource, PromoteWorkflow, SqliteFlushPlanApply, WorkflowContext, WorkflowDrainer,
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

#[tokio::test]
async fn promote_workflow_drains_planned_flushes_into_sqlite_store_end_to_end() {
    let store = Arc::new(memstore().await);
    let mut candidate = sample_record(3);
    candidate.kind = MemoryKind::Reference;
    candidate.confidence = 0.95;
    let mut already_promoted = sample_record(4);
    already_promoted.kind = MemoryKind::Fact;
    already_promoted.confidence = 0.99;
    store.upsert(&candidate).await.expect("seed candidate");
    store
        .upsert(&already_promoted)
        .await
        .expect("seed already promoted");

    let source = Arc::new(PromotePlanSource::new(
        store.clone(),
        Identity::parse("agt:cairn-workflows:promote:v1").expect("valid issuer"),
        MemoryKind::Fact,
        0.9,
    ));
    let drainer = WorkflowDrainer::new(
        WorkflowContext::default(),
        Arc::new(SqliteFlushPlanApply::new(store.clone())),
    );

    let first = drainer
        .run(Arc::new(PromoteWorkflow::new(source.clone())))
        .await
        .expect("first promote drain");

    assert_eq!(first.workflow, "promote");
    assert_eq!(first.planned, 1);
    assert_eq!(first.applied, 1);
    assert_eq!(first.already_applied, 0);
    assert!(!first.cancelled);
    let promoted = store
        .get_active_by_target(&candidate.target_id)
        .await
        .expect("get candidate")
        .expect("candidate remains active");
    assert_eq!(promoted.record.kind, MemoryKind::Fact);

    let second = drainer
        .run(Arc::new(PromoteWorkflow::new(source)))
        .await
        .expect("second promote drain");

    assert_eq!(second.workflow, "promote");
    assert_eq!(second.planned, 0);
    assert_eq!(second.applied, 0);
    assert_eq!(second.already_applied, 0);
    assert!(!second.cancelled);
}

#[tokio::test]
async fn consolidate_workflow_drains_planned_flushes_into_sqlite_store_end_to_end() {
    let store = Arc::new(memstore().await);
    let mut keeper = sample_record(5);
    keeper.body = "shared body".to_owned();
    keeper.confidence = 0.9;
    keeper.salience = 0.8;
    let mut duplicate = sample_record(6);
    duplicate.body = "shared body".to_owned();
    duplicate.confidence = 0.6;
    duplicate.salience = 0.4;
    let mut unique = sample_record(7);
    unique.body = "unique body".to_owned();
    unique.confidence = 0.1;
    store.upsert(&keeper).await.expect("seed keeper");
    store.upsert(&duplicate).await.expect("seed duplicate");
    store.upsert(&unique).await.expect("seed unique");

    let source = Arc::new(ConsolidatePlanSource::new(
        store.clone(),
        Identity::parse("agt:cairn-workflows:consolidate:v1").expect("valid issuer"),
    ));
    let drainer = WorkflowDrainer::new(
        WorkflowContext::default(),
        Arc::new(SqliteFlushPlanApply::new(store.clone())),
    );

    let first = drainer
        .run(Arc::new(ConsolidateWorkflow::new(source.clone())))
        .await
        .expect("first consolidate drain");

    assert_eq!(first.workflow, "consolidate");
    assert_eq!(first.planned, 1);
    assert_eq!(first.applied, 1);
    assert_eq!(first.already_applied, 0);
    assert!(!first.cancelled);
    assert!(
        store
            .get_active_by_target(&keeper.target_id)
            .await
            .expect("get keeper")
            .is_some()
    );
    assert!(
        store
            .get_active_by_target(&duplicate.target_id)
            .await
            .expect("get duplicate")
            .is_none()
    );
    assert!(
        store
            .get_active_by_target(&unique.target_id)
            .await
            .expect("get unique")
            .is_some()
    );

    let second = drainer
        .run(Arc::new(ConsolidateWorkflow::new(source)))
        .await
        .expect("second consolidate drain");

    assert_eq!(second.workflow, "consolidate");
    assert_eq!(second.planned, 0);
    assert_eq!(second.applied, 0);
    assert_eq!(second.already_applied, 0);
    assert!(!second.cancelled);
}
