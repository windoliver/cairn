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
use cairn_core::generated::verbs::forget::ForgetArgs;
use cairn_core::generated::verbs::ingest::IngestArgs;
use cairn_core::generated::verbs::retrieve::RetrieveArgs;
use cairn_core::generated::verbs::search::{SearchArgs, SearchArgsFilters};

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
    // The check now fires at the Ulid newtype's hand-rolled Deserialize,
    // so the error message no longer carries the field name; matching on
    // "ULID" alone is enough to confirm the right validator caught it.
    m.insert("operation_id".into(), serde_json::json!("not-a-ulid"));
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("ULID"),
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
    // Same as operation_id — the Ulid newtype rejects the entry at its
    // own deserialize layer; field name is no longer in the error.
    assert!(
        err.to_string().contains("ULID"),
        "expected ULID error, got: {err}"
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
    let msg = err.to_string();
    // After F2 the Nonce16Base64 newtype's hand-rolled Deserialize fires
    // first; the SignedIntent extra-checks are the second line of defense
    // for fields not lifted to a typed newtype. Either signal is acceptable.
    assert!(
        msg.contains("nonce") || msg.contains("Nonce16Base64"),
        "expected nonce-shape error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_malformed_nonce_invalid_char() {
    let mut m = signed_intent_minimum();
    // `*` is not in the base64 alphabet.
    m.insert("nonce".into(), serde_json::json!("AAAAAAAAAAAAAAAAAAAA*A"));
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("nonce") || msg.contains("Nonce16Base64"),
        "expected nonce-shape error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_nonce_with_noncanonical_tail_char() {
    let mut m = signed_intent_minimum();
    // 22 chars but tail is `B` (not in [AQgw]) — non-canonical 16-byte encoding.
    m.insert("nonce".into(), serde_json::json!("AAAAAAAAAAAAAAAAAAAAAB"));
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("nonce") || msg.contains("Nonce16Base64"),
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
    let msg = err.to_string();
    // The newtype primitive Deserialize identifies the violated shape;
    // SignedIntent's bespoke field check tags the field name. After F2
    // the primitive check runs first.
    assert!(
        msg.contains("server_challenge") || msg.contains("Nonce16Base64"),
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
    let msg = err.to_string();
    // After F2 the Ed25519Signature primitive's hand-rolled Deserialize
    // fires first; the SignedIntent bespoke check is the second line of
    // defense.
    assert!(
        msg.contains("signature") || msg.contains("Ed25519Signature"),
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
    let msg = err.to_string();
    assert!(
        msg.contains("signature") || msg.contains("Ed25519Signature"),
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
    let msg = err.to_string();
    assert!(
        msg.contains("signature") || msg.contains("Ed25519Signature"),
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
    let msg = err.to_string();
    // After F2 the Identity primitive's hand-rolled Deserialize fires
    // first; the SignedIntent bespoke check is the second line of defense.
    assert!(
        msg.contains("issuer") || msg.contains("Identity"),
        "expected issuer-prefix error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_issuer_with_empty_body() {
    let mut m = signed_intent_minimum();
    m.insert("issuer".into(), serde_json::json!("agt:"));
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("issuer") || msg.contains("Identity"),
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

// ── F1 (round 6): SearchArgs limit + filter depth/fanout/minItems ────────────

fn search_args_minimal() -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert("mode".into(), serde_json::json!("keyword"));
    m.insert("query".into(), serde_json::json!("hello"));
    m
}

#[test]
fn search_args_rejects_limit_zero() {
    let mut m = search_args_minimal();
    m.insert("limit".into(), serde_json::json!(0_i64));
    let err = serde_json::from_value::<SearchArgs>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("limit"),
        "expected limit-out-of-range error, got: {err}"
    );
}

#[test]
fn search_args_rejects_limit_above_max() {
    let mut m = search_args_minimal();
    m.insert("limit".into(), serde_json::json!(1001_i64));
    let err = serde_json::from_value::<SearchArgs>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("limit"),
        "expected limit-out-of-range error, got: {err}"
    );
}

#[test]
fn search_args_accepts_limit_in_range() {
    let mut m = search_args_minimal();
    m.insert("limit".into(), serde_json::json!(50_i64));
    let parsed: SearchArgs = serde_json::from_value(serde_json::Value::Object(m)).unwrap();
    assert_eq!(parsed.limit, Some(50));
}

#[test]
fn search_args_rejects_empty_query() {
    let mut m = search_args_minimal();
    m.insert("query".into(), serde_json::json!(""));
    let err = serde_json::from_value::<SearchArgs>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("query"),
        "expected empty-query error, got: {err}"
    );
}

#[test]
fn filter_rejects_empty_and() {
    let err =
        serde_json::from_value::<SearchArgsFilters>(serde_json::json!({"and": []})).unwrap_err();
    assert!(
        err.to_string().contains("and") && err.to_string().contains("at least one"),
        "expected empty-and error, got: {err}"
    );
}

#[test]
fn filter_rejects_empty_or() {
    let err =
        serde_json::from_value::<SearchArgsFilters>(serde_json::json!({"or": []})).unwrap_err();
    assert!(
        err.to_string().contains("or") && err.to_string().contains("at least one"),
        "expected empty-or error, got: {err}"
    );
}

#[test]
fn filter_rejects_depth_above_max() {
    // Build nested `not` chain of depth 9 — IDL caps at 8.
    let leaf = serde_json::json!({"field": "kind", "op": "eq", "value": "note"});
    let mut node = leaf;
    for _ in 0..9 {
        node = serde_json::json!({"not": node});
    }
    let err = serde_json::from_value::<SearchArgsFilters>(node).unwrap_err();
    assert!(
        err.to_string().contains("depth"),
        "expected max-depth error, got: {err}"
    );
}

#[test]
fn filter_accepts_depth_at_max() {
    // Depth-8 nested `not` chain — exactly at the cap.
    let leaf = serde_json::json!({"field": "kind", "op": "eq", "value": "note"});
    let mut node = leaf;
    for _ in 0..8 {
        node = serde_json::json!({"not": node});
    }
    let parsed: SearchArgsFilters = serde_json::from_value(node).unwrap();
    assert!(matches!(parsed, SearchArgsFilters::Not { .. }));
}

#[test]
fn filter_rejects_fanout_above_max() {
    // and-array with 33 leaf items — IDL caps fanout at 32.
    let leaf = serde_json::json!({"field": "kind", "op": "eq", "value": "note"});
    let items: Vec<_> = (0..33).map(|_| leaf.clone()).collect();
    let err =
        serde_json::from_value::<SearchArgsFilters>(serde_json::json!({"and": items})).unwrap_err();
    assert!(
        err.to_string().contains("fanout"),
        "expected fanout-exceeded error, got: {err}"
    );
}

#[test]
fn filter_accepts_fanout_at_max() {
    let leaf = serde_json::json!({"field": "kind", "op": "eq", "value": "note"});
    let items: Vec<_> = (0..32).map(|_| leaf.clone()).collect();
    let parsed: SearchArgsFilters =
        serde_json::from_value(serde_json::json!({"and": items})).unwrap();
    if let SearchArgsFilters::And { and } = parsed {
        assert_eq!(and.len(), 32);
    } else {
        panic!("expected And variant");
    }
}

#[test]
fn filter_rejects_huge_fanout_without_recursing_into_children() {
    // 1024-element `and` array with malformed children. The new parser
    // short-circuits at `arr.len() > max_fanout` *before* allocating the
    // child Vec or visiting any child, so the rejection message is the
    // fanout error — not a per-child shape error. This is the F1
    // contract: fanout/depth checks happen before allocation.
    let bogus_child = serde_json::json!({"this": "is not a valid leaf"});
    let items: Vec<_> = (0..1024).map(|_| bogus_child.clone()).collect();
    let err =
        serde_json::from_value::<SearchArgsFilters>(serde_json::json!({"and": items})).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("fanout"),
        "expected fanout rejection (short-circuit before child parse), got: {err}"
    );
}

#[test]
fn filter_rejects_huge_depth_without_traversing_full_chain() {
    // Build a 1024-deep `not` chain wrapping a malformed leaf. The depth
    // budget is decremented on each operator descent and rejection
    // happens at the cap (8) — the parser does not walk the full 1024-
    // frame chain before rejecting. The malformed leaf is therefore
    // never reached / allocated.
    let mut node = serde_json::json!({"this": "is not a valid leaf"});
    for _ in 0..1024 {
        node = serde_json::json!({"not": node});
    }
    let err = serde_json::from_value::<SearchArgsFilters>(node).unwrap_err();
    assert!(
        err.to_string().contains("depth"),
        "expected depth rejection (short-circuit before full traversal), got: {err}"
    );
}

// ── F1 (round 6): RetrieveArgs per-variant constraints ───────────────────────

#[test]
fn retrieve_session_rejects_limit_zero() {
    let json = serde_json::json!({
        "target": "session",
        "session_id": "s1",
        "limit": 0,
    });
    let err = serde_json::from_value::<RetrieveArgs>(json).unwrap_err();
    assert!(
        err.to_string().contains("limit"),
        "expected limit-out-of-range error, got: {err}"
    );
}

#[test]
fn retrieve_session_rejects_limit_above_max() {
    let json = serde_json::json!({
        "target": "session",
        "session_id": "s1",
        "limit": 10001,
    });
    let err = serde_json::from_value::<RetrieveArgs>(json).unwrap_err();
    assert!(
        err.to_string().contains("limit"),
        "expected limit-out-of-range error, got: {err}"
    );
}

#[test]
fn retrieve_session_rejects_empty_include() {
    let json = serde_json::json!({
        "target": "session",
        "session_id": "s1",
        "include": [],
    });
    let err = serde_json::from_value::<RetrieveArgs>(json).unwrap_err();
    assert!(
        err.to_string().contains("include"),
        "expected empty-include error, got: {err}"
    );
}

#[test]
fn retrieve_session_rejects_duplicate_include() {
    let json = serde_json::json!({
        "target": "session",
        "session_id": "s1",
        "include": ["tool_calls", "tool_calls"],
    });
    let err = serde_json::from_value::<RetrieveArgs>(json).unwrap_err();
    assert!(
        err.to_string().contains("unique"),
        "expected uniqueItems error, got: {err}"
    );
}

#[test]
fn retrieve_session_accepts_valid() {
    let json = serde_json::json!({
        "target": "session",
        "session_id": "s1",
        "limit": 100,
        "include": ["tool_calls", "reasoning"],
    });
    let parsed: RetrieveArgs = serde_json::from_value(json).unwrap();
    assert!(matches!(parsed, RetrieveArgs::Session { .. }));
}

#[test]
fn retrieve_folder_rejects_depth_above_max() {
    let json = serde_json::json!({
        "target": "folder",
        "path": "/x",
        "depth": 17,
    });
    let err = serde_json::from_value::<RetrieveArgs>(json).unwrap_err();
    assert!(
        err.to_string().contains("depth"),
        "expected depth-out-of-range error, got: {err}"
    );
}

#[test]
fn retrieve_folder_accepts_depth_at_max() {
    let json = serde_json::json!({
        "target": "folder",
        "path": "/x",
        "depth": 16,
    });
    let parsed: RetrieveArgs = serde_json::from_value(json).unwrap();
    assert!(matches!(parsed, RetrieveArgs::Folder { .. }));
}

#[test]
fn retrieve_profile_rejects_no_subject() {
    let json = serde_json::json!({"target": "profile"});
    let err = serde_json::from_value::<RetrieveArgs>(json).unwrap_err();
    assert!(
        err.to_string().contains("at least one"),
        "expected anyOf error, got: {err}"
    );
}

#[test]
fn retrieve_scope_rejects_long_cursor() {
    let json = serde_json::json!({
        "target": "scope",
        "scope": {"user": "u1"},
        "cursor": "x".repeat(513),
    });
    let err = serde_json::from_value::<RetrieveArgs>(json).unwrap_err();
    assert!(
        err.to_string().contains("cursor"),
        "expected cursor-too-long error, got: {err}"
    );
}

// ── F2 (round 6): UnknownVerb error must carry message + data.verb ───────────

#[test]
fn response_unknown_verb_without_message_is_rejected() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("unknown"));
    m.insert("status".into(), serde_json::json!("rejected"));
    m.insert(
        "error".into(),
        serde_json::json!({"code": "UnknownVerb", "data": {"verb": "xyz"}}),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("message"),
        "expected non-empty-message error, got: {err}"
    );
}

