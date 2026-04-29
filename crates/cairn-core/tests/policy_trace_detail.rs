//! `PolicyDetail` is body-free: every variant carries only metadata
//! (counts, codes, enum tags) and never raw bytes. The wire string
//! produced by `to_wire_string` is short and stable.

use std::collections::BTreeMap;

use cairn_core::domain::MemoryVisibility;
use cairn_core::pipeline::filter::{DiscardReason, RedactionTag};
use cairn_core::policy_trace::PolicyDetail;

#[test]
fn none_is_empty_string() {
    assert_eq!(PolicyDetail::None.to_wire_string(), "");
}

#[test]
fn discard_reason_serializes_to_kind_and_code() {
    let d = PolicyDetail::DiscardReason(DiscardReason::PiiBlocked);
    assert_eq!(d.to_wire_string(), "discard:pii_blocked");
}

#[test]
fn redaction_tag_counts_emit_sorted_pairs() {
    let mut counts = BTreeMap::new();
    counts.insert(RedactionTag::Email, 2);
    counts.insert(RedactionTag::Ssn, 1);
    let d = PolicyDetail::RedactionTagCounts(counts);
    assert_eq!(d.to_wire_string(), "redacted:email=2,ssn=1");
}

#[test]
fn visibility_floor_serializes_to_floor_and_tier() {
    let d = PolicyDetail::VisibilityFloor(MemoryVisibility::Session);
    assert_eq!(d.to_wire_string(), "floor:session");
}

#[test]
fn scope_mismatch_emits_required_tier_only() {
    // Caller's actual scope is never echoed; only the *required* tier.
    let d = PolicyDetail::ScopeMismatch { required_tier: MemoryVisibility::Project };
    assert_eq!(d.to_wire_string(), "scope_required:project");
}

#[test]
fn error_code_uses_static_string() {
    let d = PolicyDetail::ErrorCode("wal_failure");
    assert_eq!(d.to_wire_string(), "error:wal_failure");
}
