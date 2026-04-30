//! Invariant: the JSON serialization of any `to_wire` output never
//! contains a body-bearing field name regardless of the contained
//! `PolicyDetail`. Reuses `ConsentEvent::BANNED_FIELDS` (13 keys) so
//! the `policy_trace` invariant stays in lockstep with #94's consent
//! journal walker.

use std::collections::BTreeMap;

use cairn_core::domain::{MemoryVisibility, consent::ConsentEvent};
use cairn_core::pipeline::filter::{DiscardReason, RedactionTag};
use cairn_core::policy_trace::{
    PolicyDetail, PolicyErrorCode, PolicyGate, PolicyOutcome, PolicyTraceEntry, to_wire,
};

/// Hard ceiling on any string value in a serialized policy trace. Real
/// detail strings are short (`error:wal_failure`, `redacted:email=2`);
/// anything past this is almost certainly smuggled body text.
const MAX_STRING_LEN: usize = 128;

fn walk(v: &serde_json::Value) {
    match v {
        serde_json::Value::Object(o) => {
            for (k, child) in o {
                for banned in ConsentEvent::BANNED_FIELDS {
                    assert_ne!(
                        k.as_str(),
                        *banned,
                        "policy trace JSON must not use field name {banned:?}: {v}"
                    );
                }
                walk(child);
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(walk),
        serde_json::Value::String(s) => {
            assert!(
                s.len() <= MAX_STRING_LEN,
                "policy trace string value is suspiciously long ({} > {MAX_STRING_LEN}): {s:?}",
                s.len()
            );
            assert!(
                !s.chars().any(char::is_whitespace),
                "policy trace string value must not contain whitespace (free-text marker): {s:?}"
            );
            assert!(
                s.is_ascii(),
                "policy trace string value must be ASCII (non-ASCII suggests free text): {s:?}"
            );
        }
        _ => {}
    }
}

fn assert_body_free(json: &str) {
    let v: serde_json::Value = serde_json::from_str(json).expect("valid JSON");
    walk(&v);
}

fn sample_entries() -> Vec<PolicyTraceEntry> {
    let mut counts = BTreeMap::new();
    counts.insert(RedactionTag::Email, 2);
    counts.insert(RedactionTag::Ssn, 1);
    vec![
        PolicyTraceEntry::pass(PolicyGate::PresidioRedaction),
        PolicyTraceEntry::pass(PolicyGate::PromptInjectionFence),
        PolicyTraceEntry::deny(
            PolicyGate::FilterShouldMemorize,
            PolicyDetail::DiscardReason(DiscardReason::PiiBlocked),
        ),
        PolicyTraceEntry::new(
            PolicyGate::PresidioRedaction,
            PolicyOutcome::Pass,
            PolicyDetail::RedactionTagCounts(counts),
        ),
        PolicyTraceEntry::new(
            PolicyGate::VisibilityFloor,
            PolicyOutcome::Pass,
            PolicyDetail::VisibilityFloor(MemoryVisibility::Session),
        ),
        PolicyTraceEntry::deny(
            PolicyGate::ScopeCheck,
            PolicyDetail::ScopeMismatch {
                required_tier: MemoryVisibility::Project,
            },
        ),
        PolicyTraceEntry::error(
            PolicyGate::ConsentJournalAppend,
            PolicyErrorCode::WAL_FAILURE,
        ),
    ]
}

#[test]
fn fixed_corpus_is_body_free() {
    let wire = to_wire(&sample_entries());
    let json = serde_json::to_string(&wire).expect("serializable");
    assert_body_free(&json);
}

#[test]
#[should_panic(expected = "must not use field name")]
fn walker_catches_banned_keys() {
    // Sanity check: the walker actually rejects bad input.
    let bad = serde_json::json!([{ "gate": "x", "result": "pass", "body": "leaked" }]);
    assert_body_free(&serde_json::to_string(&bad).expect("serializable"));
}

proptest::proptest! {
    #[test]
    fn arbitrary_traces_stay_body_free(seed in 0u64..1000) {
        // Deterministic shuffle of the fixed corpus; body-free invariant
        // is purely a function of the variant set we emit.
        let mut entries = sample_entries();
        let n = entries.len() as u64;
        entries.rotate_left(usize::try_from(seed % n).expect("len fits usize"));
        let wire = to_wire(&entries);
        let json = serde_json::to_string(&wire).expect("serializable");
        assert_body_free(&json);
    }
}