#[test]
fn response_unknown_verb_with_empty_message_is_rejected() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("unknown"));
    m.insert("status".into(), serde_json::json!("rejected"));
    m.insert(
        "error".into(),
        serde_json::json!({"code": "UnknownVerb", "message": "", "data": {"verb": "xyz"}}),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("message"),
        "expected non-empty-message error, got: {err}"
    );
}

#[test]
fn response_unknown_verb_without_data_verb_is_rejected() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("unknown"));
    m.insert("status".into(), serde_json::json!("rejected"));
    m.insert(
        "error".into(),
        serde_json::json!({"code": "UnknownVerb", "message": "boom"}),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("data.verb") || msg.contains("data object"),
        "expected non-empty-data.verb error, got: {err}"
    );
}

#[test]
fn response_unknown_verb_with_empty_data_verb_is_rejected() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("unknown"));
    m.insert("status".into(), serde_json::json!("rejected"));
    m.insert(
        "error".into(),
        serde_json::json!({"code": "UnknownVerb", "message": "boom", "data": {"verb": ""}}),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("data.verb") || err.to_string().contains("verb"),
        "expected non-empty-data.verb error, got: {err}"
    );
}

#[test]
fn response_unknown_verb_with_message_and_data_verb_round_trips() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("unknown"));
    m.insert("status".into(), serde_json::json!("rejected"));
    m.insert(
        "error".into(),
        serde_json::json!({"code": "UnknownVerb", "message": "unrecognised verb", "data": {"verb": "synthesise"}}),
    );
    let parsed: Response = serde_json::from_value(serde_json::Value::Object(m)).unwrap();
    assert!(parsed.data.is_none());
}

