// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::contract::memory_store::MemoryStore as _;
use cairn_core::domain::flush_plan::PatchTarget;
use cairn_core::domain::{ExpirationReason, FlushMode, MemoryKind, PlannedMutation};
use cairn_test_fixtures::{flush_plan::sample_plan, memstore, sample_record};
use cairn_workflows::{FlushPlanApply, FlushPlanApplyOutcome, SqliteFlushPlanApply, WorkflowError};

#[tokio::test]
async fn sqlite_apply_rejects_unsupported_mutation_without_partial_success() {
    let apply =
        SqliteFlushPlanApply::new(Arc::new(cairn_store_sqlite::SqliteMemoryStore::default()));
    let mut plan = sample_plan("01HQZK000000000000000000A1", FlushMode::Autonomous);
    plan.mutations = vec![PlannedMutation::Patch {
        target: PatchTarget::Record(
            cairn_core::domain::TargetId::parse("01HQZX9F5N0000000000000000").unwrap(),
        ),
        str_replace: vec![],
    }];

    let err = apply
        .apply("promote", plan)
        .await
        .expect_err("patch is not wired to store apply yet");

    assert!(
        err.to_string()
            .contains("unsupported mutation kind `patch`")
    );
}

#[tokio::test]
async fn sqlite_apply_treats_empty_plan_as_already_applied_noop() {
    let apply =
        SqliteFlushPlanApply::new(Arc::new(cairn_store_sqlite::SqliteMemoryStore::default()));
    let mut plan = sample_plan("01HQZK000000000000000000A2", FlushMode::Autonomous);
    plan.mutations.clear();

    let outcome = apply
        .apply("expire", plan)
        .await
        .expect("empty plan is idempotent noop");

    assert_eq!(outcome, FlushPlanApplyOutcome::AlreadyApplied);
}

#[tokio::test]
async fn sqlite_apply_preflights_all_mutations_before_applying_any() {
    let store = Arc::new(memstore().await);
    let apply = SqliteFlushPlanApply::new(store.clone());
    let record = sample_record(42);
    let mut plan = sample_plan("01HQZK000000000000000000A3", FlushMode::Autonomous);
    plan.mutations = vec![
        PlannedMutation::Upsert {
            record: Box::new(record.clone()),
            prior_version: None,
        },
        PlannedMutation::Patch {
            target: PatchTarget::Record(record.target_id.clone()),
            str_replace: vec![],
        },
    ];

    let err = apply
        .apply("promote", plan)
        .await
        .expect_err("unsupported patch should fail before upsert");

    assert!(
        err.to_string()
            .contains("unsupported mutation kind `patch`")
    );
    assert!(store.get(&record.id).await.expect("get").is_none());
}

#[tokio::test]
async fn sqlite_apply_forget_record_resolves_target_to_active_record_id() {
    let store = Arc::new(memstore().await);
    let apply = SqliteFlushPlanApply::new(store.clone());
    let record = sample_record(7);
    store.upsert(&record).await.expect("seed record");

    let mut plan = sample_plan("01HQZK000000000000000000A4", FlushMode::Autonomous);
    plan.mutations = vec![PlannedMutation::ForgetRecord {
        target: record.target_id.clone(),
    }];

    let outcome = apply.apply("expire", plan).await.expect("apply forget");

    assert_eq!(outcome, FlushPlanApplyOutcome::Applied);
    assert!(store.get(&record.id).await.expect("get").is_none());
}

#[tokio::test]
async fn sqlite_apply_delete_checks_prior_version_before_forget() {
    let store = Arc::new(memstore().await);
    let apply = SqliteFlushPlanApply::new(store.clone());
    let record = sample_record(8);
    store.upsert(&record).await.expect("seed record");

    let mut stale = sample_plan("01HQZK000000000000000000A5", FlushMode::Autonomous);
    stale.mutations = vec![PlannedMutation::Delete {
        target: record.target_id.clone(),
        prior_version: 2,
    }];
    apply
        .apply("expire", stale)
        .await
        .expect_err("stale prior_version rejected");
    assert!(store.get(&record.id).await.expect("get").is_some());

    let mut current = sample_plan("01HQZK000000000000000000A6", FlushMode::Autonomous);
    current.mutations = vec![PlannedMutation::Delete {
        target: record.target_id.clone(),
        prior_version: 1,
    }];
    let outcome = apply.apply("expire", current).await.expect("delete");

    assert_eq!(outcome, FlushPlanApplyOutcome::Applied);
    assert!(store.get(&record.id).await.expect("get").is_none());
}

