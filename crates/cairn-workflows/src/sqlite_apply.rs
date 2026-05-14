//! SQLite-backed [`FlushPlanApply`] implementation.

use std::collections::BTreeSet;
use std::sync::Arc;

use cairn_core::contract::memory_store::MemoryStore as _;
use cairn_core::domain::flush_plan::{FlushPlan, PlannedMutation};
use cairn_core::domain::{MemoryRecord, TargetId};
use cairn_store_sqlite::{SqliteMemoryStore, StoreError as SqliteStoreError};

use crate::drainer::{FlushPlanApply, FlushPlanApplyOutcome, WorkflowError};

/// Apply workflow-produced [`FlushPlan`]s through the `SQLite` store.
pub struct SqliteFlushPlanApply {
    store: Arc<SqliteMemoryStore>,
}

impl SqliteFlushPlanApply {
    /// Create an apply adapter backed by `store`.
    #[must_use]
    pub fn new(store: Arc<SqliteMemoryStore>) -> Self {
        Self { store }
    }
}

#[async_trait::async_trait]
impl FlushPlanApply for SqliteFlushPlanApply {
    async fn apply(
        &self,
        workflow: &'static str,
        plan: FlushPlan,
    ) -> Result<FlushPlanApplyOutcome, WorkflowError> {
        if plan.mutations.is_empty() {
            return Ok(FlushPlanApplyOutcome::AlreadyApplied);
        }
        preflight(&self.store, workflow, &plan).await?;

        let mut applied_any = false;
        for mutation in &plan.mutations {
            match apply_one(&self.store, workflow, &plan, mutation).await? {
                FlushPlanApplyOutcome::Applied => applied_any = true,
                FlushPlanApplyOutcome::AlreadyApplied => {}
            }
        }
        if applied_any {
            Ok(FlushPlanApplyOutcome::Applied)
        } else {
            Ok(FlushPlanApplyOutcome::AlreadyApplied)
        }
    }
}

