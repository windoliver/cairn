//! Behavioural tests pinning down the wire contract of generated SDK types
//! that have rejected an adversarial review in the past:
//!
//! 1. Untagged unions (`SignedIntent`, `IngestArgs`) MUST enforce XOR groups
//!    at deserialise time, not only via an opt-in `validate()` call.
//! 2. The recursive filter enum MUST round-trip the IDL wire shape — operator
//!    arms serialise as `{"and": [...]}`, `{"or": [...]}`, `{"not": ...}` and
//!    the leaf arm is unreachable for those operators.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use cairn_core::generated::common::ScopeFilter;
use cairn_core::generated::envelope::{
    Request, RequestArgs, Response, ResponseData, RetrieveData, SignedIntent,
};
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
    let mut m = signed_intent_minimum();
    m.remove("sequence");
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("exactly one of"),
        "expected XOR error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_both_sequence_and_challenge_at_deserialize() {
    let mut m = signed_intent_minimum();
    m.insert(
        "server_challenge".into(),
        serde_json::json!("BBBBBBBBBBBBBBBBBBBBBA"),
    );
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("exactly one of"),
        "expected XOR error, got: {err}"
    );
}

// ── Finding F2: SignedIntent enforces field-level constraints at deserialize ─

fn signed_intent_minimum() -> serde_json::Map<String, serde_json::Value> {
    // Sequence-only XOR branch, valid baseline for the bespoke validators.
    let mut m = serde_json::Map::new();
    m.insert("chain_parents".into(), serde_json::json!([]));
    m.insert(
        "expires_at".into(),
        serde_json::json!("2026-01-01T00:00:00Z"),
    );
    m.insert(
        "issued_at".into(),
        serde_json::json!("2026-01-01T00:00:00Z"),
    );
    m.insert(
        "issuer".into(),
        serde_json::json!("agt:claude-code:opus-4-7:reviewer:v1"),
    );
    m.insert("key_version".into(), serde_json::json!(1_i64));
    m.insert("nonce".into(), serde_json::json!("AAAAAAAAAAAAAAAAAAAAAA"));
    m.insert(
        "operation_id".into(),
        serde_json::json!("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
    );
    m.insert(
        "scope".into(),
        serde_json::json!({
            "entity": "alice",
            "tenant": "default",
            "tier": "private",
            "workspace": "default",
        }),
    );
    m.insert("sequence".into(), serde_json::json!(1_u64));
    m.insert(
        "signature".into(),
        serde_json::json!(format!("ed25519:{}", "0".repeat(128))),
    );
    m.insert(
        "target_hash".into(),
        serde_json::json!(format!("sha256:{}", "0".repeat(64))),
    );
    m
}

#[test]
fn signed_intent_accepts_valid_minimum() {
    let parsed: SignedIntent =
        serde_json::from_value(serde_json::Value::Object(signed_intent_minimum())).unwrap();
    assert_eq!(parsed.sequence, Some(1));
}

#[test]
fn signed_intent_rejects_sequence_above_safe_integer_cap() {
    let mut m = signed_intent_minimum();
    // 2^53 = 9_007_199_254_740_992 — one above the cap.
    m.insert(
        "sequence".into(),
        serde_json::json!(9_007_199_254_740_992_u64),
    );
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("sequence"),
        "expected sequence-cap error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_key_version_zero() {
    let mut m = signed_intent_minimum();
    m.insert("key_version".into(), serde_json::json!(0_i64));
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("key_version"),
        "expected key_version error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_chain_parents_above_max() {
    let mut m = signed_intent_minimum();
    let parents: Vec<String> = (0..65)
        .map(|_| "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string())
        .collect();
    m.insert("chain_parents".into(), serde_json::json!(parents));
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("chain_parents") && err.to_string().contains("64"),
        "expected chain_parents-max error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_chain_parents_with_duplicates() {
    let mut m = signed_intent_minimum();
    m.insert(
        "chain_parents".into(),
        serde_json::json!(["01ARZ3NDEKTSV4RRFFQ69G5FAV", "01ARZ3NDEKTSV4RRFFQ69G5FAV",]),
    );
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("unique"),
        "expected uniqueness error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_malformed_operation_id() {
    let mut m = signed_intent_minimum();
    // Lowercase + wrong length — both fail the Crockford ULID check.
    m.insert("operation_id".into(), serde_json::json!("not-a-ulid"));
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("operation_id") && err.to_string().contains("ULID"),
        "expected ULID error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_malformed_chain_parent_ulid() {
    let mut m = signed_intent_minimum();
    m.insert(
        "chain_parents".into(),
        serde_json::json!(["not-a-ulid-of-length-26-chars"]),
    );
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("chain_parents") && err.to_string().contains("ULID"),
        "expected chain_parents ULID error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_ulid_with_disallowed_alphabet() {
    let mut m = signed_intent_minimum();
    // 26 chars but contains 'I' which is not in Crockford base32.
    m.insert(
        "operation_id".into(),
        serde_json::json!("01ARZ3NDEKTSV4RRFFQ69G5FAI"),
    );
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("ULID"),
        "expected ULID alphabet rejection, got: {err}"
    );
}

// ── F1 (round 5): SignedIntent pattern checks for crypto / identity payloads ─

#[test]
fn signed_intent_rejects_malformed_nonce_wrong_length() {
    let mut m = signed_intent_minimum();
    // 21 chars — neither 22 nor 24.
    m.insert("nonce".into(), serde_json::json!("AAAAAAAAAAAAAAAAAAAAA"));
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("nonce"),
        "expected nonce-shape error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_malformed_nonce_invalid_char() {
    let mut m = signed_intent_minimum();
    // `*` is not in the base64 alphabet.
    m.insert("nonce".into(), serde_json::json!("AAAAAAAAAAAAAAAAAAAA*A"));
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("nonce"),
        "expected nonce-shape error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_nonce_with_noncanonical_tail_char() {
    let mut m = signed_intent_minimum();
    // 22 chars but tail is `B` (not in [AQgw]) — non-canonical 16-byte encoding.
    m.insert("nonce".into(), serde_json::json!("AAAAAAAAAAAAAAAAAAAAAB"));
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("nonce"),
        "expected nonce-shape error, got: {err}"
    );
}

