//! `RecordExclusion` only accepts `ReadFilter*` gates. Constructing one
//! with a Tier-1 gate (e.g. `ScopeCheck`) is a programmer error and
//! a fail-closed safety check against leaking record ids.

use cairn_core::domain::TargetId;
use cairn_core::policy_trace::{PolicyDetail, PolicyGate, RecordExclusion};

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

// Test each non-ReadFilter* PolicyGate variant panics on construction.
reject_tier1_gate!(rejects_presidio_redaction, PolicyGate::PresidioRedaction);
reject_tier1_gate!(
    rejects_prompt_injection_fence,
    PolicyGate::PromptInjectionFence
);
reject_tier1_gate!(rejects_filter_should_memorize, PolicyGate::FilterShouldMemorize);
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
    let e = RecordExclusion::new(id.clone(), PolicyGate::ReadFilterStaleness, PolicyDetail::None);
    assert_eq!(e.target_id, id);
    assert_eq!(e.gate, PolicyGate::ReadFilterStaleness);
    assert_eq!(e.detail, PolicyDetail::None);
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
        assert_eq!(e.gate, gate);
    }
}
