//! `From` impls converting existing pipeline outcome types into
//! [`PolicyTraceEntry`]. Verbs compose gates as today and call
//! `(&result).into()` to push a trace entry.
//!
//! New pipeline outcome types that fire a gate must add a `From` impl
//! here so producers stay closed.

use std::collections::BTreeMap;

use crate::pipeline::filter::{Decision, FencedPayload, RedactedPayload, RedactionTag};

use super::{PolicyDetail, PolicyGate, PolicyOutcome, PolicyTraceEntry};

impl From<&Decision> for PolicyTraceEntry {
    fn from(d: &Decision) -> Self {
        match d {
            Decision::Proceed => PolicyTraceEntry::pass(PolicyGate::FilterShouldMemorize),
            Decision::Discard(reason) => PolicyTraceEntry::deny(
                PolicyGate::FilterShouldMemorize,
                PolicyDetail::DiscardReason(*reason),
            ),
        }
    }
}

impl From<&RedactedPayload> for PolicyTraceEntry {
    fn from(p: &RedactedPayload) -> Self {
        if p.spans.is_empty() {
            return PolicyTraceEntry::pass(PolicyGate::PresidioRedaction);
        }
        let mut counts: BTreeMap<RedactionTag, u32> = BTreeMap::new();
        for span in &p.spans {
            *counts.entry(span.tag).or_default() += 1;
        }
        PolicyTraceEntry::new(
            PolicyGate::PresidioRedaction,
            PolicyOutcome::Pass,
            PolicyDetail::RedactionTagCounts(counts),
        )
    }
}

impl From<&FencedPayload> for PolicyTraceEntry {
    fn from(_: &FencedPayload) -> Self {
        // Fencing is non-blocking — it wraps but never rejects.
        PolicyTraceEntry::pass(PolicyGate::PromptInjectionFence)
    }
}