#[tokio::test]
async fn sqlite_apply_preflights_delete_versions_before_any_mutation() {
    let store = Arc::new(memstore().await);
    let apply = SqliteFlushPlanApply::new(store.clone());
    let existing = sample_record(9);
    let inserted = sample_record(10);
    store.upsert(&existing).await.expect("seed record");

    let mut stale = sample_plan("01HQZK000000000000000000A7", FlushMode::Autonomous);
    stale.mutations = vec![
        PlannedMutation::Upsert {
            record: Box::new(inserted.clone()),
            prior_version: None,
        },
        PlannedMutation::Delete {
            target: existing.target_id.clone(),
            prior_version: 2,
        },
    ];

    apply
        .apply("expire", stale)
        .await
        .expect_err("stale delete should fail before upsert");

    assert!(
        store
            .get(&inserted.id)
            .await
            .expect("get inserted")
            .is_none()
    );
    assert!(
        store
            .get(&existing.id)
            .await
            .expect("get existing")
            .is_some()
    );
}

#[tokio::test]
async fn sqlite_apply_preflights_upsert_prior_versions() {
    let store = Arc::new(memstore().await);
    let apply = SqliteFlushPlanApply::new(store.clone());
    let existing = sample_record(11);
    let inserted = sample_record(12);
    store.upsert(&existing).await.expect("seed record");

    let mut stale_record = existing.clone();
    stale_record.body = "stale update body".to_owned();
    let mut stale = sample_plan("01HQZK000000000000000000A8", FlushMode::Autonomous);
    stale.mutations = vec![
        PlannedMutation::Upsert {
            record: Box::new(inserted.clone()),
            prior_version: None,
        },
        PlannedMutation::Upsert {
            record: Box::new(stale_record),
            prior_version: Some(2),
        },
    ];

    apply
        .apply("consolidate", stale)
        .await
        .expect_err("stale upsert should fail before any mutation");

    assert!(
        store
            .get(&inserted.id)
            .await
            .expect("get inserted")
            .is_none()
    );
    let active = store
        .get_active_by_target(&existing.target_id)
        .await
        .expect("active")
        .expect("existing remains active");
    assert_eq!(active.version, 1);
    assert_eq!(active.record.body, existing.body);
}

#[tokio::test]
async fn sqlite_apply_rejects_new_upsert_when_target_already_exists() {
    let store = Arc::new(memstore().await);
    let apply = SqliteFlushPlanApply::new(store.clone());
    let existing = sample_record(13);
    store.upsert(&existing).await.expect("seed record");

    let mut replacement = existing.clone();
    replacement.body = "unexpected replacement".to_owned();
    let mut plan = sample_plan("01HQZK000000000000000000A9", FlushMode::Autonomous);
    plan.mutations = vec![PlannedMutation::Upsert {
        record: Box::new(replacement),
        prior_version: None,
    }];

    apply
        .apply("consolidate", plan)
        .await
        .expect_err("new-record upsert should not overwrite an active target");

    let active = store
        .get_active_by_target(&existing.target_id)
        .await
        .expect("active")
        .expect("existing remains active");
    assert_eq!(active.version, 1);
    assert_eq!(active.record.body, existing.body);
}

#[tokio::test]
async fn sqlite_apply_rejects_duplicate_target_mutations_before_apply() {
    let store = Arc::new(memstore().await);
    let apply = SqliteFlushPlanApply::new(store.clone());
    let first = sample_record(14);
    let mut second = first.clone();
    second.body = "second body for same target".to_owned();
    second.id = sample_record(15).id;

    let mut plan = sample_plan("01HQZK000000000000000000B1", FlushMode::Autonomous);
    plan.mutations = vec![
        PlannedMutation::Upsert {
            record: Box::new(first.clone()),
            prior_version: None,
        },
        PlannedMutation::Upsert {
            record: Box::new(second),
            prior_version: None,
        },
    ];

    let err = apply
        .apply("consolidate", plan)
        .await
        .expect_err("duplicate target should fail before first upsert");

    assert!(err.to_string().contains("duplicate mutation target"));
    assert!(store.get(&first.id).await.expect("get").is_none());
}

