//! Property tests for `FlushPlan` JSON round-trip.
//!
//! Locks in the wire-stability invariant: arbitrary plan → `serde_json` →
//! plan == arbitrary plan (modulo `serde_json::Value` normalization).

#![allow(missing_docs)]

use std::collections::BTreeMap;

use cairn_core::domain::flush_plan::{
    ExpirationReason, FlushMode, FlushPlan, PatchTarget, PersistedPlan, PlanReason, PlanStatus,
    PlannedMutation, ReplaceOccurrence, StrReplace,
};
use cairn_core::domain::{Identity, ScopeTuple, SessionId, TargetId};
use cairn_core::generated::common::Ulid;
use proptest::prelude::*;

fn arb_target() -> impl Strategy<Value = TargetId> {
    // 26-char Crockford base32, leading char 0..=7, no I/L/O/U.
    "[0-7][0-9A-HJKMNP-TV-Z]{25}".prop_map(|s| TargetId::parse(s).unwrap())
}

fn arb_ulid() -> impl Strategy<Value = Ulid> {
    "[0-7][0-9A-HJKMNP-TV-Z]{25}".prop_map(Ulid)
}

fn arb_session() -> impl Strategy<Value = SessionId> {
    "[0-7][0-9A-HJKMNP-TV-Z]{25}".prop_map(|s| SessionId::parse(&s).unwrap())
}

fn arb_patch_target() -> impl Strategy<Value = PatchTarget> {
    prop_oneof![
        arb_target().prop_map(PatchTarget::Record),
        arb_session().prop_map(PatchTarget::Session),
    ]
}

fn arb_replace_occurrence() -> impl Strategy<Value = ReplaceOccurrence> {
    prop_oneof![
        Just(ReplaceOccurrence::First),
        Just(ReplaceOccurrence::All),
        (0usize..4).prop_map(ReplaceOccurrence::Nth),
    ]
}

fn arb_str_replace() -> impl Strategy<Value = StrReplace> {
    ("[a-z]{1,8}", "[a-z]{1,8}", arb_replace_occurrence()).prop_map(|(old, new, occurrence)| {
        StrReplace {
            old,
            new,
            occurrence,
        }
    })
}

fn arb_mutation() -> impl Strategy<Value = PlannedMutation> {
    prop_oneof![
        (arb_target(), 0u32..u32::MAX).prop_map(|(target, prior_version)| {
            PlannedMutation::Delete {
                target,
                prior_version,
            }
        }),
        (
            arb_patch_target(),
            prop::collection::vec(arb_str_replace(), 1..3),
        )
            .prop_map(|(target, str_replace)| PlannedMutation::Patch {
                target,
                str_replace,
            }),
        (arb_target(), arb_target())
            .prop_map(|(record_id, new_id)| PlannedMutation::Rename { record_id, new_id }),
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
            placeholder: false,
        })
}

#[test]
fn patch_and_rename_round_trip_json() {
    use cairn_core::domain::flush_plan::{
        PatchTarget, PlannedMutation, ReplaceOccurrence, StrReplace,
    };

    let patch = PlannedMutation::Patch {
        target: PatchTarget::Session(SessionId::parse("01JTS6R4J70000000000000000").unwrap()),
        str_replace: vec![StrReplace {
            old: "old-title".into(),
            new: "new-title".into(),
            occurrence: ReplaceOccurrence::First,
        }],
    };
    let rename = PlannedMutation::Rename {
        record_id: TargetId::parse("01JTS6R4J70000000000000001").unwrap(),
        new_id: TargetId::parse("01JTS6R4J70000000000000002").unwrap(),
    };

    for mutation in [patch, rename] {
        let bytes = serde_json::to_vec(&mutation).expect("serialize");
        let back: PlannedMutation = serde_json::from_slice(&bytes).expect("deserialize");
        assert_eq!(
            serde_json::to_value(&mutation).unwrap(),
            serde_json::to_value(&back).unwrap()
        );
    }
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
