//! FlushPlan seam for dream distillation output.

use std::collections::BTreeMap;
use std::sync::Arc;

use cairn_core::contract::memory_store::{MemoryStore, StoreError, UpsertOutcome};
use cairn_core::domain::flush_plan::{FlushMode, FlushPlan, PlanReason, PlannedMutation};
use cairn_core::domain::{Identity, MemoryRecord, ScopeTuple};
use cairn_core::generated::common::Ulid;
use chrono::{Duration, SecondsFormat};
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
/// Errors raised while building or applying a dream plan.
pub enum DreamPlanError {
    /// The generated plan did not match the narrow dream-upsert shape.
    #[error("invalid dream plan: {message}")]
    Invalid {
        /// Human-readable validation failure.
        message: String,
    },
    /// Underlying memory-store failure while applying the plan.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Build the autonomous one-upsert FlushPlan used by dream distillation.
pub fn build_dream_plan(
    record: MemoryRecord,
    issuer: &str,
    scope: ScopeTuple,
    tier: &str,
    evidence_count: usize,
) -> Result<FlushPlan, DreamPlanError> {
    let issuer = Identity::parse(issuer.to_owned()).map_err(|source| DreamPlanError::Invalid {
        message: format!("invalid dream plan issuer: {source}"),
    })?;
    let evidence_count = u32::try_from(evidence_count).unwrap_or(u32::MAX);
    let evidence_count_part = evidence_count.to_string();
    let operation_id = stable_plan_ulid(&[
        "dream",
        issuer.as_str(),
        tier,
        record.id.as_str(),
        record.target_id.as_str(),
        record.body.as_str(),
        evidence_count_part.as_str(),
    ]);
    Ok(FlushPlan {
        operation_id,
        issued_at: cairn_core::time::now_rfc3339_seconds(),
        issuer,
        principal: None,
        scope,
        mode: FlushMode::Autonomous,
        mutations: vec![PlannedMutation::Upsert {
            record: Box::new(record),
            prior_version: None,
        }],
        reason: PlanReason::Dream {
            tier: tier.to_owned(),
            evidence_count,
        },
        source_events: Vec::new(),
        target_hashes: BTreeMap::new(),
        dependencies: Vec::new(),
        expires_at: expires_at_rfc3339(),
        placeholder: false,
    })
}

/// Apply a dream FlushPlan after validating it contains exactly one upsert.
pub async fn apply_dream_plan(
    store: &Arc<dyn MemoryStore>,
    plan: FlushPlan,
) -> Result<UpsertOutcome, DreamPlanError> {
    let mut mutations = plan.mutations.into_iter();
    let Some(PlannedMutation::Upsert {
        record,
        prior_version: None,
    }) = mutations.next()
    else {
        return Err(DreamPlanError::Invalid {
            message: "dream plan must contain exactly one prior-version-free upsert".to_owned(),
        });
    };
    if mutations.next().is_some() {
        return Err(DreamPlanError::Invalid {
            message: "dream plan must contain exactly one mutation".to_owned(),
        });
    }
    store.upsert(&record).await.map_err(DreamPlanError::Store)
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

fn expires_at_rfc3339() -> String {
    (chrono::Utc::now() + Duration::minutes(5)).to_rfc3339_opts(SecondsFormat::Secs, true)
}