#[test]
fn signed_intent_accepts_padded_nonce() {
    let mut m = signed_intent_minimum();
    // 24-char form with `==` padding.
    m.insert(
        "nonce".into(),
        serde_json::json!("AAAAAAAAAAAAAAAAAAAAAA=="),
    );
    let parsed: SignedIntent =
        serde_json::from_value(serde_json::Value::Object(m)).expect("padded nonce should parse");
    assert_eq!(parsed.nonce.0, "AAAAAAAAAAAAAAAAAAAAAA==");
}

#[test]
fn signed_intent_rejects_malformed_server_challenge() {
    let mut m = signed_intent_minimum();
    m.remove("sequence");
    // Wrong-length challenge.
    m.insert(
        "server_challenge".into(),
        serde_json::json!("not-a-base64-nonce"),
    );
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("server_challenge"),
        "expected server_challenge-shape error, got: {err}"
    );
}

#[test]
fn signed_intent_accepts_valid_server_challenge() {
    let mut m = signed_intent_minimum();
    m.remove("sequence");
    m.insert(
        "server_challenge".into(),
        serde_json::json!("BBBBBBBBBBBBBBBBBBBBBA"),
    );
    let parsed: SignedIntent = serde_json::from_value(serde_json::Value::Object(m))
        .expect("valid server_challenge should parse");
    assert_eq!(
        parsed.server_challenge.as_ref().map(|n| n.0.as_str()),
        Some("BBBBBBBBBBBBBBBBBBBBBA")
    );
}

