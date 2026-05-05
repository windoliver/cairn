//! Plan fixtures shared between core tests and CLI integration tests.

use cairn_core::domain::flush_plan::{
    FlushMode, FlushPlan, PersistedPlan, PlanReason, PlannedMutation,
};
use cairn_core::domain::{Identity, ScopeTuple, TargetId};
use cairn_core::generated::common::Ulid;

/// Build a sample [`FlushPlan`] with one `Delete` mutation. Used across
/// core tests + CLI integration tests so the wire shape stays consistent.
///
/// `operation_id` must be a 26-char Crockford base32 ULID (uppercase, no
/// `I L O U`, leading char `0..=7`). The function panics if the value is
/// rejected by [`Identity::parse`] internally — fixtures only.
#[must_use]
#[allow(clippy::expect_used)]
pub fn sample_plan(operation_id: &str, mode: FlushMode) -> FlushPlan {
    FlushPlan {
        operation_id: Ulid(operation_id.into()),
        issued_at: "2026-05-04T12:00:00Z".into(),
        issuer: Identity::parse("agt:claude-code:opus-4-7:reviewer:v1")
            .expect("hard-coded fixture identity is always valid"),
        principal: None,
        scope: ScopeTuple::default(),
        mode,
        mutations: vec![PlannedMutation::Delete {
            target: TargetId::parse("01HQZX9F5N0000000000000000")
                .expect("hard-coded fixture target ULID is always valid"),
            prior_version: 1,
        }],
        reason: PlanReason::UserIngest,
        source_events: vec![],
        target_hashes: std::collections::BTreeMap::new(),
        dependencies: vec![],
        expires_at: "2026-05-04T12:05:00Z".into(),
    }
}

/// Wrap [`sample_plan`] in a [`PersistedPlan`] with `mode=HumanReview` and
/// `status=Pending`.
#[must_use]
pub fn sample_pending(operation_id: &str) -> PersistedPlan {
    PersistedPlan::pending(sample_plan(operation_id, FlushMode::HumanReview))
}