// ── F3 (round 6): RFC-3339 field-range checks ────────────────────────────────

#[test]
fn signed_intent_rejects_out_of_range_month() {
    let mut m = signed_intent_minimum();
    m.insert(
        "issued_at".into(),
        serde_json::json!("2026-13-15T12:00:00Z"),
    );
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("issued_at"),
        "expected issued_at error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_out_of_range_hour() {
    let mut m = signed_intent_minimum();
    m.insert(
        "issued_at".into(),
        serde_json::json!("2026-04-25T25:00:00Z"),
    );
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("issued_at"),
        "expected issued_at error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_garbage_field_ranges() {
    let mut m = signed_intent_minimum();
    m.insert(
        "issued_at".into(),
        serde_json::json!("2026-99-99T99:99:99+99:99"),
    );
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("issued_at"),
        "expected issued_at error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_offset_hour_above_max() {
    let mut m = signed_intent_minimum();
    m.insert(
        "issued_at".into(),
        serde_json::json!("2026-04-25T12:00:00+24:00"),
    );
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("issued_at"),
        "expected issued_at error, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_minute_above_max() {
    let mut m = signed_intent_minimum();
    m.insert(
        "issued_at".into(),
        serde_json::json!("2026-04-25T12:60:00Z"),
    );
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("issued_at"),
        "expected issued_at error, got: {err}"
    );
}

#[test]
fn signed_intent_accepts_fractional_offset_datetime() {
    let mut m = signed_intent_minimum();
    m.insert(
        "issued_at".into(),
        serde_json::json!("2026-04-25T12:00:00.123456789+05:30"),
    );
    let parsed: SignedIntent = serde_json::from_value(serde_json::Value::Object(m)).unwrap();
    assert_eq!(parsed.issued_at, "2026-04-25T12:00:00.123456789+05:30");
}

#[test]
fn signed_intent_accepts_leap_second() {
    // RFC-3339 §5.6 allows seconds=60.
    let mut m = signed_intent_minimum();
    m.insert(
        "issued_at".into(),
        serde_json::json!("2026-12-31T23:59:60Z"),
    );
    let parsed: SignedIntent = serde_json::from_value(serde_json::Value::Object(m)).unwrap();
    assert_eq!(parsed.issued_at, "2026-12-31T23:59:60Z");
}

// ── F1 (round 7): Tagged-union variants reject cross-variant / unknown keys ──

#[test]
fn retrieve_record_rejects_cross_variant_scope_field() {
    // `scope` belongs to ArgsScope; it must not slip through on a record-target
    // payload (where the IDL marks ArgsRecord additionalProperties: false).
    let json = serde_json::json!({
        "target": "record",
        "id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
        "scope": {"user": "u1"},
    });
    let err = serde_json::from_value::<RetrieveArgs>(json).unwrap_err();
    assert!(
        err.to_string().contains("unknown field") || err.to_string().contains("scope"),
        "expected cross-variant-field rejection, got: {err}"
    );
}

#[test]
fn retrieve_record_rejects_arbitrary_unknown_key() {
    let json = serde_json::json!({
        "target": "record",
        "id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
        "unknown": 1,
    });
    let err = serde_json::from_value::<RetrieveArgs>(json).unwrap_err();
    assert!(
        err.to_string().contains("unknown field") || err.to_string().contains("unknown"),
        "expected unknown-field rejection, got: {err}"
    );
}

#[test]
fn retrieve_session_rejects_cross_variant_path_field() {
    let json = serde_json::json!({
        "target": "session",
        "session_id": "s1",
        "path": "/x",
    });
    let err = serde_json::from_value::<RetrieveArgs>(json).unwrap_err();
    assert!(
        err.to_string().contains("unknown field") || err.to_string().contains("path"),
        "expected cross-variant-field rejection, got: {err}"
    );
}

#[test]
fn forget_record_rejects_cross_variant_session_id_field() {
    let json = serde_json::json!({
        "mode": "record",
        "record_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
        "session_id": "x",
    });
    let err = serde_json::from_value::<ForgetArgs>(json).unwrap_err();
    assert!(
        err.to_string().contains("unknown field") || err.to_string().contains("session_id"),
        "expected cross-variant-field rejection, got: {err}"
    );
}

#[test]
fn forget_session_rejects_cross_variant_scope_field() {
    let json = serde_json::json!({
        "mode": "session",
        "session_id": "s1",
        "scope": {"user": "u1"},
    });
    let err = serde_json::from_value::<ForgetArgs>(json).unwrap_err();
    assert!(
        err.to_string().contains("unknown field") || err.to_string().contains("scope"),
        "expected cross-variant-field rejection, got: {err}"
    );
}

#[test]
fn forget_record_accepts_canonical_payload() {
    let json = serde_json::json!({
        "mode": "record",
        "record_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
    });
    let parsed: ForgetArgs = serde_json::from_value(json).unwrap();
    assert!(matches!(parsed, ForgetArgs::Record { .. }));
}

// ── F2 (round 7): filter leaf wire shape ─────────────────────────────────────

#[test]
fn filter_rejects_empty_object_leaf() {
    // The leaf must be a closed `{field, op, value}` object — `{}` has none of
    // those required fields and previously slipped through as `Leaf(Value)`.
    let err =
        serde_json::from_value::<SearchArgsFilters>(serde_json::json!({"and": [{}]})).unwrap_err();
    assert!(
        err.to_string().contains("filter leaf"),
        "expected leaf-shape error, got: {err}"
    );
}

#[test]
fn filter_rejects_null_leaf() {
    let err = serde_json::from_value::<SearchArgsFilters>(serde_json::json!({"and": [null]}))
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("filter leaf") || msg.contains("did not match") || msg.contains("JSON object"),
        "expected null-leaf rejection, got: {err}"
    );
}