#[test]
fn signed_intent_rejects_signature_missing_prefix() {
    let mut m = signed_intent_minimum();
    // Bare hex without the `ed25519:` tag.
    m.insert("signature".into(), serde_json::json!("0".repeat(128)));
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("signature"),
        "expected signature-shape error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_signature_wrong_tail_length() {
    let mut m = signed_intent_minimum();
    m.insert(
        "signature".into(),
        serde_json::json!(format!("ed25519:{}", "0".repeat(64))),
    );
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("signature"),
        "expected signature-tail-length error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_signature_uppercase_hex() {
    let mut m = signed_intent_minimum();
    // Spec: 128 *lowercase* hex chars.
    m.insert(
        "signature".into(),
        serde_json::json!(format!("ed25519:{}", "A".repeat(128))),
    );
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("signature"),
        "expected signature-hex error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_target_hash_missing_prefix() {
    let mut m = signed_intent_minimum();
    m.insert("target_hash".into(), serde_json::json!("0".repeat(64)));
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("target_hash"),
        "expected target_hash-shape error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_target_hash_non_hex_tail() {
    let mut m = signed_intent_minimum();
    m.insert(
        "target_hash".into(),
        serde_json::json!(format!("sha256:{}", "z".repeat(64))),
    );
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("target_hash"),
        "expected target_hash-hex error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_issuer_with_unknown_prefix() {
    let mut m = signed_intent_minimum();
    m.insert("issuer".into(), serde_json::json!("xyz:alice"));
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("issuer"),
        "expected issuer-prefix error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_issuer_with_empty_body() {
    let mut m = signed_intent_minimum();
    m.insert("issuer".into(), serde_json::json!("agt:"));
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("issuer"),
        "expected issuer-empty-body error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_empty_scope_tenant() {
    let mut m = signed_intent_minimum();
    m.insert(
        "scope".into(),
        serde_json::json!({
            "entity": "alice",
            "tenant": "",
            "tier": "private",
            "workspace": "default",
        }),
    );
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("scope.tenant"),
        "expected scope.tenant-empty error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_empty_scope_workspace() {
    let mut m = signed_intent_minimum();
    m.insert(
        "scope".into(),
        serde_json::json!({
            "entity": "alice",
            "tenant": "default",
            "tier": "private",
            "workspace": "",
        }),
    );
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("scope.workspace"),
        "expected scope.workspace-empty error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_empty_scope_entity() {
    let mut m = signed_intent_minimum();
    m.insert(
        "scope".into(),
        serde_json::json!({
            "entity": "",
            "tenant": "default",
            "tier": "private",
            "workspace": "default",
        }),
    );
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("scope.entity"),
        "expected scope.entity-empty error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_malformed_issued_at() {
    let mut m = signed_intent_minimum();
    m.insert("issued_at".into(), serde_json::json!("yesterday"));
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("issued_at"),
        "expected issued_at-shape error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_malformed_expires_at() {
    let mut m = signed_intent_minimum();
    m.insert("expires_at".into(), serde_json::json!("2026-01-01"));
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("expires_at"),
        "expected expires_at-shape error, got: {err}"
    );
}

#[test]
fn signed_intent_accepts_offset_datetime() {
    let mut m = signed_intent_minimum();
    m.insert(
        "issued_at".into(),
        serde_json::json!("2026-01-01T00:00:00+00:00"),
    );
    m.insert(
        "expires_at".into(),
        serde_json::json!("2026-01-01T00:00:00.123-08:00"),
    );
    let parsed: SignedIntent = serde_json::from_value(serde_json::Value::Object(m))
        .expect("offset / fractional date-time should parse");
    assert_eq!(parsed.issued_at, "2026-01-01T00:00:00+00:00");
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

// ── Finding A: Request envelope dispatches args by verb ──────────────────────

fn signed_intent_json() -> serde_json::Value {
    // A minimum-viable SignedIntent payload (sequence-only XOR branch).
    serde_json::Value::Object(signed_intent_minimum())
}

#[test]
fn request_search_dispatches_search_args() {
    let json = serde_json::json!({
        "contract": "cairn.mcp.v1",
        "verb": "search",
        "signed_intent": signed_intent_json(),
        "args": { "mode": "keyword", "query": "hello" },
    });
    let req: Request = serde_json::from_value(json).unwrap();
    match req.args {
        RequestArgs::Search(_) => {}
        other => panic!("expected Search variant, got {other:?}"),
    }
}

#[test]
fn request_rejects_args_shape_mismatched_to_verb() {
    // verb=search but args carries the ingest shape — must fail because the
    // typed dispatch tries to deserialise SearchArgs and finds wrong fields.
    let json = serde_json::json!({
        "contract": "cairn.mcp.v1",
        "verb": "search",
        "signed_intent": signed_intent_json(),
        "args": { "kind": "note", "body": "hello" },
    });
    let err = serde_json::from_value::<Request>(json).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("missing field") || msg.contains("unknown field"),
        "expected dispatched-args error, got: {msg}"
    );
}

