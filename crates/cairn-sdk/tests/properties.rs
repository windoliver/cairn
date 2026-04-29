//! Property tests for the SDK's hand-rolled validators.
//!
//! These exercise the boundary conditions where the SDK's structural
//! floor is most likely to drift from the generated wire validators —
//! ULID grammar, the search filter leaf op set, the URI floor, the
//! ingest body/file/url XOR, and the `at-least-one-of` rule on
//! `ScopeFilter`.

use cairn_sdk::SdkError;
use cairn_sdk::generated::common::{Cursor, ScopeFilter, Ulid};
use cairn_sdk::generated::verbs::ingest::IngestArgs;
use cairn_sdk::generated::verbs::retrieve::RetrieveArgs;
use cairn_sdk::generated::verbs::search::{SearchArgs, SearchArgsFilters, SearchArgsMode};
use cairn_sdk::{Sdk, VerbResponse};
use proptest::prelude::*;

fn sdk() -> Sdk {
    Sdk::new()
}

fn is_invalid_args(err: &SdkError) -> bool {
    matches!(err, SdkError::InvalidArgs { .. })
}

fn is_capability_unavailable(err: &SdkError) -> bool {
    matches!(err, SdkError::CapabilityUnavailable { .. })
}

fn search_with_filter(filter: serde_json::Value) -> SearchArgs {
    SearchArgs {
        citations: None,
        cursor: None,
        filters: Some(SearchArgsFilters::Leaf(filter)),
        limit: None,
        mode: SearchArgsMode::Keyword,
        query: "q".to_owned(),
        scope: None,
    }
}

fn ingest_with_url(url: String) -> IngestArgs {
    IngestArgs {
        body: None,
        file: None,
        frontmatter: None,
        kind: "note".to_owned(),
        session_id: None,
        tags: None,
        url: Some(url),
    }
}

// ────────────────────────────────────────────────────────────────────
// ULID grammar — Crockford base32, exactly 26 chars, no I/L/O/U.
// ────────────────────────────────────────────────────────────────────

const CROCKFORD: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

fn valid_ulid_string() -> impl Strategy<Value = String> {
    proptest::collection::vec(0..CROCKFORD.len(), 26..=26).prop_map(|idxs| {
        String::from_utf8(idxs.into_iter().map(|i| CROCKFORD[i]).collect()).unwrap()
    })
}

proptest! {
    #[test]
    fn valid_ulids_round_trip_through_retrieve_record(s in valid_ulid_string()) {
        // RetrieveArgs::Record only validates the ULID grammar; capability
        // gating runs after, so passing the ULID floor surfaces as
        // CapabilityUnavailable, not InvalidArgs.
        let args = RetrieveArgs::Record { id: Ulid(s) };
        let err = sdk().retrieve(&args).expect_err("P0 has no capability");
        prop_assert!(
            is_capability_unavailable(&err),
            "expected CapabilityUnavailable for a valid ULID, got {err:?}"
        );
    }

    #[test]
    fn wrong_length_ulids_reject_with_invalid_args(
        s in "[0-9A-HJKMNP-TV-Z]{0,25}|[0-9A-HJKMNP-TV-Z]{27,40}"
    ) {
        let args = RetrieveArgs::Record { id: Ulid(s) };
        prop_assert!(is_invalid_args(&sdk().retrieve(&args).expect_err("must reject")));
    }

    #[test]
    fn ulids_with_disallowed_chars_reject(
        bad in "[ILOUilou \t\n!@#]"
    ) {
        // Pad to 26 chars with a known-good prefix so length isn't the
        // failing constraint.
        let s = format!("{}{}", "0123456789ABCDEFGHJKMNPQR", bad);
        let s = s.chars().take(26).collect::<String>();
        if s.len() == 26 {
            let args = RetrieveArgs::Record { id: Ulid(s) };
            prop_assert!(is_invalid_args(&sdk().retrieve(&args).expect_err("must reject")));
        }
    }
}

