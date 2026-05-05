//! Property tests for `FlushPlan` JSON round-trip.
//!
//! Locks in the wire-stability invariant: arbitrary plan → `serde_json` →
//! plan == arbitrary plan (modulo `serde_json::Value` normalization).

#![allow(missing_docs)]

use std::collections::BTreeMap;

use cairn_core::domain::flush_plan::{
    ExpirationReason, FlushMode, FlushPlan, PersistedPlan, PlanReason, PlanStatus, PlannedMutation,
};
use cairn_core::domain::{Identity, ScopeTuple, TargetId};
use cairn_core::generated::common::Ulid;
use proptest::prelude::*;

fn arb_target() -> impl Strategy<Value = TargetId> {
    // 26-char Crockford base32, leading char 0..=7, no I/L/O/U.
    "[0-7][0-9A-HJKMNP-TV-Z]{25}".prop_map(|s| TargetId::parse(s).unwrap())
}

fn arb_ulid() -> impl Strategy<Value = Ulid> {
    "[0-7][0-9A-HJKMNP-TV-Z]{25}".prop_map(Ulid)
}

fn arb_mutation() -> impl Strategy<Value = PlannedMutation> {
    prop_oneof![
        (arb_target(), 0u32..u32::MAX).prop_map(|(target, prior_version)| {
            PlannedMutation::Delete {
                target,
                prior_version,
            }
        }),
        arb_target().prop_map(|target| PlannedMutation::ForgetRecord { target }),
        (
            arb_target(),
            prop_oneof![
                Just(ExpirationReason::TtlExpired),
                Just(ExpirationReason::SalienceBelowThreshold),
                Just(ExpirationReason::SupersededByCanonical),
            ]
        )
            .prop_map(|(target, reason)| PlannedMutation::Expire { target, reason }),
    ]
}

fn arb_mode() -> impl Strategy<Value = FlushMode> {
    prop_oneof![
        Just(FlushMode::Autonomous),
        Just(FlushMode::DryRun),
        Just(FlushMode::HumanReview),
    ]
}

fn arb_plan() -> impl Strategy<Value = FlushPlan> {
    (
        arb_ulid(),
        arb_mode(),
        prop::collection::vec(arb_mutation(), 1..6),
    )
        .prop_map(|(operation_id, mode, mutations)| FlushPlan {
            operation_id,
            issued_at: "2026-05-04T12:00:00Z".into(),
            issuer: Identity::parse("agt:claude-code:opus-4-7:reviewer:v1").unwrap(),
            principal: None,
            scope: ScopeTuple::default(),
            mode,
            mutations,
            reason: PlanReason::UserIngest,
            source_events: vec![],
            target_hashes: BTreeMap::default(),
            dependencies: vec![],
            expires_at: "2026-05-04T12:05:00Z".into(),
        })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, .. ProptestConfig::default() })]

    #[test]
    fn flush_plan_json_round_trip(plan in arb_plan()) {
        let bytes = serde_json::to_vec(&plan).expect("serialize");
        let back: FlushPlan = serde_json::from_slice(&bytes).expect("deserialize");
        prop_assert_eq!(serde_json::to_value(&plan).unwrap(), serde_json::to_value(&back).unwrap());
    }

    #[test]
    fn persisted_plan_round_trip(plan in arb_plan()) {
        let p = PersistedPlan::pending(plan);
        let bytes = serde_json::to_vec(&p).expect("serialize");
        let back: PersistedPlan = serde_json::from_slice(&bytes).expect("deserialize");
        prop_assert_eq!(back.schema_version, PersistedPlan::SCHEMA_VERSION);
        prop_assert!(matches!(back.status, PlanStatus::Pending));
    }
}
