//! `PolicyDetail` is body-free: every variant carries only metadata
//! (counts, codes, enum tags) and never raw bytes. The wire string
//! produced by `to_wire_string` is short and stable.

use std::collections::BTreeMap;

use cairn_core::domain::MemoryVisibility;
use cairn_core::domain::sharing::{SharingDecisionKind, SharingPolicyAction, SharingPolicySubject};
use cairn_core::pipeline::filter::{DiscardReason, RedactionTag};
use cairn_core::policy_trace::{PolicyDetail, PolicyErrorCode};
use cairn_core::rebac::{RebacAction, RebacDecisionKind};

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
    let d = PolicyDetail::ScopeMismatch {
        required_tier: MemoryVisibility::Project,
    };
    assert_eq!(d.to_wire_string(), "scope_required:project");
}

#[test]
fn rebac_detail_serializes_action_tier_and_reason() {
    let d = PolicyDetail::Rebac {
        action: RebacAction::Read,
        tier: MemoryVisibility::Project,
        reason: RebacDecisionKind::DeniedNoRelation,
    };
    assert_eq!(d.to_wire_string(), "rebac:read:project:no_relation");
}

#[test]
fn consent_receipt_detail_serializes_action_and_reason() {
    let d = PolicyDetail::Sharing {
        subject: SharingPolicySubject::ConsentReceipt,
        action: SharingPolicyAction::Promote,
        reason: SharingDecisionKind::TargetMismatch,
    };
    assert_eq!(d.to_wire_string(), "consent:promote:target_mismatch");
}

#[test]
fn share_link_detail_serializes_action_and_reason() {
    let d = PolicyDetail::Sharing {
        subject: SharingPolicySubject::ShareLink,
        action: SharingPolicyAction::Grant,
        reason: SharingDecisionKind::Revoked,
    };
    assert_eq!(d.to_wire_string(), "share_link:grant:revoked");
}

#[test]
fn error_code_uses_validated_static_code() {
    let d = PolicyDetail::ErrorCode(PolicyErrorCode::WAL_FAILURE);
    assert_eq!(d.to_wire_string(), "error:wal_failure");
}

#[test]
fn error_code_from_static_accepts_snake_case() {
    let c = PolicyErrorCode::from_static("custom_code_42");
    assert_eq!(c.as_str(), "custom_code_42");
}

#[test]
#[should_panic(expected = "[a-z][a-z0-9_]")]
fn error_code_rejects_free_text_with_spaces() {
    // A careless `&'static str` like `"raw body: hello world"` would
    // smuggle free text into `policy_trace.detail`. The validator must
    // panic to fail-close the body-free invariant (brief §14).
    let _ = PolicyErrorCode::from_static("raw body: hello world");
}

#[test]
#[should_panic(expected = "[a-z][a-z0-9_]")]
fn error_code_rejects_punctuation() {
    let _ = PolicyErrorCode::from_static("user-said-hi!");
}

#[test]
#[should_panic(expected = "[a-z][a-z0-9_]")]
fn error_code_rejects_uppercase() {
    let _ = PolicyErrorCode::from_static("WAL_FAILURE");
}

#[test]
#[should_panic(expected = "[a-z][a-z0-9_]")]
fn error_code_rejects_empty_string() {
    let _ = PolicyErrorCode::from_static("");
}

#[test]
fn error_code_is_valid_helper_matches_constructor() {
    assert!(PolicyErrorCode::is_valid("ok_code"));
    assert!(PolicyErrorCode::is_valid("a"));
    assert!(!PolicyErrorCode::is_valid(""));
    assert!(!PolicyErrorCode::is_valid("Bad"));
    assert!(!PolicyErrorCode::is_valid("has space"));
    assert!(!PolicyErrorCode::is_valid("9starts_with_digit"));
    assert!(!PolicyErrorCode::is_valid(&"a".repeat(65)));
}
