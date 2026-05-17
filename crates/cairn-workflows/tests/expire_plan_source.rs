// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::contract::memory_store::MemoryStore as _;
use cairn_core::domain::record::Ed25519Signature;
use cairn_core::domain::taxonomy::MemoryClass;
use cairn_core::domain::{
    ExpirationReason, FlushMode, Identity, MemoryKind, PlanReason, PlannedMutation,
};
use cairn_test_fixtures::{FixtureStore, memstore, sample_record};
use cairn_workflows::{
    ConsolidatePlanSource, ExpirePlanSource, PromotePlanSource, ReflectionPlanSource,
    WorkflowContext, WorkflowPlanSource,
};

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
    assert!(matches!(
        plans[0].reason,
        PlanReason::Expire {
            ttl_expired: false,
            salience_below: Some(salience),
        } if (salience - low.salience).abs() < f32::EPSILON
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

#[tokio::test]
async fn expire_plan_source_reuses_operation_ids_for_same_candidates() {
    let store = Arc::new(FixtureStore::new());
    let mut low = sample_record(9);
    low.salience = 0.1;
    store.upsert(&low).await.expect("seed low salience");

    let source = ExpirePlanSource::new(
        store,
        Identity::parse("agt:cairn-workflows:expire:v1").expect("valid issuer"),
        0.2,
    );

    let first = source
        .plan("expire", &WorkflowContext::default())
        .await
        .expect("first plan");
    let second = source
        .plan("expire", &WorkflowContext::default())
        .await
        .expect("second plan");

    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    assert_eq!(first[0].operation_id, second[0].operation_id);
}

#[tokio::test]
async fn promote_plan_source_emits_promote_plan_for_confident_non_target_kind() {
    let store = Arc::new(memstore().await);
    let mut candidate = sample_record(4);
    candidate.kind = MemoryKind::Reference;
    candidate.confidence = 0.95;
    let mut already_promoted = sample_record(5);
    already_promoted.kind = MemoryKind::Fact;
    already_promoted.confidence = 0.99;
    store.upsert(&candidate).await.expect("seed candidate");
    store
        .upsert(&already_promoted)
        .await
        .expect("seed promoted");

    let source = PromotePlanSource::new(
        store,
        Identity::parse("agt:cairn-workflows:promote:v1").expect("valid issuer"),
        MemoryKind::Fact,
        0.9,
    );

    let plans = source
        .plan("promote", &WorkflowContext::default())
        .await
        .expect("plan");

    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].mode, FlushMode::Autonomous);
    assert!(matches!(
        &plans[0].mutations[0],
        PlannedMutation::Promote {
            from,
            to_kind: MemoryKind::Fact,
            evidence,
        } if from == &candidate.target_id && evidence.is_empty()
    ));
    assert!(matches!(
        plans[0].reason,
        PlanReason::Promote {
            confidence,
            evidence_count: 0,
        } if (confidence - 0.95).abs() < f32::EPSILON
    ));
}

#[tokio::test]
async fn promote_plan_source_reuses_operation_ids_for_same_candidates() {
    let store = Arc::new(memstore().await);
    let mut candidate = sample_record(10);
    candidate.kind = MemoryKind::Reference;
    candidate.confidence = 0.95;
    store.upsert(&candidate).await.expect("seed candidate");

    let source = PromotePlanSource::new(
        store,
        Identity::parse("agt:cairn-workflows:promote:v1").expect("valid issuer"),
        MemoryKind::Fact,
        0.9,
    );

    let first = source
        .plan("promote", &WorkflowContext::default())
        .await
        .expect("first plan");
    let second = source
        .plan("promote", &WorkflowContext::default())
        .await
        .expect("second plan");

    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    assert_eq!(first[0].operation_id, second[0].operation_id);
}

#[tokio::test]
async fn consolidate_plan_source_expires_duplicate_bodies_but_keeps_best_record() {
    let store = Arc::new(memstore().await);
    let mut keeper = sample_record(6);
    keeper.body = "shared body".to_owned();
    keeper.confidence = 0.9;
    keeper.salience = 0.8;
    let mut duplicate = sample_record(7);
    duplicate.body = "shared body".to_owned();
    duplicate.confidence = 0.6;
    duplicate.salience = 0.4;
    let mut unique = sample_record(8);
    unique.body = "unique body".to_owned();
    unique.confidence = 0.1;
    store.upsert(&keeper).await.expect("seed keeper");
    store.upsert(&duplicate).await.expect("seed duplicate");
    store.upsert(&unique).await.expect("seed unique");

    let source = ConsolidatePlanSource::new(
        store,
        Identity::parse("agt:cairn-workflows:consolidate:v1").expect("valid issuer"),
    );

    let plans = source
        .plan("consolidate", &WorkflowContext::default())
        .await
        .expect("plan");

    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].mode, FlushMode::Autonomous);
    assert!(matches!(
        &plans[0].mutations[0],
        PlannedMutation::Expire {
            target,
            reason: ExpirationReason::SupersededByCanonical,
        } if target == &duplicate.target_id
    ));
}