// ────────────────────────────────────────────────────────────────────
// Search filter leaf — every canonical op accepts well-typed values;
// every made-up op rejects.
// ────────────────────────────────────────────────────────────────────

const CANONICAL_OPS: &[&str] = &[
    "string_contains",
    "string_starts_with",
    "string_ends_with",
    "eq",
    "neq",
    "lt",
    "lte",
    "gt",
    "gte",
    "in",
    "nin",
    "between",
    "array_contains",
    "array_contains_any",
    "array_contains_all",
    "array_size_eq",
];

proptest! {
    #[test]
    fn unknown_filter_ops_reject(
        op in "[a-z_]{1,32}"
    ) {
        prop_assume!(!CANONICAL_OPS.contains(&op.as_str()));
        let args = search_with_filter(serde_json::json!({
            "field": "x",
            "op": op,
            "value": "v"
        }));
        prop_assert!(is_invalid_args(&sdk().search(&args).expect_err("must reject")));
    }

    #[test]
    fn unknown_filter_keys_reject(
        extra in "[a-z]{3,12}"
    ) {
        prop_assume!(!matches!(extra.as_str(), "field" | "op" | "value"));
        let mut leaf = serde_json::json!({"field": "x", "op": "eq", "value": "v"});
        leaf.as_object_mut().unwrap().insert(extra, serde_json::json!(1));
        let args = search_with_filter(leaf);
        prop_assert!(is_invalid_args(&sdk().search(&args).expect_err("must reject")));
    }

    #[test]
    fn empty_field_rejects_for_every_op(
        op_idx in 0..CANONICAL_OPS.len()
    ) {
        let args = search_with_filter(serde_json::json!({
            "field": "",
            "op": CANONICAL_OPS[op_idx],
            "value": "v"
        }));
        prop_assert!(is_invalid_args(&sdk().search(&args).expect_err("must reject")));
    }
}

// ────────────────────────────────────────────────────────────────────
// URI floor — non-ASCII / whitespace / control / scheme-only / digit-led.
// ────────────────────────────────────────────────────────────────────

proptest! {
    #[test]
    fn non_ascii_urls_reject(s in "http://[\u{0080}-\u{00FF}]{1,16}") {
        let err = sdk().ingest(&ingest_with_url(s)).expect_err("must reject");
        prop_assert!(is_invalid_args(&err));
    }

    #[test]
    fn whitespace_or_control_in_urls_rejects(
        bad in "[\t\n\r\u{0001}\u{007f} ]"
    ) {
        let s = format!("http://example.com/{bad}x");
        prop_assert!(is_invalid_args(&sdk().ingest(&ingest_with_url(s)).expect_err("must reject")));
    }

    #[test]
    fn digit_led_scheme_rejects(rest in "[a-z0-9+./-]{0,12}") {
        let s = format!("1{rest}:rest");
        prop_assert!(is_invalid_args(&sdk().ingest(&ingest_with_url(s)).expect_err("must reject")));
    }

    #[test]
    fn scheme_only_url_rejects(scheme in "[a-z][a-z0-9+.-]{0,12}") {
        let s = format!("{scheme}:");
        prop_assert!(is_invalid_args(&sdk().ingest(&ingest_with_url(s)).expect_err("must reject")));
    }
}

// ────────────────────────────────────────────────────────────────────
// Ingest body/file/url XOR — exactly one must be Some.
// ────────────────────────────────────────────────────────────────────

proptest! {
    #[test]
    fn ingest_xor_arity_zero_or_more_than_one_rejects(
        has_body in any::<bool>(),
        has_file in any::<bool>(),
        has_url in any::<bool>(),
    ) {
        let count =
            usize::from(has_body) + usize::from(has_file) + usize::from(has_url);
        prop_assume!(count != 1);
        let args = IngestArgs {
            body: has_body.then(|| "b".to_owned()),
            file: has_file.then(|| "/f".to_owned()),
            frontmatter: None,
            kind: "note".to_owned(),
            session_id: None,
            tags: None,
            url: has_url.then(|| "http://example.com/x".to_owned()),
        };
        let err = sdk().ingest(&args).expect_err("must reject");
        prop_assert!(is_invalid_args(&err));
    }
}

