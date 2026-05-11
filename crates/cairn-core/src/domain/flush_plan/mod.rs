//! Typed `FlushPlan` for brief §5.5 — one plan per write-path mutation,
//! same shape across `autonomous`, `dry_run`, and `human_review` modes.
//!
//! Pure data + serde. No I/O, no async. CLI / adapter crates do the file
//! writes; this module's path helpers (`store.rs`) only return paths.
//!
//! Brief sources:
//! - §5.5 Plan, then apply
//! - §5.6 WAL envelope (`operation_id`, `target_hash`, `dependencies`, `expires_at`)
//! - §5.2 Write path (mutation kinds)

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::domain::{Identity, MemoryKind, MemoryRecord, ScopeTuple, SessionId, TargetId};
use crate::generated::common::Ulid;

pub mod diff;
pub mod store;

/// Dispatch mode for a write-path run (brief §5.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlushMode {
    /// Capture → … → Plan → apply inline, same turn (default).
    Autonomous,
    /// Plan returned without side effects.
    DryRun,
    /// Plan persisted to `.cairn/flush/pending/`; apply waits for an
    /// explicit `cairn flush apply <id>`.
    HumanReview,
}

/// Typed plan. The `apply` step is a pure function from
/// `FlushPlan → side effects`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlushPlan {
    /// Doubles as the WAL `operation_id` and the on-disk filename stem.
    pub operation_id: Ulid,
    /// RFC 3339 timestamp. Stored as a String for wire stability; the
    /// canonical conversion to [`crate::domain::Rfc3339Timestamp`] happens
    /// at the call boundary that needs it.
    pub issued_at: String,
    /// Identity of the agent or sensor that produced this plan.
    pub issuer: Identity,
    /// Present when policy tier requires a human principal (§5.6).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub principal: Option<Identity>,
    /// Scope context for all mutations in this plan.
    pub scope: ScopeTuple,
    /// How this plan will be (or was) dispatched.
    pub mode: FlushMode,
    /// Ordered list of mutations to apply atomically.
    pub mutations: Vec<PlannedMutation>,
    /// Why this plan was produced — surfaces in the human diff and audit log.
    pub reason: PlanReason,
    /// Capture / extract event ids that motivated this plan (free-form
    /// references — opaque ULIDs).
    #[serde(default)]
    pub source_events: Vec<Ulid>,
    /// Pre-state SHA-256 hashes per target (lowercase hex). Used at apply
    /// time to detect drift.
    #[serde(default)]
    pub target_hashes: BTreeMap<String, String>,
    /// WAL ops this one must apply after (§5.6).
    #[serde(default)]
    pub dependencies: Vec<Ulid>,
    /// 5-minute receipt TTL (§5.6). Apply past this is rejected.
    pub expires_at: String,
    /// `true` when the plan was produced by the CLI stub planner (no live
    /// ingest pipeline yet, awaiting #9). `apply` honors this by emitting a
    /// prominent warning and recording `apply_kind = "metadata_only"` in
    /// the resulting [`PlanStatus::Applied`] — the file moves but no
    /// `MemoryStore` mutation runs. Defaults to `false` so a real planner's
    /// plans are treated as authoritative.
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    pub placeholder: bool,
}

impl FlushPlan {
    /// Idempotency key per §5.6 — the `operation_id`.
    #[must_use]
    pub fn idempotency_key(&self) -> &Ulid {
        &self.operation_id
    }

    /// Pre-state hash for `target`, if recorded.
    #[must_use]
    pub fn target_hash(&self, target: &TargetId) -> Option<&str> {
        self.target_hashes.get(target.as_str()).map(String::as_str)
    }

    /// Pre-state hash for a session metadata target (issue #289 review
    /// round 2). `target_hashes` is keyed by string, so session patches
    /// reuse it under the session id.
    #[must_use]
    pub fn session_hash(&self, session: &SessionId) -> Option<&str> {
        self.target_hashes
            .get(session.as_str())
            .map(String::as_str)
    }
}

/// One concrete mutation inside a [`FlushPlan`]. Tagged externally for
/// stable JSON shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatchTarget {
    /// Patch the active record for the target.
    Record(TargetId),
    /// Patch the session metadata document for the session.
    Session(SessionId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Which match occurrence(s) a patch replacement should edit.
pub enum ReplaceOccurrence {
    /// Replace the first matching occurrence.
    First,
    /// Replace every matching occurrence.
    All,
    /// Replace the nth matching occurrence (zero-based).
    Nth(usize),
}

/// One string replacement inside a patch mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrReplace {
    /// Existing substring to find.
    pub old: String,
    /// Replacement substring.
    pub new: String,
    /// Which occurrence(s) to replace.
    pub occurrence: ReplaceOccurrence,
}

