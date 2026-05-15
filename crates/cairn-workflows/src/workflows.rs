//! Concrete workflow class wrappers.
//!
//! The planning logic for consolidate/promote/expire is intentionally injected
//! through [`WorkflowPlanSource`]. That keeps these workflow types pure
//! `FlushPlan` producers while the store-specific candidate selection lands in
//! narrower follow-up modules.

use std::sync::Arc;

use cairn_core::domain::FlushPlan;

use crate::drainer::{Workflow, WorkflowContext, WorkflowError};

/// Planner backing a concrete workflow class.
#[async_trait::async_trait]
pub trait WorkflowPlanSource: Send + Sync {
    /// Produce plans for `workflow`.
    async fn plan(
        &self,
        workflow: &'static str,
        ctx: &WorkflowContext,
    ) -> Result<Vec<FlushPlan>, WorkflowError>;
}

macro_rules! workflow_type {
    ($ty:ident, $name:literal, $doc:literal) => {
        #[doc = $doc]
        pub struct $ty {
            source: Arc<dyn WorkflowPlanSource>,
        }

        impl $ty {
            /// Create a workflow backed by `source`.
            #[must_use]
            pub fn new(source: Arc<dyn WorkflowPlanSource>) -> Self {
                Self { source }
            }
        }

        #[async_trait::async_trait]
        impl Workflow for $ty {
            fn name(&self) -> &'static str {
                $name
            }

            async fn plan(&self, ctx: &WorkflowContext) -> Result<Vec<FlushPlan>, WorkflowError> {
                self.source.plan($name, ctx).await
            }
        }
    };
}

workflow_type!(
    ConsolidateWorkflow,
    "consolidate",
    "Consolidation workflow class: emits `FlushPlan` batches for canonicalization and summary updates."
);
workflow_type!(
    PromoteWorkflow,
    "promote",
    "Promotion workflow class: emits `FlushPlan` batches for tier-boundary promotion candidates."
);
workflow_type!(
    ExpireWorkflow,
    "expire",
    "Expiration workflow class: emits `FlushPlan` batches for TTL and salience expiration candidates."
);