#[test]
fn filter_rejects_leaf_with_empty_field() {
    let leaf = serde_json::json!({"field": "", "op": "eq", "value": 1});
    let err = serde_json::from_value::<SearchArgsFilters>(serde_json::json!({"and": [leaf]}))
        .unwrap_err();
    assert!(
        err.to_string().contains("field") && err.to_string().contains("empty"),
        "expected empty-field rejection, got: {err}"
    );
}

#[test]
fn filter_rejects_leaf_with_unknown_op() {
    let leaf = serde_json::json!({"field": "x", "op": "unknown_op", "value": 1});
    let err = serde_json::from_value::<SearchArgsFilters>(serde_json::json!({"and": [leaf]}))
        .unwrap_err();
    assert!(
        err.to_string().contains("unknown op") || err.to_string().contains("op"),
        "expected unknown-op rejection, got: {err}"
    );
}

#[test]
fn filter_rejects_leaf_with_extra_key() {
    let leaf = serde_json::json!({"field": "x", "op": "eq", "value": 1, "extra": true});
    let err = serde_json::from_value::<SearchArgsFilters>(serde_json::json!({"and": [leaf]}))
        .unwrap_err();
    assert!(
        err.to_string().contains("unknown key") || err.to_string().contains("filter leaf"),
        "expected extra-key rejection, got: {err}"
    );
}

#[test]
fn filter_accepts_canonical_string_leaf() {
    let leaf = serde_json::json!({"field": "kind", "op": "eq", "value": "note"});
    let parsed: SearchArgsFilters = serde_json::from_value(leaf).unwrap();
    assert!(matches!(parsed, SearchArgsFilters::Leaf(_)));
}

#[test]
fn filter_accepts_between_leaf() {
    let leaf = serde_json::json!({"field": "score", "op": "between", "value": [0.1, 0.9]});
    let parsed: SearchArgsFilters = serde_json::from_value(leaf).unwrap();
    assert!(matches!(parsed, SearchArgsFilters::Leaf(_)));
}

#[test]
fn filter_rejects_between_with_wrong_arity() {
    let leaf = serde_json::json!({"field": "score", "op": "between", "value": [0.1]});
    let err = serde_json::from_value::<SearchArgsFilters>(leaf).unwrap_err();
    assert!(
        err.to_string().contains("between"),
        "expected between-arity rejection, got: {err}"
    );
}

#[test]
fn filter_rejects_in_with_empty_array() {
    let leaf = serde_json::json!({"field": "tag", "op": "in", "value": []});
    let err = serde_json::from_value::<SearchArgsFilters>(leaf).unwrap_err();
    assert!(
        err.to_string().contains("non-empty array") || err.to_string().contains("array"),
        "expected empty-in-array rejection, got: {err}"
    );
}

// ── F3 (round 7): Response error envelope shape ──────────────────────────────

#[test]
fn response_aborted_with_notfound_full_data_round_trips() {
    // NotFound is in the aborted family per response.json#x-cairn-error-code-families.
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("retrieve"));
    m.insert("status".into(), serde_json::json!("aborted"));
    m.insert(
        "error".into(),
        serde_json::json!({"code": "NotFound", "message": "missing", "data": {"target": "record:01"}}),
    );
    let parsed: Response = serde_json::from_value(serde_json::Value::Object(m)).unwrap();
    assert!(parsed.data.is_none());
}

#[test]
fn response_aborted_notfound_without_message_is_rejected() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("retrieve"));
    m.insert("status".into(), serde_json::json!("aborted"));
    m.insert(
        "error".into(),
        serde_json::json!({"code": "NotFound", "data": {"target": "record:01"}}),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("message"),
        "expected missing-message rejection, got: {err}"
    );
}

#[test]
fn response_rejected_with_unknown_code_is_rejected() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("search"));
    m.insert("status".into(), serde_json::json!("rejected"));
    m.insert(
        "error".into(),
        serde_json::json!({"code": "unknownInvalidCode", "message": "x"}),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("closed enum") || err.to_string().contains("code"),
        "expected unknown-code rejection, got: {err}"
    );
}

#[test]
fn response_aborted_with_empty_message_is_rejected() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("search"));
    m.insert("status".into(), serde_json::json!("aborted"));
    m.insert(
        "error".into(),
        serde_json::json!({"code": "Internal", "message": ""}),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("message"),
        "expected empty-message rejection, got: {err}"
    );
}

#[test]
fn response_rejected_invalidargs_without_data_is_rejected() {
    // InvalidArgs requires data.field + data.reason — payload omits data entirely.
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("search"));
    m.insert("status".into(), serde_json::json!("rejected"));
    m.insert(
        "error".into(),
        serde_json::json!({"code": "InvalidArgs", "message": "boom"}),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("data"),
        "expected missing-data rejection, got: {err}"
    );
}

#[test]
fn response_rejected_invalidargs_with_empty_field_is_rejected() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("search"));
    m.insert("status".into(), serde_json::json!("rejected"));
    m.insert(
        "error".into(),
        serde_json::json!({"code": "InvalidArgs", "message": "boom", "data": {"field": "", "reason": "x"}}),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("field") && err.to_string().contains("empty"),
        "expected empty-field rejection, got: {err}"
    );
}

#[test]
fn response_rejected_internal_without_data_round_trips() {
    // Internal has no required data fields — payload may omit data.
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("search"));
    m.insert("status".into(), serde_json::json!("aborted"));
    m.insert(
        "error".into(),
        serde_json::json!({"code": "Internal", "message": "boom"}),
    );
    let parsed: Response = serde_json::from_value(serde_json::Value::Object(m)).unwrap();
    assert!(parsed.data.is_none());
}

// ── F4 (round 7): RFC-3339 calendar-aware month/day validation ───────────────

#[test]
fn signed_intent_rejects_february_31() {
    let mut m = signed_intent_minimum();
    m.insert(
        "issued_at".into(),
        serde_json::json!("2026-02-31T00:00:00Z"),
    );
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("issued_at"),
        "expected feb-31 rejection, got: {err}"
    );
}

#[test]
fn signed_intent_rejects_april_31() {
    let mut m = signed_intent_minimum();
    m.insert(
        "issued_at".into(),
        serde_json::json!("2025-04-31T00:00:00Z"),
    );
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("issued_at"),
        "expected april-31 rejection, got: {err}"
    );
}

#[test]
fn signed_intent_accepts_leap_day_div_by_4_not_100() {
    // 2024 is divisible by 4 but not by 100 — leap year.
    let mut m = signed_intent_minimum();
    m.insert(
        "issued_at".into(),
        serde_json::json!("2024-02-29T00:00:00Z"),
    );
    let parsed: SignedIntent = serde_json::from_value(serde_json::Value::Object(m)).unwrap();
    assert_eq!(parsed.issued_at, "2024-02-29T00:00:00Z");
}

