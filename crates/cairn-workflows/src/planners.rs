//! Store-backed workflow plan sources.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::Arc;

use cairn_core::contract::memory_store::{ListArgs, ListCursor, MemoryStore};
use cairn_core::domain::flush_plan::{
    ExpirationReason, FlushMode, FlushPlan, PlanReason, PlannedMutation,
};
use cairn_core::domain::record::{Ed25519Signature, RecordId};
use cairn_core::domain::taxonomy::{MemoryKind, MemoryVisibility};
use cairn_core::domain::{
    ActorChainEntry, ChainRole, EvidenceVector, Identity, MemoryRecord, Provenance,
    Rfc3339Timestamp, SourceId, TargetId,
};
use cairn_core::generated::common::Ulid;
use cairn_core::pipeline::reflection::{
    ReflectionCandidate, ReflectionConfig, ReflectionDisposition, ReflectionPolicy,
    ReflectionSignal, ReflectionSignalKind, extract_reflection_candidates,
};
use chrono::{Duration, SecondsFormat};
use sha2::{Digest, Sha256};

use crate::drainer::{WorkflowContext, WorkflowError};
use crate::synthetic::sha256_hex;
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

/// Store-backed reflection planner.
pub struct ReflectionPlanSource {
    store: Arc<dyn MemoryStore>,
    issuer: Identity,
    config: ReflectionConfig,
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

impl ReflectionPlanSource {
    const DEFAULT_PAGE_LIMIT: usize = 1000;

    /// Create a reflection planner with default gates.
    #[must_use]
    pub fn new(store: Arc<dyn MemoryStore>, issuer: Identity) -> Self {
        Self {
            store,
            issuer,
            config: ReflectionConfig::default(),
            page_limit: Self::DEFAULT_PAGE_LIMIT,
        }
    }

