//! `RecordExclusion` only accepts `ReadFilter*` gates. Constructing one
//! with a Tier-1 gate (e.g. `ScopeCheck`) is a programmer error and
//! a fail-closed safety check against leaking record ids.

use cairn_core::domain::TargetId;
use cairn_core::generated::common::{
    RecordExclusion as WireRecordExclusion, RecordExclusionGate as WireRecordExclusionGate,
};
use cairn_core::policy_trace::{PolicyDetail, PolicyGate, RecordExclusion, to_wire_exclusions};

// Valid TargetId: 26-char Crockford base32, leading char 0..=7.
const FIXTURE_ID: &str = "01HQZX9F5N0000000000000000";

// Macro to reduce boilerplate for testing rejection of all Tier-1 gates.
// Each non-ReadFilter* gate must panic on RecordExclusion::new.
macro_rules! reject_tier1_gate {
    ($name:ident, $gate:expr) => {
        #[test]
        #[should_panic(expected = "ReadFilter")]
        fn $name() {
            let id = TargetId::parse(FIXTURE_ID).expect("valid ULID");
            let _ = RecordExclusion::new(id, $gate, PolicyDetail::None);
        }
    };
}

// Compile-time assertion that `RecordExclusion`'s fields are private,
// so external callers cannot bypass the `new` constructor by writing
//
//     RecordExclusion { gate: PolicyGate::ScopeCheck, target_id: …, detail: … }
//
// (which would dodge the ReadFilter*-only invariant). The assertion is
// the field-literal form below — it MUST fail to compile from this
// integration-test crate. If a future change makes any field `pub`,
// uncomment the body and the build will succeed, signalling the
// regression that codex flagged in PR #237 round 5:
//
//     fn _record_exclusion_struct_literal_must_not_compile() {
//         let _ = RecordExclusion {
//             target_id: TargetId::parse(FIXTURE_ID).unwrap(),
//             gate: PolicyGate::ScopeCheck,
//             detail: PolicyDetail::None,
//         };
//     }

// Test each non-ReadFilter* PolicyGate variant panics on construction.
reject_tier1_gate!(rejects_presidio_redaction, PolicyGate::PresidioRedaction);
reject_tier1_gate!(
    rejects_prompt_injection_fence,
    PolicyGate::PromptInjectionFence
);
reject_tier1_gate!(
    rejects_filter_should_memorize,
    PolicyGate::FilterShouldMemorize
);
reject_tier1_gate!(rejects_visibility_floor, PolicyGate::VisibilityFloor);
reject_tier1_gate!(rejects_scope_check, PolicyGate::ScopeCheck);
reject_tier1_gate!(rejects_forget_capability, PolicyGate::ForgetCapability);
reject_tier1_gate!(
    rejects_consent_journal_append,
    PolicyGate::ConsentJournalAppend
);

#[test]
fn exclusion_holds_target_gate_detail() {
    let id = TargetId::parse(FIXTURE_ID).expect("valid ULID");
    let e = RecordExclusion::new(
        id.clone(),
        PolicyGate::ReadFilterStaleness,
        PolicyDetail::None,
    );
    assert_eq!(e.target_id(), &id);
    assert_eq!(e.gate(), PolicyGate::ReadFilterStaleness);
    assert_eq!(e.detail(), &PolicyDetail::None);
}

#[test]
fn exclusion_accepts_all_three_read_filter_gates() {
    let id = TargetId::parse(FIXTURE_ID).expect("valid ULID");
    for gate in [
        PolicyGate::ReadFilterRelevance,
        PolicyGate::ReadFilterStaleness,
        PolicyGate::ReadFilterDedup,
    ] {
        let e = RecordExclusion::new(id.clone(), gate, PolicyDetail::None);
        assert_eq!(e.gate(), gate);
    }
}

#[test]
fn wire_conversion_maps_each_read_filter_gate() {
    let id = TargetId::parse(FIXTURE_ID).expect("valid ULID");
    let cases = [
        (
            PolicyGate::ReadFilterRelevance,
            WireRecordExclusionGate::ReadFilterRelevance,
        ),
        (
            PolicyGate::ReadFilterStaleness,
            WireRecordExclusionGate::ReadFilterStaleness,
        ),
        (
            PolicyGate::ReadFilterDedup,
            WireRecordExclusionGate::ReadFilterDedup,
        ),
    ];
    for (core_gate, wire_gate) in cases {
        let core = RecordExclusion::new(id.clone(), core_gate, PolicyDetail::None);
        let wire: WireRecordExclusion = (&core).into();
        assert_eq!(wire.gate, wire_gate);
        assert_eq!(wire.target_id.0, FIXTURE_ID);
    }
}

#[test]
fn wire_conversion_serializes_detail_as_stable_code() {
    let id = TargetId::parse(FIXTURE_ID).expect("valid ULID");
    let core = RecordExclusion::new(id, PolicyGate::ReadFilterStaleness, PolicyDetail::None);
    let wire: WireRecordExclusion = (&core).into();
    assert_eq!(
        wire.detail,
        PolicyDetail::None.to_wire_string(),
        "wire detail must come from PolicyDetail::to_wire_string()"
    );
}

#[test]
fn to_wire_exclusions_preserves_order_and_count() {
    let a = TargetId::parse("01HQZX9F5N000000000000000A").expect("valid ULID");
    let b = TargetId::parse("01HQZX9F5N000000000000000B").expect("valid ULID");
    let items = vec![
        RecordExclusion::new(a.clone(), PolicyGate::ReadFilterDedup, PolicyDetail::None),
        RecordExclusion::new(
            b.clone(),
            PolicyGate::ReadFilterStaleness,
            PolicyDetail::None,
        ),
    ];
    let wire = to_wire_exclusions(&items);
    assert_eq!(wire.len(), 2);
    assert_eq!(wire[0].target_id.0, a.to_string());
    assert_eq!(wire[1].target_id.0, b.to_string());
}
