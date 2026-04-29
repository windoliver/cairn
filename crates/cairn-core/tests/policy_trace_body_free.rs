//! Invariant: the JSON serialization of any `to_wire` output never
//! contains a body-bearing field name regardless of the contained
//! `PolicyDetail`. Same body-free pattern as #94's `ConsentEvent` walker.
//!
//! Banned key names — even appearing as a substring of a value would
//! be suspicious — match the #94 invariant set:
//! `body | text | raw | command | url | content | payload`.

use std::collections::BTreeMap;

use cairn_core::domain::MemoryVisibility;
use cairn_core::pipeline::filter::{DiscardReason, RedactionTag};
use cairn_core::policy_trace::{PolicyDetail, PolicyGate, PolicyOutcome, PolicyTraceEntry, to_wire};

const BANNED_KEYS: &[&str] = &["body", "text", "raw", "command", "url", "content", "payload"];

fn walk(v: &serde_json::Value) {
    match v {
        serde_json::Value::Object(o) => {
            for (k, child) in o {
                for banned in BANNED_KEYS {
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
            PolicyDetail::ScopeMismatch { required_tier: MemoryVisibility::Project },
        ),
        PolicyTraceEntry::error(PolicyGate::ConsentJournalAppend, "wal_failure"),
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