async fn preflight(
    store: &SqliteMemoryStore,
    workflow: &'static str,
    plan: &FlushPlan,
) -> Result<(), WorkflowError> {
    let mut seen_targets = BTreeSet::new();
    for mutation in &plan.mutations {
        if !is_supported(mutation) {
            return Err(unsupported_error(workflow, mutation));
        }
        if let Some(target) = mutation_target(mutation)
            && !seen_targets.insert(target.to_owned())
        {
            return Err(duplicate_target_error(workflow, target));
        }
        match mutation {
            PlannedMutation::Upsert {
                record,
                prior_version,
            } => {
                ensure_upsert_version(store, workflow, plan, record, *prior_version).await?;
            }
            PlannedMutation::Delete {
                target,
                prior_version,
            } => {
                ensure_delete_version(store, workflow, plan, target, *prior_version).await?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn mutation_target(mutation: &PlannedMutation) -> Option<&str> {
    match mutation {
        PlannedMutation::Upsert { record, .. } => Some(record.target_id.as_str()),
        PlannedMutation::Delete { target, .. }
        | PlannedMutation::Expire { target, .. }
        | PlannedMutation::ForgetRecord { target } => Some(target.as_str()),
        _ => None,
    }
}

async fn apply_one(
    store: &SqliteMemoryStore,
    workflow: &'static str,
    plan: &FlushPlan,
    mutation: &PlannedMutation,
) -> Result<FlushPlanApplyOutcome, WorkflowError> {
    match mutation {
        PlannedMutation::Upsert {
            record,
            prior_version,
        } => {
            ensure_upsert_version(store, workflow, plan, record, *prior_version).await?;
            store
                .upsert(record)
                .await
                .map(|outcome| {
                    if outcome.content_changed {
                        FlushPlanApplyOutcome::Applied
                    } else {
                        FlushPlanApplyOutcome::AlreadyApplied
                    }
                })
                .map_err(|source| apply_boxed_error(workflow, plan, source))
        }
        PlannedMutation::Expire { target, .. } => {
            expire_active_target(store, workflow, plan, target).await
        }
        PlannedMutation::ForgetRecord { target } => {
            forget_active_target(store, workflow, plan, target).await
        }
        PlannedMutation::Delete {
            target,
            prior_version,
        } => delete_active_target(store, workflow, plan, target, *prior_version).await,
        other => Err(unsupported_error(workflow, other)),
    }
}

fn is_supported(mutation: &PlannedMutation) -> bool {
    matches!(
        mutation,
        PlannedMutation::Upsert { .. }
            | PlannedMutation::Expire { .. }
            | PlannedMutation::ForgetRecord { .. }
            | PlannedMutation::Delete { .. }
    )
}

fn unsupported_error(workflow: &'static str, mutation: &PlannedMutation) -> WorkflowError {
    WorkflowError::Internal {
        workflow,
        message: format!(
            "unsupported mutation kind `{}` in workflow FlushPlan apply",
            mutation_kind(mutation),
        ),
    }
}

fn duplicate_target_error(workflow: &'static str, target: &str) -> WorkflowError {
    WorkflowError::Internal {
        workflow,
        message: format!("duplicate mutation target `{target}` in workflow FlushPlan apply"),
    }
}

async fn forget_active_target(
    store: &SqliteMemoryStore,
    workflow: &'static str,
    plan: &FlushPlan,
    target: &TargetId,
) -> Result<FlushPlanApplyOutcome, WorkflowError> {
    let Some(active) = store
        .get_active_by_target(target)
        .await
        .map_err(|source| apply_boxed_error(workflow, plan, source))?
    else {
        return Ok(FlushPlanApplyOutcome::AlreadyApplied);
    };
    store
        .forget_record(&active.record.id)
        .await
        .map(|_| FlushPlanApplyOutcome::Applied)
        .map_err(|e| WorkflowError::apply(workflow, plan, e))
}

async fn delete_active_target(
    store: &SqliteMemoryStore,
    workflow: &'static str,
    plan: &FlushPlan,
    target: &TargetId,
    prior_version: u32,
) -> Result<FlushPlanApplyOutcome, WorkflowError> {
    let Some(active) = store
        .get_active_by_target(target)
        .await
        .map_err(|source| apply_boxed_error(workflow, plan, source))?
    else {
        return Ok(FlushPlanApplyOutcome::AlreadyApplied);
    };
    check_delete_version(workflow, plan, target, prior_version, active.version)?;
    store
        .forget_record(&active.record.id)
        .await
        .map(|_| FlushPlanApplyOutcome::Applied)
        .map_err(|e| WorkflowError::apply(workflow, plan, e))
}

async fn expire_active_target(
    store: &SqliteMemoryStore,
    workflow: &'static str,
    plan: &FlushPlan,
    target: &TargetId,
) -> Result<FlushPlanApplyOutcome, WorkflowError> {
    if store
        .get_active_by_target(target)
        .await
        .map_err(|source| apply_boxed_error(workflow, plan, source))?
        .is_none()
    {
        return Ok(FlushPlanApplyOutcome::AlreadyApplied);
    }
    store
        .expire(target)
        .await
        .map(|()| FlushPlanApplyOutcome::Applied)
        .map_err(|e| WorkflowError::apply(workflow, plan, e))
}

async fn ensure_delete_version(
    store: &SqliteMemoryStore,
    workflow: &'static str,
    plan: &FlushPlan,
    target: &TargetId,
    prior_version: u32,
) -> Result<(), WorkflowError> {
    let Some(active) = store
        .get_active_by_target(target)
        .await
        .map_err(|source| apply_boxed_error(workflow, plan, source))?
    else {
        return Ok(());
    };
    check_delete_version(workflow, plan, target, prior_version, active.version)
}

async fn ensure_upsert_version(
    store: &SqliteMemoryStore,
    workflow: &'static str,
    plan: &FlushPlan,
    record: &MemoryRecord,
    prior_version: Option<u32>,
) -> Result<(), WorkflowError> {
    let active = store
        .get_active_by_target(&record.target_id)
        .await
        .map_err(|source| apply_boxed_error(workflow, plan, source))?;
    match (active, prior_version) {
        (None, None) => Ok(()),
        (Some(active), Some(prior_version)) => check_upsert_version(
            workflow,
            plan,
            &record.target_id,
            prior_version,
            active.version,
        ),
        (Some(active), None) => Err(WorkflowError::apply(
            workflow,
            plan,
            SqliteStoreError::Invariant {
                what: format!(
                    "upsert expected new target `{}` but live version {} exists",
                    record.target_id.as_str(),
                    active.version,
                ),
            },
        )),
        (None, Some(prior_version)) => Err(WorkflowError::apply(
            workflow,
            plan,
            SqliteStoreError::Invariant {
                what: format!(
                    "upsert prior_version mismatch for missing target `{}`: plan={}",
                    record.target_id.as_str(),
                    prior_version,
                ),
            },
        )),
    }
}

fn check_delete_version(
    workflow: &'static str,
    plan: &FlushPlan,
    target: &TargetId,
    prior_version: u32,
    active_version: u32,
) -> Result<(), WorkflowError> {
    if active_version == prior_version {
        return Ok(());
    }
    Err(WorkflowError::apply(
        workflow,
        plan,
        SqliteStoreError::Invariant {
            what: format!(
                "delete prior_version mismatch for target `{}`: plan={}, live={}",
                target.as_str(),
                prior_version,
                active_version,
            ),
        },
    ))
}

fn check_upsert_version(
    workflow: &'static str,
    plan: &FlushPlan,
    target: &TargetId,
    prior_version: u32,
    active_version: u32,
) -> Result<(), WorkflowError> {
    if active_version == prior_version {
        return Ok(());
    }
    Err(WorkflowError::apply(
        workflow,
        plan,
        SqliteStoreError::Invariant {
            what: format!(
                "upsert prior_version mismatch for target `{}`: plan={}, live={}",
                target.as_str(),
                prior_version,
                active_version,
            ),
        },
    ))
}

fn apply_boxed_error(
    workflow: &'static str,
    plan: &FlushPlan,
    source: Box<dyn std::error::Error + Send + Sync>,
) -> WorkflowError {
    WorkflowError::Apply {
        workflow,
        operation_id: plan.operation_id.0.clone(),
        source,
    }
}

fn mutation_kind(mutation: &PlannedMutation) -> &'static str {
    match mutation {
        PlannedMutation::Upsert { .. } => "upsert",
        PlannedMutation::Delete { .. } => "delete",
        PlannedMutation::Patch { .. } => "patch",
        PlannedMutation::Rename { .. } => "rename",
        PlannedMutation::Promote { .. } => "promote",
        PlannedMutation::Expire { .. } => "expire",
        PlannedMutation::ForgetSession { .. } => "forget_session",
        PlannedMutation::ForgetRecord { .. } => "forget_record",
        PlannedMutation::Evolve { .. } => "evolve",
        _ => "unknown",
    }
}