#[test]
fn signed_intent_rejects_feb_29_non_leap_year() {
    // 2025 is not divisible by 4 — Feb 29 invalid.
    let mut m = signed_intent_minimum();
    m.insert(
        "issued_at".into(),
        serde_json::json!("2025-02-29T00:00:00Z"),
    );
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("issued_at"),
        "expected non-leap feb-29 rejection, got: {err}"
    );
}

#[test]
fn signed_intent_accepts_leap_day_div_by_400() {
    // 2000 is divisible by 400 — leap year despite being divisible by 100.
    let mut m = signed_intent_minimum();
    m.insert(
        "issued_at".into(),
        serde_json::json!("2000-02-29T00:00:00Z"),
    );
    let parsed: SignedIntent = serde_json::from_value(serde_json::Value::Object(m)).unwrap();
    assert_eq!(parsed.issued_at, "2000-02-29T00:00:00Z");
}

#[test]
fn signed_intent_rejects_feb_29_div_by_100_not_400() {
    // 1900 is divisible by 100 but not by 400 — not a leap year.
    let mut m = signed_intent_minimum();
    m.insert(
        "issued_at".into(),
        serde_json::json!("1900-02-29T00:00:00Z"),
    );
    let err = serde_json::from_value::<SignedIntent>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("issued_at"),
        "expected century non-leap feb-29 rejection, got: {err}"
    );
}

// ── F5 (round 7): Ulid newtype pattern enforced at every call site ───────────

#[test]
fn retrieve_record_rejects_non_ulid_id() {
    let json = serde_json::json!({"target": "record", "id": "not-a-ulid"});
    let err = serde_json::from_value::<RetrieveArgs>(json).unwrap_err();
    assert!(
        err.to_string().contains("ULID"),
        "expected ULID rejection, got: {err}"
    );
}

#[test]
fn retrieve_record_rejects_short_ulid() {
    let json = serde_json::json!({"target": "record", "id": "0123"});
    let err = serde_json::from_value::<RetrieveArgs>(json).unwrap_err();
    assert!(
        err.to_string().contains("ULID"),
        "expected ULID rejection, got: {err}"
    );
}

#[test]
fn forget_record_rejects_short_ulid() {
    let json = serde_json::json!({"mode": "record", "record_id": "0123"});
    let err = serde_json::from_value::<ForgetArgs>(json).unwrap_err();
    assert!(
        err.to_string().contains("ULID"),
        "expected ULID rejection, got: {err}"
    );
}

#[test]
fn retrieve_record_rejects_lowercase_ulid() {
    // Lowercase letters are not in the Crockford base32 alphabet.
    let json = serde_json::json!({"target": "record", "id": "01arz3ndektsv4rrffq69g5fav"});
    let err = serde_json::from_value::<RetrieveArgs>(json).unwrap_err();
    assert!(
        err.to_string().contains("ULID") || err.to_string().contains("Crockford"),
        "expected lowercase-ULID rejection, got: {err}"
    );
}

#[test]
fn retrieve_record_accepts_valid_ulid() {
    let json = serde_json::json!({"target": "record", "id": "01ARZ3NDEKTSV4RRFFQ69G5FAV"});
    let parsed: RetrieveArgs = serde_json::from_value(json).unwrap();
    assert!(matches!(parsed, RetrieveArgs::Record { .. }));
}

// ── F1 (round 8): ForgetArgs Session.session_id minLength=1 ──────────────────

#[test]
fn forget_session_rejects_empty_session_id() {
    let json = serde_json::json!({"mode": "session", "session_id": ""});
    let err = serde_json::from_value::<ForgetArgs>(json).unwrap_err();
    assert!(
        err.to_string().contains("session_id") && err.to_string().contains("empty"),
        "expected empty session_id rejection, got: {err}"
    );
}

#[test]
fn forget_session_accepts_non_empty_session_id() {
    let json = serde_json::json!({"mode": "session", "session_id": "s1"});
    let parsed: ForgetArgs = serde_json::from_value(json).unwrap();
    assert!(matches!(parsed, ForgetArgs::Session { .. }));
}

#[test]
fn retrieve_scope_rejects_overlong_cursor_via_newtype() {
    // Cursor newtype enforces maxLength: 512 at deserialize. Use the Session
    // variant since RetrieveArgs::Session.cursor is a typed Cursor newtype
    // (the Scope variant currently uses a raw String). 513 chars trips the cap.
    let cursor: String = "x".repeat(513);
    let json = serde_json::json!({
        "target": "session",
        "session_id": "s1",
        "cursor": cursor,
    });
    let err = serde_json::from_value::<RetrieveArgs>(json).unwrap_err();
    assert!(
        err.to_string().contains("Cursor") || err.to_string().contains("cursor"),
        "expected cursor cap rejection, got: {err}"
    );
}

// ── F4 (round 8): filter_leaf validator matches per-shape oneOf branches ────

#[test]
fn filter_leaf_rejects_boolean_neq() {
    // filter_leaf_boolean only permits op=eq.
    let leaf = serde_json::json!({"field": "x", "op": "neq", "value": true});
    let err = serde_json::from_value::<SearchArgsFilters>(serde_json::json!({"and": [leaf]}))
        .unwrap_err();
    assert!(
        err.to_string().contains("boolean") || err.to_string().contains("eq"),
        "expected boolean-neq rejection, got: {err}"
    );
}

#[test]
fn filter_leaf_accepts_boolean_eq() {
    let leaf = serde_json::json!({"field": "x", "op": "eq", "value": true});
    let parsed: SearchArgsFilters =
        serde_json::from_value(serde_json::json!({"and": [leaf]})).unwrap();
    assert!(matches!(parsed, SearchArgsFilters::And { .. }));
}

#[test]
fn filter_leaf_rejects_in_with_mixed_types() {
    let leaf = serde_json::json!({"field": "x", "op": "in", "value": ["a", 1]});
    let err = serde_json::from_value::<SearchArgsFilters>(serde_json::json!({"and": [leaf]}))
        .unwrap_err();
    assert!(
        err.to_string().contains("all strings or all numbers"),
        "expected mixed-types rejection, got: {err}"
    );
}

#[test]
fn filter_leaf_rejects_nin_with_mixed_types() {
    let leaf = serde_json::json!({"field": "x", "op": "nin", "value": [1, "a"]});
    let err = serde_json::from_value::<SearchArgsFilters>(serde_json::json!({"and": [leaf]}))
        .unwrap_err();
    assert!(
        err.to_string().contains("all strings or all numbers"),
        "expected mixed-types rejection, got: {err}"
    );
}