// ────────────────────────────────────────────────────────────────────
// ScopeFilter — at least one field must be set.
// ────────────────────────────────────────────────────────────────────

#[test]
fn empty_scope_filter_rejects() {
    let args = SearchArgs {
        citations: None,
        cursor: None,
        filters: None,
        limit: None,
        mode: SearchArgsMode::Keyword,
        query: "q".to_owned(),
        scope: Some(ScopeFilter {
            agent: None,
            entity: None,
            kind: None,
            record_ids: None,
            session_id: None,
            tags: None,
            tenant: None,
            tier: None,
            user: None,
            workspace: None,
        }),
    };
    assert!(matches!(
        sdk().search(&args).expect_err("must reject"),
        SdkError::InvalidArgs { .. }
    ));
}

// ────────────────────────────────────────────────────────────────────
// Cursor — non-empty, ≤ 512 chars.
// ────────────────────────────────────────────────────────────────────

proptest! {
    #[test]
    fn oversized_cursor_rejects(s in "[a-zA-Z0-9]{513,1024}") {
        let args = SearchArgs {
            citations: None,
            cursor: Some(Cursor(s)),
            filters: None,
            limit: None,
            mode: SearchArgsMode::Keyword,
            query: "q".to_owned(),
            scope: None,
        };
        prop_assert!(is_invalid_args(&sdk().search(&args).expect_err("must reject")));
    }
}

// ────────────────────────────────────────────────────────────────────
// VerbResponse Serialize — verb/target invariant under a wide
// permutation sweep.
// ────────────────────────────────────────────────────────────────────

use cairn_sdk::generated::envelope::{ResponseTarget, ResponseVerb};

fn verb_strategy() -> impl Strategy<Value = ResponseVerb> {
    prop_oneof![
        Just(ResponseVerb::Ingest),
        Just(ResponseVerb::Search),
        Just(ResponseVerb::Retrieve),
        Just(ResponseVerb::Summarize),
        Just(ResponseVerb::AssembleHot),
        Just(ResponseVerb::CaptureTrace),
        Just(ResponseVerb::Lint),
        Just(ResponseVerb::Forget),
        Just(ResponseVerb::Unknown),
    ]
}

fn target_strategy() -> impl Strategy<Value = Option<ResponseTarget>> {
    prop_oneof![
        Just(None),
        Just(Some(ResponseTarget::Record)),
        Just(Some(ResponseTarget::Session)),
        Just(Some(ResponseTarget::Turn)),
        Just(Some(ResponseTarget::Folder)),
        Just(Some(ResponseTarget::Scope)),
        Just(Some(ResponseTarget::Profile)),
    ]
}

fn known_good_ulid() -> Ulid {
    Ulid("01HZX9YT6Q4G2K7M3N5P8R0V1W".to_owned())
}

proptest! {
    #[test]
    fn verb_response_serialize_invariants(
        verb in verb_strategy(),
        target in target_strategy(),
    ) {
        let resp: VerbResponse<serde_json::Value> = VerbResponse {
            operation_id: known_good_ulid(),
            policy_trace: vec![],
            verb,
            target,
            data: serde_json::json!({}),
        };
        let serialized = serde_json::to_value(&resp);
        let is_unknown = matches!(verb, ResponseVerb::Unknown);
        let is_retrieve = matches!(verb, ResponseVerb::Retrieve);
        let target_set = target.is_some();
        // Allowed: known verb + (retrieve ⇔ target).
        let should_succeed = !is_unknown && (is_retrieve == target_set);
        prop_assert_eq!(serialized.is_ok(), should_succeed);
    }
}
