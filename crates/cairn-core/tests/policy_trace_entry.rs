//! `PolicyTraceEntry` constructors and `to_wire` round-trip into the
//! generated `ResponsePolicyTrace` shape.

use cairn_core::domain::MemoryVisibility;
use cairn_core::generated::envelope::ResponsePolicyTraceResult;
use cairn_core::policy_trace::{PolicyDetail, PolicyGate, PolicyOutcome, PolicyTraceEntry, to_wire};

#[test]
fn entry_holds_gate_outcome_detail() {
    let e = PolicyTraceEntry::new(
        PolicyGate::ScopeCheck,
        PolicyOutcome::Deny,
        PolicyDetail::ScopeMismatch { required_tier: MemoryVisibility::Project },
    );
    assert_eq!(e.gate, PolicyGate::ScopeCheck);
    assert_eq!(e.outcome, PolicyOutcome::Deny);
    assert!(matches!(e.detail, PolicyDetail::ScopeMismatch { .. }));
}

#[test]
fn pass_constructor_uses_none_detail() {
    let e = PolicyTraceEntry::pass(PolicyGate::PromptInjectionFence);
    assert_eq!(e.outcome, PolicyOutcome::Pass);
    assert_eq!(e.detail, PolicyDetail::None);
}

#[test]
fn to_wire_maps_each_field() {
    let entries = vec![
        PolicyTraceEntry::pass(PolicyGate::PresidioRedaction),
        PolicyTraceEntry::new(
            PolicyGate::FilterShouldMemorize,
            PolicyOutcome::Deny,
            PolicyDetail::ErrorCode("pii_blocked"),
        ),
    ];
    let wire = to_wire(&entries);
    assert_eq!(wire.len(), 2);
    assert_eq!(wire[0].gate, "presidio_redaction");
    assert_eq!(wire[0].result, ResponsePolicyTraceResult::Pass);
    assert_eq!(wire[0].detail, None); // empty wire string maps to None

    assert_eq!(wire[1].gate, "filter_should_memorize");
    assert_eq!(wire[1].result, ResponsePolicyTraceResult::Deny);
    assert_eq!(wire[1].detail.as_deref(), Some("error:pii_blocked"));
}

#[test]
fn to_wire_preserves_order() {
    let entries = vec![
        PolicyTraceEntry::pass(PolicyGate::PresidioRedaction),
        PolicyTraceEntry::pass(PolicyGate::PromptInjectionFence),
        PolicyTraceEntry::pass(PolicyGate::FilterShouldMemorize),
    ];
    let wire = to_wire(&entries);
    let gates: Vec<&str> = wire.iter().map(|e| e.gate.as_str()).collect();
    assert_eq!(gates, vec!["presidio_redaction", "prompt_injection_fence", "filter_should_memorize"]);
}