#[test]
fn filter_leaf_accepts_in_all_strings() {
    let leaf = serde_json::json!({"field": "x", "op": "in", "value": ["a", "b"]});
    let parsed: SearchArgsFilters =
        serde_json::from_value(serde_json::json!({"and": [leaf]})).unwrap();
    assert!(matches!(parsed, SearchArgsFilters::And { .. }));
}

#[test]
fn filter_leaf_accepts_in_all_numbers() {
    let leaf = serde_json::json!({"field": "x", "op": "in", "value": [1, 2]});
    let parsed: SearchArgsFilters =
        serde_json::from_value(serde_json::json!({"and": [leaf]})).unwrap();
    assert!(matches!(parsed, SearchArgsFilters::And { .. }));
}

// ── F3 (round 8): Filter operator nodes deny extra keys ─────────────────────

#[test]
fn filter_rejects_node_with_two_operator_keys() {
    let leaf = serde_json::json!({"field": "x", "op": "eq", "value": 1});
    let wire = serde_json::json!({"and": [leaf.clone()], "or": [leaf.clone()]});
    let err = serde_json::from_value::<SearchArgsFilters>(wire).unwrap_err();
    assert!(
        err.to_string().contains("at most one"),
        "expected mixed-operator rejection, got: {err}"
    );
}

#[test]
fn filter_rejects_operator_node_with_leaf_keys() {
    // {"and":[..], "field":"x", "op":"eq", "value":1} previously matched And
    // and silently dropped the leaf-shape keys.
    let leaf = serde_json::json!({"field": "x", "op": "eq", "value": 1});
    let wire = serde_json::json!({
        "and": [leaf.clone()],
        "field": "x",
        "op": "eq",
        "value": 1,
    });
    let err = serde_json::from_value::<SearchArgsFilters>(wire).unwrap_err();
    assert!(
        err.to_string().contains("extra keys") || err.to_string().contains("operator node"),
        "expected mixed-operator+leaf rejection, got: {err}"
    );
}

#[test]
fn filter_rejects_operator_node_with_arbitrary_extra_key() {
    let leaf = serde_json::json!({"field": "x", "op": "eq", "value": 1});
    let wire = serde_json::json!({"and": [leaf.clone()], "foo": 1});
    let err = serde_json::from_value::<SearchArgsFilters>(wire).unwrap_err();
    assert!(
        err.to_string().contains("extra keys") || err.to_string().contains("operator node"),
        "expected extra-key rejection, got: {err}"
    );
}

#[test]
fn filter_pure_and_still_round_trips() {
    let leaf = serde_json::json!({"field": "x", "op": "eq", "value": 1});
    let wire = serde_json::json!({"and": [leaf.clone()]});
    let parsed: SearchArgsFilters = serde_json::from_value(wire.clone()).unwrap();
    assert!(matches!(parsed, SearchArgsFilters::And { .. }));
    assert_eq!(serde_json::to_value(&parsed).unwrap(), wire);
}

#[test]
fn filter_pure_or_still_round_trips() {
    let leaf = serde_json::json!({"field": "x", "op": "eq", "value": 1});
    let wire = serde_json::json!({"or": [leaf.clone()]});
    let parsed: SearchArgsFilters = serde_json::from_value(wire.clone()).unwrap();
    assert!(matches!(parsed, SearchArgsFilters::Or { .. }));
}

#[test]
fn filter_pure_not_still_round_trips() {
    let leaf = serde_json::json!({"field": "x", "op": "eq", "value": 1});
    let wire = serde_json::json!({"not": leaf.clone()});
    let parsed: SearchArgsFilters = serde_json::from_value(wire).unwrap();
    assert!(matches!(parsed, SearchArgsFilters::Not { .. }));
}

#[test]
fn filter_pure_leaf_still_round_trips_as_leaf() {
    let wire = serde_json::json!({"field": "x", "op": "eq", "value": 1});
    let parsed: SearchArgsFilters = serde_json::from_value(wire).unwrap();
    assert!(matches!(parsed, SearchArgsFilters::Leaf(_)));
}

// ── F5 (round 8): Error envelope validator closes top-level + data shapes ───

#[test]
fn response_error_rejects_unknown_top_level_key() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("retrieve"));
    m.insert("status".into(), serde_json::json!("aborted"));
    m.insert(
        "error".into(),
        serde_json::json!({
            "code": "NotFound",
            "message": "x",
            "data": {"target": "t"},
            "unknown_top": 1,
        }),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("unknown top-level key"),
        "expected unknown-top-key rejection, got: {err}"
    );
}

#[test]
fn response_error_rejects_replay_detected_with_non_ulid_operation_id() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("search"));
    m.insert("status".into(), serde_json::json!("rejected"));
    m.insert(
        "error".into(),
        serde_json::json!({
            "code": "ReplayDetected",
            "message": "x",
            "data": {"operation_id": "not-a-ulid"},
        }),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("ULID") || err.to_string().contains("Crockford"),
        "expected non-ULID operation_id rejection, got: {err}"
    );
}

#[test]
fn response_error_rejects_expired_intent_with_garbage_timestamp() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("search"));
    m.insert("status".into(), serde_json::json!("rejected"));
    m.insert(
        "error".into(),
        serde_json::json!({
            "code": "ExpiredIntent",
            "message": "x",
            "data": {
                "issued_at": "yesterday",
                "expires_at": "2026-01-01T00:00:00Z",
                "now": "2026-01-01T00:00:00Z",
            },
        }),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("RFC-3339") || err.to_string().contains("issued_at"),
        "expected RFC-3339 rejection, got: {err}"
    );
}

#[test]
fn response_error_rejects_capability_unavailable_with_unknown_capability() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("search"));
    m.insert("status".into(), serde_json::json!("rejected"));
    m.insert(
        "error".into(),
        serde_json::json!({
            "code": "CapabilityUnavailable",
            "message": "x",
            "data": {"capability": "cairn.bogus.fake.v1"},
        }),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("known capability") || err.to_string().contains("capability"),
        "expected unknown-capability rejection, got: {err}"
    );
}

#[test]
fn response_error_capability_unavailable_with_known_capability_round_trips() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("search"));
    m.insert("status".into(), serde_json::json!("rejected"));
    m.insert(
        "error".into(),
        serde_json::json!({
            "code": "CapabilityUnavailable",
            "message": "x",
            "data": {"capability": "cairn.mcp.v1.search.semantic"},
        }),
    );
    let parsed: Response = serde_json::from_value(serde_json::Value::Object(m)).unwrap();
    assert!(parsed.data.is_none());
}

