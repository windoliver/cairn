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
use cairn_core::generated::envelope::{Request, RequestArgs, Response, ResponseData, SignedIntent};
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
    m.insert("signature".into(), serde_json::json!("00".repeat(64)));
    m.insert("target_hash".into(), serde_json::json!("0".repeat(64)));
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
    serde_json::json!({
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
        "signature": "00".repeat(64),
        "target_hash": "0".repeat(64),
    })
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
    // Retrieve sub-dispatch is deferred — just assert it lands in the
    // Retrieve(opaque JSON) variant.
    assert!(matches!(parsed.data, Some(ResponseData::Retrieve(_))));
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