#[tokio::test]
async fn sqlite_apply_preserves_upsert_store_errors_as_apply_errors() {
    let store = Arc::new(memstore().await);
    let apply = SqliteFlushPlanApply::new(store);
    let mut invalid = sample_record(16);
    invalid.body.clear();

    let mut plan = sample_plan("01HQZK000000000000000000B2", FlushMode::Autonomous);
    plan.mutations = vec![PlannedMutation::Upsert {
        record: Box::new(invalid),
        prior_version: None,
    }];

    let err = apply
        .apply("consolidate", plan)
        .await
        .expect_err("invalid record should surface store error");

    assert!(matches!(err, WorkflowError::Apply { .. }));
    assert!(err.to_string().contains("invalid record"));
}

#[tokio::test]
async fn sqlite_apply_reports_same_body_upsert_as_already_applied() {
    let store = Arc::new(memstore().await);
    let apply = SqliteFlushPlanApply::new(store.clone());
    let record = sample_record(17);
    store.upsert(&record).await.expect("seed record");

    let mut plan = sample_plan("01HQZK000000000000000000B3", FlushMode::Autonomous);
    plan.mutations = vec![PlannedMutation::Upsert {
        record: Box::new(record),
        prior_version: Some(1),
    }];

    let outcome = apply.apply("consolidate", plan).await.expect("apply");

    assert_eq!(outcome, FlushPlanApplyOutcome::AlreadyApplied);
}

#[tokio::test]
async fn sqlite_apply_reports_missing_delete_target_as_already_applied() {
    let store = Arc::new(memstore().await);
    let apply = SqliteFlushPlanApply::new(store);
    let missing = sample_record(18).target_id;

    let mut plan = sample_plan("01HQZK000000000000000000B4", FlushMode::Autonomous);
    plan.mutations = vec![PlannedMutation::Delete {
        target: missing,
        prior_version: 1,
    }];

    let outcome = apply.apply("expire", plan).await.expect("delete");

    assert_eq!(outcome, FlushPlanApplyOutcome::AlreadyApplied);
}

#[tokio::test]
async fn sqlite_apply_reports_missing_expire_target_as_already_applied() {
    let store = Arc::new(memstore().await);
    let apply = SqliteFlushPlanApply::new(store);
    let missing = sample_record(19).target_id;

    let mut plan = sample_plan("01HQZK000000000000000000B5", FlushMode::Autonomous);
    plan.mutations = vec![PlannedMutation::Expire {
        target: missing,
        reason: ExpirationReason::SalienceBelowThreshold,
    }];

    let outcome = apply.apply("expire", plan).await.expect("expire");

    assert_eq!(outcome, FlushPlanApplyOutcome::AlreadyApplied);
}

#[tokio::test]
async fn sqlite_apply_reports_applied_when_any_mutation_changes_state() {
    let store = Arc::new(memstore().await);
    let apply = SqliteFlushPlanApply::new(store.clone());
    let existing = sample_record(20);
    let inserted = sample_record(21);
    store.upsert(&existing).await.expect("seed record");

    let mut plan = sample_plan("01HQZK000000000000000000B6", FlushMode::Autonomous);
    plan.mutations = vec![
        PlannedMutation::Upsert {
            record: Box::new(existing),
            prior_version: Some(1),
        },
        PlannedMutation::Upsert {
            record: Box::new(inserted.clone()),
            prior_version: None,
        },
    ];

    let outcome = apply.apply("consolidate", plan).await.expect("apply");

    assert_eq!(outcome, FlushPlanApplyOutcome::Applied);
    assert!(store.get(&inserted.id).await.expect("get").is_some());
}

#[tokio::test]
async fn sqlite_apply_promotes_active_record_kind_idempotently() {
    let store = Arc::new(memstore().await);
    let apply = SqliteFlushPlanApply::new(store.clone());
    let mut record = sample_record(20);
    record.kind = MemoryKind::Reference;
    store.upsert(&record).await.expect("seed record");

    let mut plan = sample_plan("01HQZK000000000000000000H1", FlushMode::Autonomous);
    plan.mutations = vec![PlannedMutation::Promote {
        from: record.target_id.clone(),
        to_kind: MemoryKind::Fact,
        evidence: vec![],
    }];

    let first = apply
        .apply("promote", plan.clone())
        .await
        .expect("first promote");

    assert_eq!(first, FlushPlanApplyOutcome::Applied);
    let promoted = store
        .get_active_by_target(&record.target_id)
        .await
        .expect("active")
        .expect("record remains active");
    assert_eq!(promoted.record.kind, MemoryKind::Fact);

    let second = apply.apply("promote", plan).await.expect("second promote");

    assert_eq!(second, FlushPlanApplyOutcome::AlreadyApplied);
}
