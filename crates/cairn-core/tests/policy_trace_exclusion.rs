//! `RecordExclusion` only accepts `ReadFilter*` gates. Constructing one
//! with a Tier-1 gate (e.g. `ScopeCheck`) is a programmer error and
//! a fail-closed safety check against leaking record ids.

use cairn_core::domain::TargetId;
use cairn_core::policy_trace::{PolicyDetail, PolicyGate, RecordExclusion};

// Valid TargetId: 26-char Crockford base32, leading char 0..=7.
const FIXTURE_ID: &str = "01HQZX9F5N0000000000000000";

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

#[test]
#[should_panic(expected = "ReadFilter")]
fn exclusion_rejects_scope_check_gate() {
    // ScopeCheck is a Tier-1 gate; per design §5.5, a Tier-1 invisible
    // record's id must never appear in `excluded`.
    let id = TargetId::parse(FIXTURE_ID).expect("valid ULID");
    let _ = RecordExclusion::new(id, PolicyGate::ScopeCheck, PolicyDetail::None);
}

#[test]
#[should_panic(expected = "ReadFilter")]
fn exclusion_rejects_filter_should_memorize_gate() {
    let id = TargetId::parse(FIXTURE_ID).expect("valid ULID");
    let _ = RecordExclusion::new(id, PolicyGate::FilterShouldMemorize, PolicyDetail::None);
}