#[test]
fn request_rejects_wrong_contract_literal() {
    let json = serde_json::json!({
        "contract": "cairn.mcp.v2",
        "verb": "search",
        "signed_intent": signed_intent_json(),
        "args": { "mode": "keyword", "query": "hello" },
    });
    let err = serde_json::from_value::<Request>(json).unwrap_err();
    assert!(
        err.to_string().contains("contract"),
        "expected contract mismatch error, got: {err}"
    );
}

#[test]
fn request_ingest_dispatches_and_round_trips_args() {
    let json = serde_json::json!({
        "contract": "cairn.mcp.v1",
        "verb": "ingest",
        "signed_intent": signed_intent_json(),
        "args": { "kind": "note", "body": "hello" },
    });
    let req: Request = serde_json::from_value(json).unwrap();
    match &req.args {
        RequestArgs::Ingest(args) => assert_eq!(args.body.as_deref(), Some("hello")),
        other => panic!("expected Ingest variant, got {other:?}"),
    }
}

// ── Finding B: ScopeFilter rejects empty / unknown-only payloads ─────────────

#[test]
fn scope_filter_rejects_empty_object() {
    // No predicates at all — must fail per anyOf-required.
    let err = serde_json::from_value::<ScopeFilter>(serde_json::json!({})).unwrap_err();
    assert!(
        err.to_string().contains("at least one of"),
        "expected anyOf-required error, got: {err}"
    );
}

#[test]
fn scope_filter_accepts_single_predicate() {
    let parsed: ScopeFilter = serde_json::from_value(serde_json::json!({ "user": "u1" })).unwrap();
    assert_eq!(parsed.user.as_deref(), Some("u1"));
}

// ── Finding F3: Response envelope status/data/target invariants ──────────────

fn response_base() -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert("contract".into(), serde_json::json!("cairn.mcp.v1"));
    m.insert(
        "operation_id".into(),
        serde_json::json!("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
    );
    m.insert("policy_trace".into(), serde_json::json!([]));
    m
}

#[test]
fn response_committed_search_round_trips() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("search"));
    m.insert("status".into(), serde_json::json!("committed"));
    m.insert(
        "data".into(),
        serde_json::json!({"hits": [], "next_cursor": null}),
    );
    let parsed: Response = serde_json::from_value(serde_json::Value::Object(m)).unwrap();
    assert!(matches!(parsed.data, Some(ResponseData::Search(_))));
}

#[test]
fn response_committed_without_data_is_rejected() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("search"));
    m.insert("status".into(), serde_json::json!("committed"));
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("committed requires data"),
        "expected committed-requires-data error, got: {err}"
    );
}

#[test]
fn response_rejected_with_data_is_rejected() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("search"));
    m.insert("status".into(), serde_json::json!("rejected"));
    m.insert("error".into(), serde_json::json!({"code": "InvalidArgs"}));
    m.insert("data".into(), serde_json::json!({"hits": []}));
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("forbids data"),
        "expected rejected-forbids-data error, got: {err}"
    );
}

#[test]
fn response_rejected_without_error_is_rejected() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("search"));
    m.insert("status".into(), serde_json::json!("rejected"));
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("requires error"),
        "expected rejected-requires-error error, got: {err}"
    );
}

#[test]
fn response_aborted_with_data_is_rejected() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("search"));
    m.insert("status".into(), serde_json::json!("aborted"));
    m.insert("error".into(), serde_json::json!({"code": "Internal"}));
    m.insert("data".into(), serde_json::json!({"hits": []}));
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("forbids data"),
        "expected aborted-forbids-data error, got: {err}"
    );
}

#[test]
fn response_retrieve_committed_without_target_is_rejected() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("retrieve"));
    m.insert("status".into(), serde_json::json!("committed"));
    m.insert(
        "data".into(),
        serde_json::json!({"record_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV", "kind": "note"}),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("retrieve") && err.to_string().contains("target"),
        "expected retrieve-requires-target error, got: {err}"
    );
}

