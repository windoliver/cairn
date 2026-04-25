//! Behavioural tests pinning down the wire contract of generated SDK types
//! that have rejected an adversarial review in the past:
//!
//! 1. Untagged unions (`SignedIntent`, `IngestArgs`) MUST enforce XOR groups
//!    at deserialise time, not only via an opt-in `validate()` call.
//! 2. The recursive filter enum MUST round-trip the IDL wire shape — operator
//!    arms serialise as `{"and": [...]}`, `{"or": [...]}`, `{"not": ...}` and
//!    the leaf arm is unreachable for those operators.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use cairn_core::generated::envelope::SignedIntent;
use cairn_core::generated::verbs::ingest::IngestArgs;
use cairn_core::generated::verbs::search::SearchArgsFilters;

// ── Finding 1: XOR enforced at Deserialize ───────────────────────────────────

#[test]
fn ingest_args_rejects_zero_xor_members_at_deserialize() {
    // No `body` / `file` / `url` — invalid per IDL.
    let json = serde_json::json!({ "kind": "note" });
    let err = serde_json::from_value::<IngestArgs>(json).unwrap_err();
    assert!(
        err.to_string().contains("exactly one of"),
        "expected XOR error, got: {err}"
    );
}

#[test]
fn ingest_args_rejects_two_xor_members_at_deserialize() {
    let json = serde_json::json!({
        "kind": "note",
        "body": "hello",
        "file": "/tmp/x.md",
    });
    let err = serde_json::from_value::<IngestArgs>(json).unwrap_err();
    assert!(
        err.to_string().contains("exactly one of"),
        "expected XOR error, got: {err}"
    );
}

#[test]
fn ingest_args_accepts_exactly_one_xor_member() {
    let json = serde_json::json!({ "kind": "note", "body": "hello" });
    let args: IngestArgs = serde_json::from_value(json).unwrap();
    assert_eq!(args.body.as_deref(), Some("hello"));
    // Hand-built validate() still works for callers that construct by hand.
    assert!(args.validate().is_ok());
}

#[test]
fn signed_intent_rejects_missing_sequence_and_challenge_at_deserialize() {
    // Stripped-down intent missing both `sequence` and `server_challenge`.
    let json = serde_json::json!({
        "chain_parents": [],
        "expires_at": "2026-01-01T00:00:00Z",
        "issued_at": "2026-01-01T00:00:00Z",
        "issuer": "agt:claude-code:opus-4-7:reviewer:v1",
        "key_version": 1_i64,
        "nonce": "AAAAAAAAAAAAAAAAAAAAAA",
        "operation_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
        "scope": {
            "entity": "alice",
            "tenant": "default",
            "tier": "private",
            "workspace": "default",
        },
        "signature": "00".repeat(64),
        "target_hash": "0".repeat(64),
    });
    let err = serde_json::from_value::<SignedIntent>(json).unwrap_err();
    assert!(
        err.to_string().contains("exactly one of"),
        "expected XOR error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_both_sequence_and_challenge_at_deserialize() {
    let json = serde_json::json!({
        "chain_parents": [],
        "expires_at": "2026-01-01T00:00:00Z",
        "issued_at": "2026-01-01T00:00:00Z",
        "issuer": "agt:claude-code:opus-4-7:reviewer:v1",
        "key_version": 1_i64,
        "nonce": "AAAAAAAAAAAAAAAAAAAAAA",
        "operation_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
        "scope": {
            "entity": "alice",
            "tenant": "default",
            "tier": "private",
            "workspace": "default",
        },
        "sequence": 1_u64,
        "server_challenge": "BBBBBBBBBBBBBBBBBBBBBB",
        "signature": "00".repeat(64),
        "target_hash": "0".repeat(64),
    });
    let err = serde_json::from_value::<SignedIntent>(json).unwrap_err();
    assert!(
        err.to_string().contains("exactly one of"),
        "expected XOR error, got: {err}"
    );
}

// ── Finding 2: Filter wire shape ─────────────────────────────────────────────

#[test]
fn filter_and_round_trips_with_and_key() {
    // Wire: {"and": [{...leaf...}, {...leaf...}]} — must deserialise to And,
    // not to Leaf. Leaf would be unreachable if it appeared first.
    let leaf = serde_json::json!({"field": "kind", "op": "eq", "value": "note"});
    let wire = serde_json::json!({"and": [leaf.clone(), leaf.clone()]});
    let parsed: SearchArgsFilters = serde_json::from_value(wire.clone()).unwrap();
    match &parsed {
        SearchArgsFilters::And { and } => assert_eq!(and.len(), 2),
        other => panic!("expected And, got {other:?}"),
    }
    // Serialise back and compare structurally.
    let round = serde_json::to_value(&parsed).unwrap();
    assert_eq!(round, wire);
}

#[test]
fn filter_or_round_trips_with_or_key() {
    let leaf = serde_json::json!({"field": "kind", "op": "eq", "value": "note"});
    let wire = serde_json::json!({"or": [leaf.clone()]});
    let parsed: SearchArgsFilters = serde_json::from_value(wire.clone()).unwrap();
    assert!(matches!(parsed, SearchArgsFilters::Or { .. }));
    let round = serde_json::to_value(&parsed).unwrap();
    assert_eq!(round, wire);
}

#[test]
fn filter_not_round_trips_with_not_key() {
    let leaf = serde_json::json!({"field": "kind", "op": "eq", "value": "note"});
    let wire = serde_json::json!({"not": leaf.clone()});
    let parsed: SearchArgsFilters = serde_json::from_value(wire.clone()).unwrap();
    assert!(matches!(parsed, SearchArgsFilters::Not { .. }));
    let round = serde_json::to_value(&parsed).unwrap();
    assert_eq!(round, wire);
}

#[test]
fn filter_leaf_round_trips_as_object() {
    // A bare leaf without and/or/not lands in the Leaf arm.
    let leaf = serde_json::json!({"field": "kind", "op": "eq", "value": "note"});
    let parsed: SearchArgsFilters = serde_json::from_value(leaf.clone()).unwrap();
    assert!(matches!(parsed, SearchArgsFilters::Leaf(_)));
    let round = serde_json::to_value(&parsed).unwrap();
    assert_eq!(round, leaf);
}