#[test]
fn response_error_replay_detected_with_valid_ulid_round_trips() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("search"));
    m.insert("status".into(), serde_json::json!("rejected"));
    m.insert(
        "error".into(),
        serde_json::json!({
            "code": "ReplayDetected",
            "message": "x",
            "data": {"operation_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV"},
        }),
    );
    let parsed: Response = serde_json::from_value(serde_json::Value::Object(m)).unwrap();
    assert!(parsed.data.is_none());
}

#[test]
fn response_error_rejects_data_with_unknown_key() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("retrieve"));
    m.insert("status".into(), serde_json::json!("aborted"));
    m.insert(
        "error".into(),
        serde_json::json!({
            "code": "NotFound",
            "message": "x",
            "data": {"target": "t", "extra": 1},
        }),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("unknown key"),
        "expected unknown-data-key rejection, got: {err}"
    );
}

// ── F2 (round 8): Response status bound to error-code family ─────────────────

#[test]
fn response_rejected_with_aborted_family_code_is_rejected() {
    // NotFound is in the aborted family — illegal on status=rejected.
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("retrieve"));
    m.insert("status".into(), serde_json::json!("rejected"));
    m.insert(
        "error".into(),
        serde_json::json!({"code": "NotFound", "message": "x", "data": {"target": "t"}}),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("rejected") && msg.contains("rejected family"),
        "expected rejected-family-mismatch error, got: {err}"
    );
}

#[test]
fn response_aborted_with_rejected_family_code_is_rejected() {
    // UnknownVerb is in the rejected family — illegal on status=aborted. Use a
    // non-unknown verb so the bidirectional UnknownVerb⟺verb=unknown rule fires
    // first; that gates on verb pairing, not on family. To exercise the family
    // check use a different rejected-family code (e.g. ExpiredIntent).
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("search"));
    m.insert("status".into(), serde_json::json!("aborted"));
    m.insert(
        "error".into(),
        serde_json::json!({"code": "ExpiredIntent", "message": "x", "data": {
            "issued_at": "2026-01-01T00:00:00Z",
            "expires_at": "2026-01-01T00:00:00Z",
            "now": "2026-01-01T00:00:00Z",
        }}),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("aborted") && msg.contains("aborted family"),
        "expected aborted-family-mismatch error, got: {err}"
    );
}

#[test]
fn response_rejected_with_rejected_family_code_round_trips() {
    // ExpiredIntent is in the rejected family — accepted on status=rejected.
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("search"));
    m.insert("status".into(), serde_json::json!("rejected"));
    m.insert(
        "error".into(),
        serde_json::json!({"code": "ExpiredIntent", "message": "x", "data": {
            "issued_at": "2026-01-01T00:00:00Z",
            "expires_at": "2026-01-01T00:00:00Z",
            "now": "2026-01-01T00:00:00Z",
        }}),
    );
    let parsed: Response = serde_json::from_value(serde_json::Value::Object(m)).unwrap();
    assert!(parsed.data.is_none());
}

#[test]
fn response_aborted_with_aborted_family_code_round_trips() {
    // PluginSuspended is in the aborted family — accepted on status=aborted.
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("search"));
    m.insert("status".into(), serde_json::json!("aborted"));
    m.insert(
        "error".into(),
        serde_json::json!({"code": "PluginSuspended", "message": "x", "data": {"plugin_id": "p1"}}),
    );
    let parsed: Response = serde_json::from_value(serde_json::Value::Object(m)).unwrap();
    assert!(parsed.data.is_none());
}

// ── F2 (round 9): every pattern-bearing primitive validated at deserialize ───
//
// Before round 9 only Ulid and Cursor enforced their IDL pattern at every
// deserialise boundary. The four other pattern-bearing primitives in
// `crates/cairn-idl/schema/common/primitives.json` (Nonce16Base64,
// Ed25519Signature, Identity) derived `#[serde(transparent)]` Deserialize, so
// HandshakeResponseChallenge.nonce (and any other call-site outside
// SignedIntent) silently accepted garbage. F2 lifts every pattern-bearing
// primitive to a hand-rolled Deserialize.

#[test]
fn handshake_challenge_rejects_malformed_nonce() {
    use cairn_core::generated::handshake::HandshakeResponseChallenge;
    let json = serde_json::json!({
        "nonce": "not-a-base64-nonce",
        "expires_at": 1_000_u64,
    });
    let err = serde_json::from_value::<HandshakeResponseChallenge>(json).unwrap_err();
    assert!(
        err.to_string().contains("Nonce16Base64"),
        "expected Nonce16Base64 rejection, got: {err}"
    );
}

#[test]
fn handshake_challenge_rejects_nonce_with_noncanonical_tail() {
    use cairn_core::generated::handshake::HandshakeResponseChallenge;
    // 22 chars, base64 alphabet, but tail is `B` (not in [AQgw]).
    let json = serde_json::json!({
        "nonce": "AAAAAAAAAAAAAAAAAAAAAB",
        "expires_at": 1_000_u64,
    });
    let err = serde_json::from_value::<HandshakeResponseChallenge>(json).unwrap_err();
    assert!(
        err.to_string().contains("Nonce16Base64"),
        "expected Nonce16Base64 rejection, got: {err}"
    );
}

#[test]
fn handshake_challenge_accepts_valid_nonce() {
    use cairn_core::generated::handshake::HandshakeResponseChallenge;
    let json = serde_json::json!({
        "nonce": "AAAAAAAAAAAAAAAAAAAAAA==",
        "expires_at": 1_000_u64,
    });
    let parsed: HandshakeResponseChallenge = serde_json::from_value(json).unwrap();
    assert_eq!(parsed.nonce.0, "AAAAAAAAAAAAAAAAAAAAAA==");
}

#[test]
fn identity_primitive_rejects_unknown_prefix() {
    use cairn_core::generated::common::Identity;
    let err = serde_json::from_value::<Identity>(serde_json::json!("xyz:alice")).unwrap_err();
    assert!(
        err.to_string().contains("Identity"),
        "expected Identity rejection, got: {err}"
    );
}

#[test]
fn identity_primitive_rejects_empty_body() {
    use cairn_core::generated::common::Identity;
    let err = serde_json::from_value::<Identity>(serde_json::json!("agt:")).unwrap_err();
    assert!(
        err.to_string().contains("Identity"),
        "expected Identity rejection, got: {err}"
    );
}

#[test]
fn identity_primitive_rejects_invalid_body_chars() {
    use cairn_core::generated::common::Identity;
    // Space is not in [A-Za-z0-9._:-].
    let err = serde_json::from_value::<Identity>(serde_json::json!("usr:alice bob")).unwrap_err();
    assert!(
        err.to_string().contains("Identity"),
        "expected Identity rejection, got: {err}"
    );
}