#[tokio::test]
async fn consolidate_plan_source_reuses_operation_ids_for_same_duplicates() {
    let store = Arc::new(memstore().await);
    let mut keeper = sample_record(11);
    keeper.body = "shared body".to_owned();
    keeper.confidence = 0.9;
    keeper.salience = 0.8;
    let mut duplicate = sample_record(12);
    duplicate.body = "shared body".to_owned();
    duplicate.confidence = 0.6;
    duplicate.salience = 0.4;
    store.upsert(&keeper).await.expect("seed keeper");
    store.upsert(&duplicate).await.expect("seed duplicate");

    let source = ConsolidatePlanSource::new(
        store,
        Identity::parse("agt:cairn-workflows:consolidate:v1").expect("valid issuer"),
    );

    let first = source
        .plan("consolidate", &WorkflowContext::default())
        .await
        .expect("first plan");
    let second = source
        .plan("consolidate", &WorkflowContext::default())
        .await
        .expect("second plan");

    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    assert_eq!(first[0].operation_id, second[0].operation_id);
}

#[tokio::test]
async fn reflection_plan_source_emits_upsert_candidate_with_evidence_links() {
    let store = Arc::new(memstore().await);
    for seed in 20..=22 {
        let mut record = sample_record(seed);
        record.kind = MemoryKind::Feedback;
        record.class = MemoryClass::Episodic;
        record.body = "Always run privacy bypass tests".to_owned();
        record.salience = 0.8;
        record.confidence = 0.8;
        store.upsert(&record).await.expect("seed reflection signal");
    }

    let source = ReflectionPlanSource::new(
        store,
        Identity::parse("agt:cairn-workflows:reflection:v1").expect("valid issuer"),
    );

    let plans = source
        .plan("reflect", &WorkflowContext::default())
        .await
        .expect("plan");

    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].mode, FlushMode::Autonomous);
    let expected_scope = sample_record(20).scope;
    assert_eq!(plans[0].scope, expected_scope);
    assert!(matches!(
        plans[0].reason,
        PlanReason::Reflect {
            candidate_kind: MemoryKind::Rule,
            evidence_count: 3,
        }
    ));
    let PlannedMutation::Upsert { record, .. } = &plans[0].mutations[0] else {
        panic!("expected reflection upsert");
    };
    assert_eq!(record.kind, MemoryKind::Rule);
    assert_eq!(record.class, MemoryClass::Procedural);
    assert_eq!(record.scope, expected_scope);
    record
        .validate()
        .expect("reflection candidate record validates");
    let evidence = record
        .extra_frontmatter
        .get("reflection")
        .and_then(|v| v.get("evidence_record_ids"))
        .and_then(|v| v.as_array())
        .expect("reflection evidence ids");
    assert_eq!(evidence.len(), 3);
}

#[tokio::test]
async fn reflection_plan_source_emits_review_plan_for_low_confidence_candidates() {
    let store = Arc::new(memstore().await);
    for seed in 23..=25 {
        let mut record = sample_record(seed);
        record.kind = MemoryKind::Feedback;
        record.class = MemoryClass::Episodic;
        record.body = "Prefer cargo nextest".to_owned();
        record.salience = 0.8;
        record.confidence = 0.45;
        store.upsert(&record).await.expect("seed reflection signal");
    }

    let source = ReflectionPlanSource::new(
        store,
        Identity::parse("agt:cairn-workflows:reflection:v1").expect("valid issuer"),
    );

    let plans = source
        .plan("reflect", &WorkflowContext::default())
        .await
        .expect("plan");

    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].mode, FlushMode::HumanReview);
}

#[tokio::test]
async fn reflection_plan_source_ignores_unsigned_source_records() {
    let store = Arc::new(memstore().await);
    for seed in 26..=28 {
        let mut record = sample_record(seed);
        record.kind = MemoryKind::Feedback;
        record.class = MemoryClass::Episodic;
        record.body = "Always run privacy bypass tests".to_owned();
        record.salience = 0.8;
        record.confidence = 0.8;
        record.signature = Ed25519Signature::flush_mutated_sentinel();
        store.upsert(&record).await.expect("seed reflection signal");
    }

    let source = ReflectionPlanSource::new(
        store,
        Identity::parse("agt:cairn-workflows:reflection:v1").expect("valid issuer"),
    );

    let plans = source
        .plan("reflect", &WorkflowContext::default())
        .await
        .expect("plan");

    assert!(plans.is_empty());
}

