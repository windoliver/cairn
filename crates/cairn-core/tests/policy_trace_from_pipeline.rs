//! `From` impls map existing pipeline outcomes to `PolicyTraceEntry`.

use std::collections::BTreeMap;

use cairn_core::pipeline::filter::{
    Decision, DiscardReason, FenceMark, FenceMarkKind, FencedPayload, RedactedPayload,
    RedactionSpan, RedactionTag,
};
use cairn_core::policy_trace::{PolicyDetail, PolicyGate, PolicyOutcome, PolicyTraceEntry};

#[test]
fn decision_proceed_maps_to_pass() {
    let d = Decision::Proceed;
    let e: PolicyTraceEntry = (&d).into();
    assert_eq!(e.gate, PolicyGate::FilterShouldMemorize);
    assert_eq!(e.outcome, PolicyOutcome::Pass);
    assert_eq!(e.detail, PolicyDetail::None);
}

#[test]
fn decision_discard_maps_to_deny_with_reason() {
    let d = Decision::Discard(DiscardReason::PiiBlocked);
    let e: PolicyTraceEntry = (&d).into();
    assert_eq!(e.gate, PolicyGate::FilterShouldMemorize);
    assert_eq!(e.outcome, PolicyOutcome::Deny);
    assert_eq!(
        e.detail,
        PolicyDetail::DiscardReason(DiscardReason::PiiBlocked)
    );
}

#[test]
fn redacted_with_no_spans_is_pass_with_none() {
    let payload = RedactedPayload {
        text: String::from("clean"),
        spans: Vec::new(),
    };
    let e: PolicyTraceEntry = (&payload).into();
    assert_eq!(e.gate, PolicyGate::PresidioRedaction);
    assert_eq!(e.outcome, PolicyOutcome::Pass);
    assert_eq!(e.detail, PolicyDetail::None);
}

#[test]
fn redacted_with_spans_aggregates_counts() {
    let payload = RedactedPayload {
        text: String::from("***"),
        spans: vec![
            RedactionSpan {
                start: 0,
                end: 1,
                tag: RedactionTag::Email,
            },
            RedactionSpan {
                start: 2,
                end: 3,
                tag: RedactionTag::Email,
            },
            RedactionSpan {
                start: 4,
                end: 5,
                tag: RedactionTag::Ssn,
            },
        ],
    };
    let e: PolicyTraceEntry = (&payload).into();
    let mut expected: BTreeMap<RedactionTag, u32> = BTreeMap::new();
    expected.insert(RedactionTag::Email, 2);
    expected.insert(RedactionTag::Ssn, 1);
    assert_eq!(e.gate, PolicyGate::PresidioRedaction);
    assert_eq!(e.outcome, PolicyOutcome::Pass);
    assert_eq!(e.detail, PolicyDetail::RedactionTagCounts(expected));
}

#[test]
fn fenced_always_pass() {
    // Fencing is non-blocking by design — it wraps, never rejects.
    let payload = FencedPayload {
        text: String::from("[FENCE]x[/FENCE]"),
        marks: vec![FenceMark {
            start: 0,
            end: 16,
            kind: FenceMarkKind::Insertion,
        }],
    };
    let e: PolicyTraceEntry = (&payload).into();
    assert_eq!(e.gate, PolicyGate::PromptInjectionFence);
    assert_eq!(e.outcome, PolicyOutcome::Pass);
    assert_eq!(e.detail, PolicyDetail::None);
}