#[test]
fn response_non_retrieve_with_target_is_rejected() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("search"));
    m.insert("status".into(), serde_json::json!("committed"));
    m.insert("data".into(), serde_json::json!({"hits": []}));
    m.insert("target".into(), serde_json::json!("record"));
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("retrieve-only") || err.to_string().contains("target"),
        "expected target-forbidden error, got: {err}"
    );
}

#[test]
fn response_retrieve_committed_with_target_round_trips() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("retrieve"));
    m.insert("status".into(), serde_json::json!("committed"));
    m.insert("target".into(), serde_json::json!("record"));
    m.insert(
        "data".into(),
        serde_json::json!({"record_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV", "kind": "note"}),
    );
    let parsed: Response = serde_json::from_value(serde_json::Value::Object(m)).unwrap();
    match parsed.data {
        Some(ResponseData::Retrieve(RetrieveData::Record(rec))) => {
            assert_eq!(rec.record_id.0, "01ARZ3NDEKTSV4RRFFQ69G5FAV");
            assert_eq!(rec.kind, "note");
        }
        other => panic!("expected Retrieve::Record variant, got {other:?}"),
    }
}

// ── F2 (round 5): retrieve sub-dispatch by target + UnknownVerb invariant ───

#[test]
fn response_retrieve_session_target_round_trips() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("retrieve"));
    m.insert("status".into(), serde_json::json!("committed"));
    m.insert("target".into(), serde_json::json!("session"));
    m.insert(
        "data".into(),
        serde_json::json!({"session_id": "s1", "items": []}),
    );
    let parsed: Response = serde_json::from_value(serde_json::Value::Object(m)).unwrap();
    match parsed.data {
        Some(ResponseData::Retrieve(RetrieveData::Session(s))) => {
            assert_eq!(s.session_id, "s1");
            assert!(s.items.is_empty());
        }
        other => panic!("expected Retrieve::Session variant, got {other:?}"),
    }
}

#[test]
fn response_retrieve_target_record_with_session_data_is_rejected() {
    // target=record but the data is session-shaped — DataRecord deserialize
    // must fail on the missing record_id / kind fields.
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("retrieve"));
    m.insert("status".into(), serde_json::json!("committed"));
    m.insert("target".into(), serde_json::json!("record"));
    m.insert(
        "data".into(),
        serde_json::json!({"session_id": "s1", "items": []}),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("missing field") || msg.contains("unknown field"),
        "expected target-mismatched-data error, got: {msg}"
    );
}

#[test]
fn response_retrieve_target_session_with_record_data_is_rejected() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("retrieve"));
    m.insert("status".into(), serde_json::json!("committed"));
    m.insert("target".into(), serde_json::json!("session"));
    m.insert(
        "data".into(),
        serde_json::json!({"record_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV", "kind": "note"}),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("missing field") || msg.contains("unknown field"),
        "expected target-mismatched-data error, got: {msg}"
    );
}

#[test]
fn response_unknown_verb_committed_is_rejected() {
    // verb=unknown is rejected-only per the IDL bidirectional binding.
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("unknown"));
    m.insert("status".into(), serde_json::json!("committed"));
    m.insert("data".into(), serde_json::json!({"hits": []}));
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("verb=unknown") || msg.contains("requires status=rejected"),
        "expected unknown-verb-needs-rejected error, got: {msg}"
    );
}

#[test]
fn response_unknown_verb_with_rejected_unknownverb_code_round_trips() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("unknown"));
    m.insert("status".into(), serde_json::json!("rejected"));
    m.insert(
        "error".into(),
        serde_json::json!({"code": "UnknownVerb", "message": "boom", "data": {"verb": "xyz"}}),
    );
    let parsed: Response = serde_json::from_value(serde_json::Value::Object(m)).unwrap();
    assert!(parsed.data.is_none());
}

#[test]
fn response_unknown_verb_with_other_error_code_is_rejected() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("unknown"));
    m.insert("status".into(), serde_json::json!("rejected"));
    m.insert(
        "error".into(),
        serde_json::json!({"code": "InvalidArgs", "message": "boom"}),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("verb=unknown") && msg.contains("UnknownVerb"),
        "expected unknown-verb-needs-UnknownVerb-code error, got: {msg}"
    );
}

