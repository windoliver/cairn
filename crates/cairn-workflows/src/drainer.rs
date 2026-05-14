//! FlushPlan-producing workflow boundary and shared drainer.
//!
//! Issue #291 starts by making background workflows pure plan producers.
//! This module owns the common apply loop so cancellation, idempotency, and
//! telemetry live in one place before concrete workflow planners are wired.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use cairn_core::domain::FlushPlan;
use cairn_core::generated::status::{StatusResponseWorkflows, StatusResponseWorkflowsWorkflow};
use chrono::SecondsFormat;
use tokio_util::sync::CancellationToken;
use tracing::Instrument as _;

/// Shared inputs available to workflow planners and the drainer.
#[derive(Debug, Clone)]
pub struct WorkflowContext {
    /// Cooperative cancellation token owned by the drainer.
    pub cancel: CancellationToken,
}

impl Default for WorkflowContext {
    fn default() -> Self {
        Self {
            cancel: CancellationToken::new(),
        }
    }
}

/// Errors raised while planning or applying workflow-produced flush plans.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WorkflowError {
    /// The workflow failed before producing a complete plan batch.
    #[error("workflow {workflow} planning failed: {source}")]
    Planning {
        /// Workflow name.
        workflow: &'static str,
        /// Error source.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// Applying one plan failed.
    #[error("workflow {workflow} apply failed for plan {operation_id}: {source}")]
    Apply {
        /// Workflow name.
        workflow: &'static str,
        /// Plan operation id.
        operation_id: String,
        /// Error source.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// Generic internal error used by adapters that do not expose a typed
    /// lower-level error.
    #[error("workflow {workflow} internal error: {message}")]
    Internal {
        /// Workflow name.
        workflow: &'static str,
        /// Human-readable error detail.
        message: String,
    },
}

impl WorkflowError {
    /// Build an apply error while preserving the lower-level source.
    pub fn apply<E>(workflow: &'static str, plan: &FlushPlan, source: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::Apply {
            workflow,
            operation_id: plan.operation_id.0.clone(),
            source: Box::new(source),
        }
    }
}

/// Pure background workflow: inspect context and return plans, but do not
/// apply mutations directly.
#[async_trait::async_trait]
pub trait Workflow: Send + Sync {
    /// Stable workflow class name, e.g. `consolidate`, `promote`, or `expire`.
    fn name(&self) -> &'static str;

    /// Produce a batch of plans for this run.
    async fn plan(&self, ctx: &WorkflowContext) -> Result<Vec<FlushPlan>, WorkflowError>;
}

/// Outcome of applying a single workflow plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushPlanApplyOutcome {
    /// Mutations were applied by this invocation.
    Applied,
    /// The plan's operation id was already terminal; this is a successful
    /// idempotent no-op.
    AlreadyApplied,
}

/// Shared apply path used by [`WorkflowDrainer`].
#[async_trait::async_trait]
pub trait FlushPlanApply: Send + Sync {
    /// Apply one plan for `workflow`.
    async fn apply(
        &self,
        workflow: &'static str,
        plan: FlushPlan,
    ) -> Result<FlushPlanApplyOutcome, WorkflowError>;
}

/// Summary of one drain run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainStats {
    /// Workflow name.
    pub workflow: &'static str,
    /// Plans produced by the workflow.
    pub planned: usize,
    /// Plans applied by this run.
    pub applied: usize,
    /// Plans that were already terminal before this run tried to apply them.
    pub already_applied: usize,
    /// Whether cancellation stopped the drain before all planned plans were
    /// considered.
    pub cancelled: bool,
}

/// Status projection for `cairn.admin.v1` workflow reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowStatusSnapshot {
    /// Workflow name.
    pub workflow: &'static str,
    /// Plans drained successfully, including idempotent already-applied plans.
    pub drained_plans: usize,
    /// Plans left undrained by cancellation.
    pub pending_plans: usize,
    /// RFC 3339 timestamp for the most recent successful plan drain.
    pub last_applied_at: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct WorkflowStatusState {
    drained_plans: usize,
    pending_plans: usize,
    last_applied_at: Option<String>,
}

/// Single apply loop for workflow-produced [`FlushPlan`] batches.
pub struct WorkflowDrainer {
    ctx: WorkflowContext,
    apply: Arc<dyn FlushPlanApply>,
    status: Arc<Mutex<BTreeMap<&'static str, WorkflowStatusState>>>,
}