    /// Override extraction gates.
    #[must_use]
    pub fn with_config(mut self, config: ReflectionConfig) -> Self {
        self.config = config;
        self
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

#[async_trait::async_trait]
impl WorkflowPlanSource for ReflectionPlanSource {
    async fn plan(
        &self,
        workflow: &'static str,
        _ctx: &WorkflowContext,
    ) -> Result<Vec<FlushPlan>, WorkflowError> {
        if workflow != "reflect" {
            return Ok(Vec::new());
        }

        let records = list_active_records(&self.store, workflow, self.page_limit).await?;
        let signals: Vec<_> = records.iter().filter_map(record_to_signal).collect();
        let outcome = extract_reflection_candidates(&signals, self.config);

        outcome
            .candidates
            .iter()
            .map(|candidate| self.plan_for_candidate(candidate))
            .collect()
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

impl ReflectionPlanSource {
    fn plan_for_candidate(
        &self,
        candidate: &ReflectionCandidate,
    ) -> Result<FlushPlan, WorkflowError> {
        let issued_at = now_rfc3339();
        let expires_at = expires_at_rfc3339();
        let record = self.build_candidate_record(candidate, &issued_at)?;
        let evidence_count = u32::try_from(candidate.evidence_record_ids.len()).unwrap_or(u32::MAX);
        Ok(FlushPlan {
            operation_id: stable_plan_ulid(&[
                "reflect",
                record.target_id.as_str(),
                candidate.kind.as_str(),
                &evidence_count.to_string(),
                &candidate.confidence.to_bits().to_string(),
            ]),
            issued_at,
            issuer: self.issuer.clone(),
            principal: None,
            scope: record.scope.clone(),
            mode: match candidate.disposition {
                ReflectionDisposition::ReadyForFlush => FlushMode::Autonomous,
                ReflectionDisposition::ReviewRequired => FlushMode::HumanReview,
            },
            mutations: vec![PlannedMutation::Upsert {
                record: Box::new(record),
                prior_version: None,
            }],
            reason: PlanReason::Reflect {
                candidate_kind: candidate.kind,
                evidence_count,
            },
            source_events: candidate
                .evidence_record_ids
                .iter()
                .map(|id| Ulid(id.as_str().to_owned()))
                .collect(),
            target_hashes: BTreeMap::new(),
            dependencies: Vec::new(),
            expires_at,
            placeholder: false,
        })
    }

    fn build_candidate_record(
        &self,
        candidate: &ReflectionCandidate,
        issued_at: &str,
    ) -> Result<MemoryRecord, WorkflowError> {
        let evidence_key = candidate
            .evidence_record_ids
            .iter()
            .map(RecordId::as_str)
            .collect::<Vec<_>>()
            .join(",");
        let target_id = stable_target_id(&[
            "reflection",
            candidate.kind.as_str(),
            candidate.class.as_str(),
            &candidate.body,
            &evidence_key,
        ])
        .map_err(|e| WorkflowError::Internal {
            workflow: "reflect",
            message: format!("reflection target id: {e}"),
        })?;
        let record_id = RecordId::parse(target_id.as_str().to_owned()).map_err(|e| {
            WorkflowError::Internal {
                workflow: "reflect",
                message: format!("reflection record id: {e}"),
            }
        })?;
        let now =
            Rfc3339Timestamp::parse(issued_at.to_owned()).map_err(|e| WorkflowError::Internal {
                workflow: "reflect",
                message: format!("reflection timestamp: {e}"),
            })?;
        let source_sensor = Identity::parse("snr:cairn-workflows:reflection:v1").map_err(|e| {
            WorkflowError::Internal {
                workflow: "reflect",
                message: format!("reflection sensor identity: {e}"),
            }
        })?;
        let self_source = SourceId::parse(target_id.as_str().to_owned()).map_err(|e| {
            WorkflowError::Internal {
                workflow: "reflect",
                message: format!("reflection source id: {e}"),
            }
        })?;
        let evidence_ids: Vec<_> = candidate
            .evidence_record_ids
            .iter()
            .map(|id| serde_json::Value::String(id.as_str().to_owned()))
            .collect();
        let mut extra_frontmatter = BTreeMap::new();
        extra_frontmatter.insert(
            "reflection".to_owned(),
            serde_json::json!({
                "evidence_record_ids": evidence_ids,
                "candidate_disposition": match candidate.disposition {
                    ReflectionDisposition::ReadyForFlush => "ready_for_flush",
                    ReflectionDisposition::ReviewRequired => "review_required",
                },
                "produced_by": "cairn-workflows::ReflectionPlanSource",
            }),
        );

        Ok(MemoryRecord {
            id: record_id,
            target_id,
            kind: candidate.kind,
            class: candidate.class,
            visibility: MemoryVisibility::Private,
            scope: candidate.scope.clone(),
            body: candidate.body.clone(),
            source_ids: Vec::new(),
            provenance: Provenance {
                source_sensor,
                created_at: now.clone(),
                originating_agent_id: self.issuer.clone(),
                source_hash: format!("sha256:{}", sha256_hex(candidate.body.as_bytes())),
                consent_ref: "consent:system:reflection-workflow".to_owned(),
                llm_id_if_any: None,
                source_ids: vec![self_source],
                source_refs: Vec::new(),
            },
            updated_at: now.clone(),
            evidence: EvidenceVector {
                recall_count: evidence_count_for(candidate),
                score: candidate.confidence,
                unique_queries: 1,
                recency_half_life_days: 14,
            },
            salience: candidate.salience,
            confidence: candidate.confidence,
            actor_chain: vec![ActorChainEntry {
                role: ChainRole::Author,
                identity: self.issuer.clone(),
                at: now,
            }],
            signature: Ed25519Signature::flush_mutated_sentinel(),
            tags: vec!["reflection_candidate".to_owned()],
            extra_frontmatter,
            consent_model: None,
        })
    }
}

fn record_to_signal(record: &MemoryRecord) -> Option<ReflectionSignal> {
    let kind = match record.kind {
        MemoryKind::Feedback => ReflectionSignalKind::UserCorrection,
        MemoryKind::KnowledgeGap => ReflectionSignalKind::KnowledgeGap,
        MemoryKind::Entity => ReflectionSignalKind::NovelEntity,
        MemoryKind::Trace if looks_like_tool_error(&record.body) => ReflectionSignalKind::ToolError,
        _ => return None,
    };
    Some(ReflectionSignal {
        record_id: record.id.clone(),
        kind,
        body: record.body.clone(),
        scope: record.scope.clone(),
        salience: record.salience,
        confidence: record.confidence,
        policy: if record.signature_attests_author() {
            ReflectionPolicy::Allowed
        } else {
            ReflectionPolicy::Rejected
        },
    })
}

fn looks_like_tool_error(body: &str) -> bool {
    let lowered = body.to_ascii_lowercase();
    lowered.contains("error") || lowered.contains("failed") || lowered.contains("failure")
}

fn evidence_count_for(candidate: &ReflectionCandidate) -> u32 {
    u32::try_from(candidate.evidence_record_ids.len()).unwrap_or(u32::MAX)
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

fn stable_target_id(parts: &[&str]) -> Result<TargetId, cairn_core::domain::DomainError> {
    TargetId::parse(stable_plan_ulid(parts).0)
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn expires_at_rfc3339() -> String {
    (chrono::Utc::now() + Duration::minutes(5)).to_rfc3339_opts(SecondsFormat::Secs, true)
}