#[test]
fn response_search_with_unknownverb_code_is_rejected() {
    // Bidirectional half — UnknownVerb code is paired with verb=unknown only.
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("search"));
    m.insert("status".into(), serde_json::json!("rejected"));
    m.insert(
        "error".into(),
        serde_json::json!({"code": "UnknownVerb", "message": "boom", "data": {"verb": "xyz"}}),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("UnknownVerb") && msg.contains("verb=unknown"),
        "expected UnknownVerb-paired-with-unknown error, got: {msg}"
    );
}

#[test]
fn response_committed_data_dispatched_by_verb() {
    // Verb=ingest with search-shaped data → IngestData deserialize fails.
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("ingest"));
    m.insert("status".into(), serde_json::json!("committed"));
    m.insert("data".into(), serde_json::json!({"hits": []}));
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("missing field") || msg.contains("unknown field"),
        "expected dispatched-data error, got: {msg}"
    );
}

#[test]
fn response_rejects_wrong_contract() {
    let mut m = response_base();
    m.insert("contract".into(), serde_json::json!("cairn.mcp.v2"));
    m.insert("verb".into(), serde_json::json!("search"));
    m.insert("status".into(), serde_json::json!("committed"));
    m.insert("data".into(), serde_json::json!({"hits": []}));
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    assert!(err.to_string().contains("contract"));
}

// ── Finding F4: ScopeFilter rejects empty values per IDL minLength/minItems ──

#[test]
fn scope_filter_rejects_empty_string_user() {
    let err = serde_json::from_value::<ScopeFilter>(serde_json::json!({ "user": "" })).unwrap_err();
    assert!(
        err.to_string().contains("user") && err.to_string().contains("empty"),
        "expected user-empty error, got: {err}"
    );
}

#[test]
fn scope_filter_rejects_empty_string_session_id() {
    let err =
        serde_json::from_value::<ScopeFilter>(serde_json::json!({ "session_id": "" })).unwrap_err();
    assert!(
        err.to_string().contains("session_id"),
        "expected session_id-empty error, got: {err}"
    );
}

#[test]
fn scope_filter_rejects_empty_tags_array() {
    let err = serde_json::from_value::<ScopeFilter>(serde_json::json!({ "tags": [] })).unwrap_err();
    assert!(
        err.to_string().contains("tags"),
        "expected tags-empty error, got: {err}"
    );
}

#[test]
fn scope_filter_rejects_empty_kind_array() {
    let err = serde_json::from_value::<ScopeFilter>(serde_json::json!({ "kind": [] })).unwrap_err();
    assert!(
        err.to_string().contains("kind"),
        "expected kind-empty error, got: {err}"
    );
}

#[test]
fn scope_filter_rejects_empty_record_ids_array() {
    let err =
        serde_json::from_value::<ScopeFilter>(serde_json::json!({ "record_ids": [] })).unwrap_err();
    assert!(
        err.to_string().contains("record_ids"),
        "expected record_ids-empty error, got: {err}"
    );
}

#[test]
fn scope_filter_rejects_tags_with_empty_string_item() {
    let err =
        serde_json::from_value::<ScopeFilter>(serde_json::json!({ "tags": [""] })).unwrap_err();
    assert!(
        err.to_string().contains("tags") && err.to_string().contains("empty"),
        "expected tags-item-empty error, got: {err}"
    );
}

#[test]
fn scope_filter_accepts_non_empty_tags() {
    let parsed: ScopeFilter = serde_json::from_value(serde_json::json!({ "tags": ["x"] })).unwrap();
    assert_eq!(parsed.tags.as_deref(), Some(&["x".to_string()][..]));
}

#[test]
fn scope_filter_accepts_non_empty_user() {
    let parsed: ScopeFilter = serde_json::from_value(serde_json::json!({ "user": "u1" })).unwrap();
    assert_eq!(parsed.user.as_deref(), Some("u1"));
}

#[test]
fn scope_filter_rejects_unknown_only_keys() {
    // deny_unknown_fields covers this — keep the assertion tight regardless.
    let err =
        serde_json::from_value::<ScopeFilter>(serde_json::json!({ "bogus": "x" })).unwrap_err();
    assert!(
        err.to_string().contains("unknown field") || err.to_string().contains("at least one"),
        "expected unknown-field rejection, got: {err}"
    );
}
