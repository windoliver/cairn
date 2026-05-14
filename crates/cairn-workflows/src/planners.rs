//! Store-backed workflow plan sources.

use std::sync::Arc;

use cairn_core::contract::memory_store::{ListArgs, ListCursor, MemoryStore};
use cairn_core::domain::flush_plan::{
    ExpirationReason, FlushMode, FlushPlan, PlanReason, PlannedMutation,
};
use cairn_core::domain::{Identity, MemoryRecord};
use cairn_core::generated::common::Ulid;
use chrono::{Duration, SecondsFormat};

use crate::drainer::{WorkflowContext, WorkflowError};
use crate::workflows::WorkflowPlanSource;

/// Store-backed expiration planner.
pub struct ExpirePlanSource {
    store: Arc<dyn MemoryStore>,
    issuer: Identity,
    salience_threshold: f32,
    page_limit: usize,
}

impl ExpirePlanSource {
    const DEFAULT_PAGE_LIMIT: usize = 1000;

    /// Create an expiration planner that expires active records whose salience
    /// is less than or equal to `salience_threshold`.
    #[must_use]
    pub fn new(store: Arc<dyn MemoryStore>, issuer: Identity, salience_threshold: f32) -> Self {
        Self {
            store,
            issuer,
            salience_threshold,
            page_limit: Self::DEFAULT_PAGE_LIMIT,
        }
    }

    /// Override the planner's store page size.
    #[must_use]
    pub fn with_page_limit(mut self, page_limit: usize) -> Self {
        self.page_limit = page_limit.max(1);
        self
    }
}

#[async_trait::async_trait]
impl WorkflowPlanSource for ExpirePlanSource {
    async fn plan(
        &self,
        workflow: &'static str,
        _ctx: &WorkflowContext,
    ) -> Result<Vec<FlushPlan>, WorkflowError> {
        if workflow != "expire" {
            return Ok(Vec::new());
        }

        let mut plans = Vec::new();
        let mut cursor: Option<ListCursor> = None;
        loop {
            let page = self
                .store
                .list(&ListArgs {
                    limit: self.page_limit,
                    cursor: cursor.clone(),
                    ..ListArgs::default()
                })
                .await
                .map_err(|e| WorkflowError::Internal {
                    workflow,
                    message: format!("expiration planner list failed: {e}"),
                })?;

            plans.extend(
                page.records
                    .iter()
                    .filter(|record| record.salience <= self.salience_threshold)
                    .map(|record| self.plan_for_record(record)),
            );

            let Some(next_cursor) = page.next_cursor else {
                break;
            };
            cursor = Some(next_cursor);
        }

        Ok(plans)
    }
}

impl ExpirePlanSource {
    fn plan_for_record(&self, record: &MemoryRecord) -> FlushPlan {
        let issued_at = now_rfc3339();
        let expires_at = expires_at_rfc3339();
        FlushPlan {
            operation_id: new_ulid(),
            issued_at,
            issuer: self.issuer.clone(),
            principal: None,
            scope: record.scope.clone(),
            mode: FlushMode::Autonomous,
            mutations: vec![PlannedMutation::Expire {
                target: record.target_id.clone(),
                reason: ExpirationReason::SalienceBelowThreshold,
            }],
            reason: PlanReason::Expire {
                ttl_expired: false,
                salience_below: Some(self.salience_threshold),
            },
            source_events: Vec::new(),
            target_hashes: std::collections::BTreeMap::new(),
            dependencies: Vec::new(),
            expires_at,
            placeholder: false,
        }
    }
}

fn new_ulid() -> Ulid {
    Ulid(ulid::Ulid::new().to_string())
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn expires_at_rfc3339() -> String {
    (chrono::Utc::now() + Duration::minutes(5)).to_rfc3339_opts(SecondsFormat::Secs, true)
}