#[test]
fn identity_primitive_accepts_canonical_value() {
    use cairn_core::generated::common::Identity;
    let parsed: Identity =
        serde_json::from_value(serde_json::json!("agt:claude-code:opus-4-7:reviewer:v1")).unwrap();
    assert_eq!(parsed.0, "agt:claude-code:opus-4-7:reviewer:v1");
}

#[test]
fn ed25519_signature_primitive_rejects_missing_prefix() {
    use cairn_core::generated::common::Ed25519Signature;
    let bad = serde_json::json!("0".repeat(128));
    let err = serde_json::from_value::<Ed25519Signature>(bad).unwrap_err();
    assert!(
        err.to_string().contains("Ed25519Signature"),
        "expected Ed25519Signature rejection, got: {err}"
    );
}

#[test]
fn ed25519_signature_primitive_rejects_uppercase_hex() {
    use cairn_core::generated::common::Ed25519Signature;
    let bad = serde_json::json!(format!("ed25519:{}", "A".repeat(128)));
    let err = serde_json::from_value::<Ed25519Signature>(bad).unwrap_err();
    assert!(
        err.to_string().contains("Ed25519Signature"),
        "expected Ed25519Signature rejection, got: {err}"
    );
}

#[test]
fn ed25519_signature_primitive_accepts_canonical_value() {
    use cairn_core::generated::common::Ed25519Signature;
    let good = serde_json::json!(format!("ed25519:{}", "0".repeat(128)));
    let parsed: Ed25519Signature = serde_json::from_value(good).unwrap();
    assert!(parsed.0.starts_with("ed25519:"));
}

#[test]
fn nonce16_primitive_rejects_wrong_length() {
    use cairn_core::generated::common::Nonce16Base64;
    let err = serde_json::from_value::<Nonce16Base64>(serde_json::json!("AAA")).unwrap_err();
    assert!(
        err.to_string().contains("Nonce16Base64"),
        "expected Nonce16Base64 rejection, got: {err}"
    );
}

#[test]
fn nonce16_primitive_accepts_unpadded_canonical_value() {
    use cairn_core::generated::common::Nonce16Base64;
    let parsed: Nonce16Base64 =
        serde_json::from_value(serde_json::json!("BBBBBBBBBBBBBBBBBBBBBA")).unwrap();
    assert_eq!(parsed.0, "BBBBBBBBBBBBBBBBBBBBBA");
}

// ── F3 (round 9): retrieve Data* per-field constraints at deserialize ─────────

#[test]
fn retrieve_data_folder_rejects_depth_above_max() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("retrieve"));
    m.insert("status".into(), serde_json::json!("committed"));
    m.insert("target".into(), serde_json::json!("folder"));
    m.insert(
        "data".into(),
        serde_json::json!({"path": "a/b", "depth": 999, "items": []}),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("depth"),
        "expected DataFolder.depth rejection, got: {err}"
    );
}

#[test]
fn retrieve_data_folder_rejects_empty_path() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("retrieve"));
    m.insert("status".into(), serde_json::json!("committed"));
    m.insert("target".into(), serde_json::json!("folder"));
    m.insert(
        "data".into(),
        serde_json::json!({"path": "", "depth": 1, "items": []}),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("path"),
        "expected DataFolder.path rejection, got: {err}"
    );
}

#[test]
fn retrieve_data_record_rejects_empty_kind() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("retrieve"));
    m.insert("status".into(), serde_json::json!("committed"));
    m.insert("target".into(), serde_json::json!("record"));
    m.insert(
        "data".into(),
        serde_json::json!({"record_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV", "kind": ""}),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("kind"),
        "expected DataRecord.kind rejection, got: {err}"
    );
}

#[test]
fn retrieve_data_session_rejects_empty_session_id() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("retrieve"));
    m.insert("status".into(), serde_json::json!("committed"));
    m.insert("target".into(), serde_json::json!("session"));
    m.insert(
        "data".into(),
        serde_json::json!({"session_id": "", "items": []}),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("session_id"),
        "expected DataSession.session_id rejection, got: {err}"
    );
}

// ── F4 (round 9): allow optional error-data fields with primitive validation ──

#[test]
fn error_invalid_filter_with_optional_path_accepts() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("search"));
    m.insert("status".into(), serde_json::json!("rejected"));
    m.insert(
        "error".into(),
        serde_json::json!({
            "code": "InvalidFilter",
            "message": "invalid filter shape",
            "data": {"reason": "leaf op unknown", "path": "filters.and[0]"}
        }),
    );
    let _: Response = serde_json::from_value(serde_json::Value::Object(m))
        .expect("InvalidFilter with optional path should accept");
}

#[test]
fn error_invalid_filter_optional_path_rejects_empty_string() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("search"));
    m.insert("status".into(), serde_json::json!("rejected"));
    m.insert(
        "error".into(),
        serde_json::json!({
            "code": "InvalidFilter",
            "message": "invalid filter shape",
            "data": {"reason": "x", "path": ""}
        }),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("path"),
        "expected InvalidFilter.path empty rejection, got: {err}"
    );
}

#[test]
fn error_quarantine_required_with_optional_quarantine_id_accepts() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("ingest"));
    m.insert("status".into(), serde_json::json!("aborted"));
    m.insert(
        "error".into(),
        serde_json::json!({
            "code": "QuarantineRequired",
            "message": "quarantine triggered",
            "data": {"reason": "policy", "quarantine_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV"}
        }),
    );
    let _: Response = serde_json::from_value(serde_json::Value::Object(m))
        .expect("QuarantineRequired with optional Ulid quarantine_id should accept");
}

#[test]
fn error_quarantine_required_optional_quarantine_id_rejects_non_ulid() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("ingest"));
    m.insert("status".into(), serde_json::json!("aborted"));
    m.insert(
        "error".into(),
        serde_json::json!({
            "code": "QuarantineRequired",
            "message": "quarantine triggered",
            "data": {"reason": "policy", "quarantine_id": "not-a-ulid"}
        }),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("quarantine_id"),
        "expected non-ULID quarantine_id rejection, got: {err}"
    );
}

#[test]
fn error_invalid_filter_rejects_unknown_data_key() {
    let mut m = response_base();
    m.insert("verb".into(), serde_json::json!("search"));
    m.insert("status".into(), serde_json::json!("rejected"));
    m.insert(
        "error".into(),
        serde_json::json!({
            "code": "InvalidFilter",
            "message": "x",
            "data": {"reason": "x", "unknown_key": 1}
        }),
    );
    let err = serde_json::from_value::<Response>(serde_json::Value::Object(m)).unwrap_err();
    assert!(
        err.to_string().contains("unknown key"),
        "expected unknown-key rejection, got: {err}"
    );
}
