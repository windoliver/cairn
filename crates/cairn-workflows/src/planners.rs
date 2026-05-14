//! Store-backed workflow plan sources.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::Arc;

use cairn_core::contract::memory_store::{ListArgs, ListCursor, MemoryStore};
use cairn_core::domain::flush_plan::{
    ExpirationReason, FlushMode, FlushPlan, PlanReason, PlannedMutation,
};
use cairn_core::domain::{Identity, MemoryKind, MemoryRecord};
use cairn_core::generated::common::Ulid;
use chrono::{Duration, SecondsFormat};
use sha2::{Digest, Sha256};

use crate::drainer::{WorkflowContext, WorkflowError};
use crate::workflows::WorkflowPlanSource;

/// Store-backed expiration planner.
pub struct ExpirePlanSource {
    store: Arc<dyn MemoryStore>,
    issuer: Identity,
    salience_threshold: f32,
    page_limit: usize,
}

/// Store-backed promotion planner.
pub struct PromotePlanSource {
    store: Arc<dyn MemoryStore>,
    issuer: Identity,
    to_kind: MemoryKind,
    confidence_threshold: f32,
    page_limit: usize,
}

impl PromotePlanSource {
    const DEFAULT_PAGE_LIMIT: usize = 1000;

    /// Create a promotion planner for active records with confidence greater
    /// than or equal to `confidence_threshold`.
    #[must_use]
    pub fn new(
        store: Arc<dyn MemoryStore>,
        issuer: Identity,
        to_kind: MemoryKind,
        confidence_threshold: f32,
    ) -> Self {
        Self {
            store,
            issuer,
            to_kind,
            confidence_threshold,
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

/// Store-backed consolidation planner.
pub struct ConsolidatePlanSource {
    store: Arc<dyn MemoryStore>,
    issuer: Identity,
    page_limit: usize,
}

impl ConsolidatePlanSource {
    const DEFAULT_PAGE_LIMIT: usize = 1000;

    /// Create a consolidation planner that expires exact duplicate active
    /// bodies, keeping the highest-confidence/highest-salience candidate.
    #[must_use]
    pub fn new(store: Arc<dyn MemoryStore>, issuer: Identity) -> Self {
        Self {
            store,
            issuer,
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

#[async_trait::async_trait]
impl WorkflowPlanSource for PromotePlanSource {
    async fn plan(
        &self,
        workflow: &'static str,
        _ctx: &WorkflowContext,
    ) -> Result<Vec<FlushPlan>, WorkflowError> {
        if workflow != "promote" {
            return Ok(Vec::new());
        }

        let mut plans = Vec::new();
        for record in list_active_records(&self.store, workflow, self.page_limit).await? {
            if record.kind == self.to_kind || record.confidence < self.confidence_threshold {
                continue;
            }
            plans.push(self.plan_for_record(&record));
        }
        Ok(plans)
    }
}

#[async_trait::async_trait]
impl WorkflowPlanSource for ConsolidatePlanSource {
    async fn plan(
        &self,
        workflow: &'static str,
        _ctx: &WorkflowContext,
    ) -> Result<Vec<FlushPlan>, WorkflowError> {
        if workflow != "consolidate" {
            return Ok(Vec::new());
        }

        let mut by_body: BTreeMap<String, Vec<MemoryRecord>> = BTreeMap::new();
        for record in list_active_records(&self.store, workflow, self.page_limit).await? {
            by_body.entry(record.body.clone()).or_default().push(record);
        }

        let mut plans = Vec::new();
        for mut records in by_body.into_values() {
            if records.len() < 2 {
                continue;
            }
            records.sort_by(compare_consolidation_candidate);
            let keeper = records.remove(0);
            for duplicate in records {
                plans.push(self.plan_for_duplicate(&duplicate, &keeper));
            }
        }
        Ok(plans)
    }
}

impl ExpirePlanSource {
    fn plan_for_record(&self, record: &MemoryRecord) -> FlushPlan {
        let issued_at = now_rfc3339();
        let expires_at = expires_at_rfc3339();
        FlushPlan {
            operation_id: stable_plan_ulid(&[
                "expire",
                record.target_id.as_str(),
                record.id.as_str(),
                &record.salience.to_bits().to_string(),
                &self.salience_threshold.to_bits().to_string(),
            ]),
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
                salience_below: Some(record.salience),
            },
            source_events: Vec::new(),
            target_hashes: std::collections::BTreeMap::new(),
            dependencies: Vec::new(),
            expires_at,
            placeholder: false,
        }
    }
}

impl PromotePlanSource {
    fn plan_for_record(&self, record: &MemoryRecord) -> FlushPlan {
        let issued_at = now_rfc3339();
        let expires_at = expires_at_rfc3339();
        FlushPlan {
            operation_id: stable_plan_ulid(&[
                "promote",
                record.target_id.as_str(),
                record.id.as_str(),
                record.kind.as_str(),
                self.to_kind.as_str(),
                &record.confidence.to_bits().to_string(),
                &self.confidence_threshold.to_bits().to_string(),
            ]),
            issued_at,
            issuer: self.issuer.clone(),
            principal: None,
            scope: record.scope.clone(),
            mode: FlushMode::Autonomous,
            mutations: vec![PlannedMutation::Promote {
                from: record.target_id.clone(),
                to_kind: self.to_kind,
                evidence: Vec::new(),
            }],
            reason: PlanReason::Promote {
                confidence: record.confidence,
                evidence_count: 0,
            },
            source_events: Vec::new(),
            target_hashes: std::collections::BTreeMap::new(),
            dependencies: Vec::new(),
            expires_at,
            placeholder: false,
        }
    }
}

impl ConsolidatePlanSource {
    fn plan_for_duplicate(&self, duplicate: &MemoryRecord, keeper: &MemoryRecord) -> FlushPlan {
        let issued_at = now_rfc3339();
        let expires_at = expires_at_rfc3339();
        FlushPlan {
            operation_id: stable_plan_ulid(&[
                "consolidate",
                duplicate.target_id.as_str(),
                duplicate.id.as_str(),
                keeper.target_id.as_str(),
                keeper.id.as_str(),
                &duplicate.confidence.to_bits().to_string(),
                &duplicate.salience.to_bits().to_string(),
            ]),
            issued_at,
            issuer: self.issuer.clone(),
            principal: None,
            scope: duplicate.scope.clone(),
            mode: FlushMode::Autonomous,
            mutations: vec![PlannedMutation::Expire {
                target: duplicate.target_id.clone(),
                reason: ExpirationReason::SupersededByCanonical,
            }],
            reason: PlanReason::Expire {
                ttl_expired: false,
                salience_below: None,
            },
            source_events: Vec::new(),
            target_hashes: std::collections::BTreeMap::new(),
            dependencies: Vec::new(),
            expires_at,
            placeholder: false,
        }
    }
}

async fn list_active_records(
    store: &Arc<dyn MemoryStore>,
    workflow: &'static str,
    page_limit: usize,
) -> Result<Vec<MemoryRecord>, WorkflowError> {
    let mut records = Vec::new();
    let mut cursor: Option<ListCursor> = None;
    loop {
        let page = store
            .list(&ListArgs {
                limit: page_limit,
                cursor: cursor.clone(),
                ..ListArgs::default()
            })
            .await
            .map_err(|e| WorkflowError::Internal {
                workflow,
                message: format!("{workflow} planner list failed: {e}"),
            })?;

        records.extend(page.records);

        let Some(next_cursor) = page.next_cursor else {
            break;
        };
        cursor = Some(next_cursor);
    }
    Ok(records)
}

fn compare_consolidation_candidate(left: &MemoryRecord, right: &MemoryRecord) -> Ordering {
    right
        .confidence
        .total_cmp(&left.confidence)
        .then_with(|| right.salience.total_cmp(&left.salience))
        .then_with(|| left.target_id.as_str().cmp(right.target_id.as_str()))
}

fn stable_plan_ulid(parts: &[&str]) -> Ulid {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    Ulid(ulid::Ulid::from_bytes(bytes).to_string())
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn expires_at_rfc3339() -> String {
    (chrono::Utc::now() + Duration::minutes(5)).to_rfc3339_opts(SecondsFormat::Secs, true)
}