/// One concrete mutation inside a [`FlushPlan`]. Tagged externally for
/// stable JSON shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PlannedMutation {
    /// Insert or update a record, optionally asserting the prior version.
    Upsert {
        /// Record to write. Boxed to keep enum variants similarly sized.
        record: Box<MemoryRecord>,
        /// Prior version to assert optimistic-concurrency on, if known.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        prior_version: Option<u32>,
    },
    /// Delete a record, asserting the prior version.
    Delete {
        /// Target lineage key to delete.
        target: TargetId,
        /// Version the caller observed — prevents blind deletes.
        prior_version: u32,
    },
    /// Patch an existing record or session metadata document.
    Patch {
        /// Which document the string replacements apply to.
        target: PatchTarget,
        /// Ordered string replacements applied left-to-right.
        str_replace: Vec<StrReplace>,
    },
    /// Rename an existing target id to a new target id.
    Rename {
        /// Existing target lineage key.
        record_id: TargetId,
        /// Destination lineage key.
        new_id: TargetId,
    },
    /// Promote a raw/wiki record to a different memory kind.
    Promote {
        /// Source target id.
        from: TargetId,
        /// Destination memory kind.
        to_kind: MemoryKind,
        /// Evidence ULID references that support the promotion.
        evidence: Vec<Ulid>,
    },
    /// Mark a record as expired (brief §10 `ExpirationWorkflow`).
    Expire {
        /// Target to expire.
        target: TargetId,
        /// Why this expiration was planned.
        reason: ExpirationReason,
    },
    /// Forget every record linked to a session.
    ForgetSession {
        /// Session to forget.
        session: SessionId,
    },
    /// Forget a specific record by target id.
    ForgetRecord {
        /// Target lineage key to forget.
        target: TargetId,
    },
    /// Evolve a skill record by applying a diff.
    Evolve {
        /// Skill target id.
        skill: TargetId,
        /// Path to the diff file (relative to vault root).
        diff_ref: PathBuf,
    },
}

/// Why an expiration was planned (brief §10 `ExpirationWorkflow`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExpirationReason {
    /// Record's TTL timestamp has passed.
    TtlExpired,
    /// Salience score dropped below the configured threshold.
    SalienceBelowThreshold,
    /// A canonical record superseded this one.
    SupersededByCanonical,
}

/// Why this plan was produced — surfaces in the human diff and audit log.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PlanReason {
    /// Triggered by a direct user ingest command.
    UserIngest,
    /// Triggered by a sensor capture event.
    SensorCapture {
        /// Sensor label that produced the capture.
        sensor: String,
    },
    /// Triggered by the promotion workflow.
    Promote {
        /// Model confidence at promotion time.
        confidence: f32,
        /// Number of evidence references consulted.
        evidence_count: u32,
    },
    /// Triggered by the expiration workflow.
    Expire {
        /// Whether the TTL timestamp was the trigger.
        ttl_expired: bool,
        /// Salience score at expiration time, if that was a factor.
        salience_below: Option<f32>,
    },
    /// Triggered by a forget request.
    Forget {
        /// Request ULID for audit tracing.
        request_id: Ulid,
    },
    /// Triggered by a skill evolution step.
    Evolve {
        /// Version of the skill before this evolution.
        previous_version: u32,
    },
}

/// On-disk wrapper persisted under `.cairn/flush/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedPlan {
    /// On-disk schema version. Always `1` in this PR.
    pub schema_version: u16,
    /// The plan itself.
    pub plan: FlushPlan,
    /// Current lifecycle state of this persisted plan.
    pub status: PlanStatus,
}

impl PersistedPlan {
    /// Schema version constant — bump when the on-disk shape changes.
    pub const SCHEMA_VERSION: u16 = 1;

    /// Wrap a [`FlushPlan`] in a [`PersistedPlan`] with [`PlanStatus::Pending`].
    #[must_use]
    pub fn pending(plan: FlushPlan) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            plan,
            status: PlanStatus::Pending,
        }
    }
}