#[tokio::test]
async fn reflection_plan_source_maps_repeated_trace_errors_to_knowledge_gap() {
    let store = Arc::new(memstore().await);
    for seed in 29..=31 {
        let mut record = sample_record(seed);
        record.kind = MemoryKind::Trace;
        record.class = MemoryClass::Episodic;
        record.body = "tool failed: sqlite vec index missing".to_owned();
        record.salience = 0.8;
        record.confidence = 0.8;
        store.upsert(&record).await.expect("seed reflection signal");
    }

    let source = ReflectionPlanSource::new(
        store,
        Identity::parse("agt:cairn-workflows:reflection:v1").expect("valid issuer"),
    );

    let plans = source
        .plan("reflect", &WorkflowContext::default())
        .await
        .expect("plan");

    assert_eq!(plans.len(), 1);
    assert!(matches!(
        plans[0].reason,
        PlanReason::Reflect {
            candidate_kind: MemoryKind::KnowledgeGap,
            evidence_count: 3,
        }
    ));
}

#[tokio::test]
async fn reflection_plan_source_returns_empty_for_other_workflow_names() {
    let store = Arc::new(memstore().await);
    for seed in 32..=34 {
        let mut record = sample_record(seed);
        record.kind = MemoryKind::Feedback;
        record.body = "Always run privacy bypass tests".to_owned();
        record.salience = 0.8;
        record.confidence = 0.8;
        store.upsert(&record).await.expect("seed reflection signal");
    }

    let source = ReflectionPlanSource::new(
        store,
        Identity::parse("agt:cairn-workflows:reflection:v1").expect("valid issuer"),
    );

    let plans = source
        .plan("expire", &WorkflowContext::default())
        .await
        .expect("plan");

    assert!(plans.is_empty());
}

#[tokio::test]
async fn reflection_plan_source_paginates_past_first_store_page() {
    let store = Arc::new(memstore().await);
    for seed in 35..=37 {
        let mut record = sample_record(seed);
        record.kind = MemoryKind::Feedback;
        record.body = "Prefer deterministic reflection plans".to_owned();
        record.salience = 0.8;
        record.confidence = 0.8;
        store.upsert(&record).await.expect("seed reflection signal");
    }

    let source = ReflectionPlanSource::new(
        store,
        Identity::parse("agt:cairn-workflows:reflection:v1").expect("valid issuer"),
    )
    .with_page_limit(2);

    let plans = source
        .plan("reflect", &WorkflowContext::default())
        .await
        .expect("plan");

    assert_eq!(plans.len(), 1);
    assert!(matches!(
        plans[0].reason,
        PlanReason::Reflect {
            candidate_kind: MemoryKind::Rule,
            evidence_count: 3,
        }
    ));
}

#[tokio::test]
async fn reflection_plan_source_emits_single_novel_entity_candidate() {
    let store = Arc::new(memstore().await);
    let mut record = sample_record(38);
    record.kind = MemoryKind::Entity;
    record.class = MemoryClass::Semantic;
    record.body = "Cairn ReflectionWorkflow".to_owned();
    record.salience = 0.8;
    record.confidence = 0.8;
    store.upsert(&record).await.expect("seed reflection signal");

    let source = ReflectionPlanSource::new(
        store,
        Identity::parse("agt:cairn-workflows:reflection:v1").expect("valid issuer"),
    );

    let plans = source
        .plan("reflect", &WorkflowContext::default())
        .await
        .expect("plan");

    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].mode, FlushMode::Autonomous);
    assert!(matches!(
        plans[0].reason,
        PlanReason::Reflect {
            candidate_kind: MemoryKind::Entity,
            evidence_count: 1,
        }
    ));
}

#[tokio::test]
async fn reflection_plan_source_does_not_merge_repeated_patterns_across_scopes() {
    let store = Arc::new(memstore().await);
    for seed in 39..=41 {
        let mut record = sample_record(seed);
        record.kind = MemoryKind::Feedback;
        record.body = "Prefer deterministic reflection plans".to_owned();
        record.salience = 0.8;
        record.confidence = 0.8;
        if seed == 41 {
            record.scope.user = Some("hmn:other-user".to_owned());
        }
        store.upsert(&record).await.expect("seed reflection signal");
    }

    let source = ReflectionPlanSource::new(
        store,
        Identity::parse("agt:cairn-workflows:reflection:v1").expect("valid issuer"),
    );

    let plans = source
        .plan("reflect", &WorkflowContext::default())
        .await
        .expect("plan");

    assert!(plans.is_empty());
}

#[tokio::test]
async fn reflection_plan_source_discards_low_salience_patterns() {
    let store = Arc::new(memstore().await);
    for seed in 42..=44 {
        let mut record = sample_record(seed);
        record.kind = MemoryKind::Feedback;
        record.body = "Prefer deterministic reflection plans".to_owned();
        record.salience = 0.2;
        record.confidence = 0.8;
        store.upsert(&record).await.expect("seed reflection signal");
    }

    let source = ReflectionPlanSource::new(
        store,
        Identity::parse("agt:cairn-workflows:reflection:v1").expect("valid issuer"),
    );

    let plans = source
        .plan("reflect", &WorkflowContext::default())
        .await
        .expect("plan");

    assert!(plans.is_empty());
}
