// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::contract::memory_store::MemoryStore as _;
use cairn_core::domain::{ExpirationReason, FlushMode, Identity, PlannedMutation};
use cairn_test_fixtures::{FixtureStore, memstore, sample_record};
use cairn_workflows::{ExpirePlanSource, WorkflowContext, WorkflowPlanSource};

#[tokio::test]
async fn expire_plan_source_emits_expire_plan_for_low_salience_records() {
    let store = Arc::new(FixtureStore::new());
    let mut low = sample_record(1);
    low.salience = 0.1;
    let mut high = sample_record(2);
    high.salience = 0.9;
    store.upsert(&low).await.expect("seed low salience");
    store.upsert(&high).await.expect("seed high salience");

    let source = ExpirePlanSource::new(
        store,
        Identity::parse("agt:cairn-workflows:expire:v1").expect("valid issuer"),
        0.2,
    );

    let plans = source
        .plan("expire", &WorkflowContext::default())
        .await
        .expect("plan");

    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].mode, FlushMode::Autonomous);
    assert_eq!(plans[0].scope, low.scope);
    assert_eq!(plans[0].mutations.len(), 1);
    assert!(matches!(
        &plans[0].mutations[0],
        PlannedMutation::Expire {
            target,
            reason: ExpirationReason::SalienceBelowThreshold,
        } if *target == low.target_id
    ));
}

#[tokio::test]
async fn expire_plan_source_returns_empty_for_other_workflow_names() {
    let store = Arc::new(FixtureStore::new());
    let source = ExpirePlanSource::new(
        store,
        Identity::parse("agt:cairn-workflows:expire:v1").expect("valid issuer"),
        0.2,
    );

    let plans = source
        .plan("promote", &WorkflowContext::default())
        .await
        .expect("plan");

    assert!(plans.is_empty());
}

#[tokio::test]
async fn expire_plan_source_paginates_past_first_store_page() {
    let store = Arc::new(memstore().await);
    let mut low = sample_record(1);
    low.salience = 0.1;
    store.upsert(&low).await.expect("seed low salience");

    for seed in 2..=3 {
        let mut high = sample_record(seed);
        high.salience = 0.9;
        store.upsert(&high).await.expect("seed high salience");
    }

    let source = ExpirePlanSource::new(
        store,
        Identity::parse("agt:cairn-workflows:expire:v1").expect("valid issuer"),
        0.2,
    )
    .with_page_limit(2);

    let plans = source
        .plan("expire", &WorkflowContext::default())
        .await
        .expect("plan");

    assert_eq!(plans.len(), 1);
    assert!(matches!(
        &plans[0].mutations[0],
        PlannedMutation::Expire { target, .. } if *target == low.target_id
    ));
}