/// How a `PlanStatus::Applied` was actually executed.
///
/// `MetadataOnly` indicates the plan's lifecycle marker advanced (file moved
/// from `pending/` to `applied/`) without any `MemoryStore` mutation —
/// the path the CLI takes today while the WAL state machine (#9) is being
/// wired. `Full` indicates the WAL apply executed the mutations against the
/// store. Defaults to `MetadataOnly` so historical plans deserialize as the
/// honest no-op state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ApplyKind {
    /// File moved to `applied/` but no `MemoryStore` mutations executed.
    /// Operator-visible warning is emitted at apply time.
    #[default]
    MetadataOnly,
    /// WAL apply ran and `MemoryStore` reflects the mutations.
    Full,
}

/// Lifecycle state of a persisted plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PlanStatus {
    /// Plan has not yet been applied or rejected.
    Pending,
    /// Plan was applied successfully.
    Applied {
        /// RFC 3339 timestamp of the apply operation.
        at: String,
        /// Distinguishes a real `MemoryStore`-backed apply from a metadata-only
        /// move that left the store untouched. The metadata-only path exists
        /// while the WAL apply integration (#9) lands; once that wires up, all
        /// applies are `ApplyKind::Full`.
        #[serde(default)]
        apply_kind: ApplyKind,
    },
    /// Plan was rejected by the human reviewer.
    Rejected {
        /// RFC 3339 timestamp of the rejection.
        at: String,
        /// Human-readable reason for rejection.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Identity, ScopeTuple, TargetId};
    use crate::generated::common::Ulid;

    fn sample_plan() -> FlushPlan {
        FlushPlan {
            operation_id: Ulid("01HQZK000000000000000000VP".into()),
            issued_at: "2026-05-04T12:00:00Z".into(),
            issuer: Identity::parse("agt:claude-code:opus-4-7:reviewer:v1").unwrap(),
            principal: Some(Identity::parse("hmn:tafeng:v1").unwrap()),
            scope: ScopeTuple::default(),
            mode: FlushMode::Autonomous,
            mutations: vec![PlannedMutation::Delete {
                target: TargetId::parse("01HQZX9F5N0000000000000000").unwrap(),
                prior_version: 1,
            }],
            reason: PlanReason::UserIngest,
            source_events: vec![],
            target_hashes: BTreeMap::default(),
            dependencies: vec![],
            expires_at: "2026-05-04T12:05:00Z".into(),
            placeholder: false,
        }
    }

    #[test]
    fn flush_plan_round_trips_through_json() {
        let plan = sample_plan();
        let bytes = serde_json::to_vec(&plan).expect("serialize");
        let back: FlushPlan = serde_json::from_slice(&bytes).expect("deserialize");
        assert_eq!(
            serde_json::to_value(&plan).unwrap(),
            serde_json::to_value(&back).unwrap()
        );
    }

    fn plan_with(mutations: Vec<PlannedMutation>) -> FlushPlan {
        let mut p = sample_plan();
        p.mutations = mutations;
        p
    }

    #[test]
    fn snapshot_delete_plan_json() {
        let plan = sample_plan();
        let json = serde_json::to_string_pretty(&PersistedPlan::pending(plan)).unwrap();
        insta::assert_snapshot!("plan_delete_json", json);
    }

    #[test]
    fn snapshot_expire_plan_json() {
        let plan = plan_with(vec![PlannedMutation::Expire {
            target: TargetId::parse("01HQZX9F5N0000000000000000").unwrap(),
            reason: ExpirationReason::TtlExpired,
        }]);
        let json = serde_json::to_string_pretty(&PersistedPlan::pending(plan)).unwrap();
        insta::assert_snapshot!("plan_expire_json", json);
    }

    #[test]
    fn snapshot_diff_markdown_for_delete() {
        let md = diff::render(&sample_plan());
        insta::assert_snapshot!("diff_delete_md", md);
    }

    #[test]
    fn snapshot_status_transitions() {
        let plan = sample_plan();
        let mut p = PersistedPlan::pending(plan);
        p.status = PlanStatus::Applied {
            at: "2026-05-04T12:01:00Z".into(),
            apply_kind: ApplyKind::Full,
        };
        insta::assert_snapshot!(
            "status_applied_json",
            serde_json::to_string_pretty(&p).unwrap()
        );
        p.status = PlanStatus::Rejected {
            at: "2026-05-04T12:02:00Z".into(),
            reason: "operator rejected".into(),
        };
        insta::assert_snapshot!(
            "status_rejected_json",
            serde_json::to_string_pretty(&p).unwrap()
        );
    }
}
