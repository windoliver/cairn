//! `PolicyGate::Display` produces the stable wire vocabulary listed in the
//! design doc §5. A snapshot freezes the full enum so any reorder or rename
//! requires an explicit review.

use cairn_core::policy_trace::PolicyGate;

#[test]
fn display_matches_wire_vocabulary() {
    let cases = [
        (PolicyGate::PresidioRedaction, "presidio_redaction"),
        (PolicyGate::PromptInjectionFence, "prompt_injection_fence"),
        (PolicyGate::FilterShouldMemorize, "filter_should_memorize"),
        (PolicyGate::VisibilityFloor, "visibility_floor"),
        (PolicyGate::ScopeCheck, "scope_check"),
        (PolicyGate::ForgetCapability, "forget_capability"),
        (PolicyGate::ConsentJournalAppend, "consent_journal_append"),
        (PolicyGate::ReadFilterRelevance, "read_filter_relevance"),
        (PolicyGate::ReadFilterStaleness, "read_filter_staleness"),
        (PolicyGate::ReadFilterDedup, "read_filter_dedup"),
    ];
    for (gate, expected) in cases {
        assert_eq!(gate.to_string(), expected, "gate {gate:?}");
    }
}

#[test]
fn debug_is_camel_case() {
    assert_eq!(
        format!("{:?}", PolicyGate::PresidioRedaction),
        "PresidioRedaction"
    );
}