impl WorkflowDrainer {
    /// Create a drainer around shared context and a concrete apply adapter.
    #[must_use]
    pub fn new(ctx: WorkflowContext, apply: Arc<dyn FlushPlanApply>) -> Self {
        Self {
            ctx,
            apply,
            status: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Return byte-stable workflow status snapshots ordered by workflow name.
    #[must_use]
    pub fn status(&self) -> Vec<WorkflowStatusSnapshot> {
        let Ok(status) = self.status.lock() else {
            return Vec::new();
        };
        status
            .iter()
            .map(|(workflow, state)| WorkflowStatusSnapshot {
                workflow,
                drained_plans: state.drained_plans,
                pending_plans: state.pending_plans,
                last_applied_at: state.last_applied_at.clone(),
            })
            .collect()
    }

    /// Convert drainer snapshots into the generated `status.workflows[]`
    /// wire shape.
    #[must_use]
    pub fn status_to_wire(snapshots: &[WorkflowStatusSnapshot]) -> Vec<StatusResponseWorkflows> {
        snapshots
            .iter()
            .filter_map(|snapshot| {
                let workflow = workflow_to_wire(snapshot.workflow)?;
                Some(StatusResponseWorkflows {
                    workflow,
                    drained_plans: u64::try_from(snapshot.drained_plans).unwrap_or(u64::MAX),
                    pending_plans: u64::try_from(snapshot.pending_plans).unwrap_or(u64::MAX),
                    last_applied_at: snapshot.last_applied_at.clone(),
                })
            })
            .collect()
    }

    /// Plan and apply one workflow batch.
    ///
    /// Cancellation is checked between plans. A plan that has started applying
    /// is allowed to finish so the WAL boundary remains atomic.
    ///
    /// # Errors
    ///
    /// Returns the first planning or apply error.
    pub async fn run(&self, workflow: Arc<dyn Workflow>) -> Result<DrainStats, WorkflowError> {
        let name = workflow.name();
        if !is_known_workflow(name) {
            return Err(WorkflowError::Internal {
                workflow: name,
                message: format!("unknown workflow `{name}`"),
            });
        }
        let plans = workflow.plan(&self.ctx).await?;
        let planned = plans.len();
        let mut applied = 0;
        let mut already_applied = 0;
        let mut cancelled = self.ctx.cancel.is_cancelled();
        self.record_run_started(name, planned);

        for plan in plans {
            if self.ctx.cancel.is_cancelled() {
                cancelled = true;
                break;
            }

            match self.apply_one(name, plan).await? {
                FlushPlanApplyOutcome::Applied => applied += 1,
                FlushPlanApplyOutcome::AlreadyApplied => already_applied += 1,
            }
            self.record_plan_drained(name);
        }

        if self.ctx.cancel.is_cancelled() && applied + already_applied < planned {
            cancelled = true;
        }

        Ok(DrainStats {
            workflow: name,
            planned,
            applied,
            already_applied,
            cancelled,
        })
    }

    fn record_run_started(&self, workflow: &'static str, planned: usize) {
        let Ok(mut status) = self.status.lock() else {
            return;
        };
        let state = status.entry(workflow).or_default();
        state.pending_plans = planned;
    }

    fn record_plan_drained(&self, workflow: &'static str) {
        let Ok(mut status) = self.status.lock() else {
            return;
        };
        let state = status.entry(workflow).or_default();
        state.drained_plans = state.drained_plans.saturating_add(1);
        state.pending_plans = state.pending_plans.saturating_sub(1);
        state.last_applied_at = Some(now_rfc3339());
    }

    async fn apply_one(
        &self,
        workflow: &'static str,
        plan: FlushPlan,
    ) -> Result<FlushPlanApplyOutcome, WorkflowError> {
        let span = tracing::info_span!(
            "workflow.apply_one",
            workflow,
            plan_id = %plan.operation_id.0,
            mutation_count = plan.mutations.len(),
        );
        self.apply.apply(workflow, plan).instrument(span).await
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn is_known_workflow(workflow: &str) -> bool {
    workflow_to_wire(workflow).is_some()
}

fn workflow_to_wire(workflow: &str) -> Option<StatusResponseWorkflowsWorkflow> {
    match workflow {
        "consolidate" => Some(StatusResponseWorkflowsWorkflow::Consolidate),
        "promote" => Some(StatusResponseWorkflowsWorkflow::Promote),
        "expire" => Some(StatusResponseWorkflowsWorkflow::Expire),
        _ => None,
    }
}
