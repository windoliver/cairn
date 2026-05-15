//! Property tests for `FlushPlan` JSON round-trip.
//!
//! Locks in the wire-stability invariant: arbitrary plan → `serde_json` →
//! plan == arbitrary plan (modulo `serde_json::Value` normalization).

#![allow(missing_docs)]

use std::collections::BTreeMap;

use cairn_core::domain::flush_plan::{
    CoordActionStatus, CoordSignalKind, ExpirationReason, FlushMode, FlushPlan, PatchTarget,
    PersistedPlan, PlanReason, PlanStatus, PlannedMutation, ReplaceOccurrence, StrReplace,
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

fn arb_identity() -> impl Strategy<Value = Identity> {
    "[a-z]{3,8}".prop_map(|name| Identity::parse(format!("agt:codex:{name}:v1")).unwrap())
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

fn arb_coord_mutation() -> impl Strategy<Value = PlannedMutation> {
    prop_oneof![
        (arb_target(), arb_identity()).prop_map(|(action_id, actor)| {
            PlannedMutation::LeaseAcquire {
                action_id,
                actor,
                ttl: "PT5M".into(),
                expires_at: "2026-05-13T20:00:00Z".into(),
            }
        }),
        (
            arb_target(),
            arb_identity(),
            prop::option::of("[a-z]{1,12}")
        )
            .prop_map(|(action_id, actor, reason)| PlannedMutation::LeaseRelease {
                action_id,
                actor,
                reason,
            },),
        (
            arb_identity(),
            arb_identity(),
            prop_oneof![
                Just(CoordSignalKind::TaskCompleted),
                Just(CoordSignalKind::LeaseReleased),
                Just(CoordSignalKind::RequestReview),
                Just(CoordSignalKind::UserInputNeeded),
                Just(CoordSignalKind::Error),
                Just(CoordSignalKind::Info),
            ],
            prop::option::of(arb_target()),
        )
            .prop_map(|(from_actor, to_actor, signal_kind, payload_id)| {
                PlannedMutation::SignalSend {
                    from_actor,
                    to_actor,
                    signal_kind,
                    payload_id,
                }
            }),
        (
            arb_target(),
            "[a-z][a-z ]{0,20}",
            prop::collection::vec(arb_target(), 0..4),
            -10i32..100i32,
        )
            .prop_map(
                |(id, title, depends_on, priority)| PlannedMutation::ActionCreate {
                    id,
                    title,
                    depends_on,
                    priority,
                }
            ),
        (
            arb_target(),
            prop_oneof![
                Just(CoordActionStatus::Pending),
                Just(CoordActionStatus::InProgress),
                Just(CoordActionStatus::Completed),
                Just(CoordActionStatus::Blocked),
                Just(CoordActionStatus::Cancelled),
            ],
            prop::option::of("[a-z]{1,12}"),
        )
            .prop_map(|(id, status, reason)| PlannedMutation::ActionUpdate {
                id,
                status,
                reason
            }),
        (
            "[a-z][a-z_]{0,16}",
            arb_ulid(),
            prop::collection::btree_map("[a-z]{1,8}", "[a-z0-9]{1,12}", 0..4),
        )
            .prop_map(|(routine_name, instance_id, vars)| {
                PlannedMutation::RoutineInstantiate {
                    routine_name,
                    instance_id,
                    vars,
                }
            }),
    ]
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
        arb_coord_mutation(),
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

#[test]
fn coord_mutations_round_trip_json() {
    let action_id = TargetId::parse("01JTS6R4J70000000000000010").unwrap();
    let actor = Identity::parse("agt:codex:gpt-5:worker:v1").unwrap();
    let peer = Identity::parse("agt:codex:gpt-5:reviewer:v1").unwrap();
    let dependency = TargetId::parse("01JTS6R4J70000000000000011").unwrap();
    let payload_id = TargetId::parse("01JTS6R4J70000000000000012").unwrap();
    let instance_id = Ulid("01JTS6R4J70000000000000013".into());

    let mutations = vec![
        PlannedMutation::LeaseAcquire {
            action_id: action_id.clone(),
            actor: actor.clone(),
            ttl: "PT5M".into(),
            expires_at: "2026-05-13T20:00:00Z".into(),
        },
        PlannedMutation::LeaseRelease {
            action_id: action_id.clone(),
            actor: actor.clone(),
            reason: Some("completed".into()),
        },
        PlannedMutation::SignalSend {
            from_actor: actor.clone(),
            to_actor: peer,
            signal_kind: CoordSignalKind::RequestReview,
            payload_id: Some(payload_id),
        },
        PlannedMutation::ActionCreate {
            id: action_id.clone(),
            title: "Review coordination contract".into(),
            depends_on: vec![dependency],
            priority: 10,
        },
        PlannedMutation::ActionUpdate {
            id: action_id,
            status: CoordActionStatus::InProgress,
            reason: Some("lease acquired".into()),
        },
        PlannedMutation::RoutineInstantiate {
            routine_name: "code_review".into(),
            instance_id,
            vars: BTreeMap::from([("pr".into(), "314".into())]),
        },
    ];

    for mutation in mutations {
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
        prop_assert_eq!(back.schema_version, back.plan.required_schema_version());
        prop_assert!(back.validate_schema_version().is_ok());
        prop_assert!(matches!(back.status, PlanStatus::Pending));
    }
}

#[test]
fn coord_mutations_require_persisted_plan_schema_v2() {
    let action_id = TargetId::parse("01HQZK000000000000000C0RD1").unwrap();
    let plan = FlushPlan {
        operation_id: Ulid("01HQZK000000000000000C0RD2".into()),
        issued_at: "2099-05-09T12:00:00Z".into(),
        issuer: Identity::parse("agt:codex:coord:v1").unwrap(),
        principal: None,
        scope: ScopeTuple::default(),
        mode: FlushMode::HumanReview,
        mutations: vec![PlannedMutation::ActionUpdate {
            id: action_id,
            status: CoordActionStatus::InProgress,
            reason: Some("lease acquired".into()),
        }],
        reason: PlanReason::UserIngest,
        source_events: vec![],
        target_hashes: BTreeMap::new(),
        dependencies: vec![],
        expires_at: "2099-05-09T12:05:00Z".into(),
        placeholder: false,
    };

    let mut persisted = PersistedPlan::pending(plan);
    assert_eq!(
        persisted.schema_version,
        PersistedPlan::COORD_SCHEMA_VERSION
    );
    assert!(persisted.validate_schema_version().is_ok());

    persisted.schema_version = PersistedPlan::BASE_SCHEMA_VERSION;
    assert!(persisted.validate_schema_version().is_err());
}
