//! SDK surface tests.
//!
//! Verifies the acceptance criteria from issue #60:
//! - SDK consumers can call every P0 verb and receive typed results.
//! - SDK version reports the same protocol capability data as `status`.
//! - Typed errors surface for unsupported capabilities (P0 stub: store
//!   not wired).
//! - SDK responses serialize into the same envelope shape the CLI emits.

use cairn_sdk::error::ErrorCode;
use cairn_sdk::generated::common::{Cursor, ScopeFilter, Ulid};
use cairn_sdk::generated::verbs::search::SearchArgsFilters;
use cairn_sdk::generated::verbs::{
    assemble_hot::AssembleHotArgs,
    capture_trace::CaptureTraceArgs,
    forget::ForgetArgs,
    ingest::IngestArgs,
    lint::LintArgs,
    retrieve::RetrieveArgs,
    search::{SearchArgs, SearchArgsMode},
    summarize::SummarizeArgs,
};
use cairn_sdk::{Sdk, SdkError, version};

fn sdk() -> Sdk {
    Sdk::new()
}

fn ulid() -> Ulid {
    // Crockford-base32 fixture; structurally valid (26 chars, allowed alphabet).
    Ulid("01HZZ0000000000000000000AB".to_owned())
}

#[test]
fn version_matches_status_server_info() {
    let resp = sdk().status();
    assert_eq!(resp.server_info.version, version());
    assert_eq!(resp.contract, "cairn.mcp.v1");
}

#[test]
fn started_at_is_bound_to_sdk_construction_not_first_status_call() {
    // Sdk::new() must prime the incarnation snapshot so `started_at`
    // reflects when the SDK service started in this process, not whenever
    // something happens to call status() first.
    let s = Sdk::new();
    let constructed_at = s.status().server_info.started_at.clone();

    // Doing other work, then calling status again, must not advance
    // started_at — same process, same incarnation.
    std::thread::sleep(std::time::Duration::from_millis(1));
    let later = s.status().server_info.started_at;
    assert_eq!(constructed_at, later);
}

#[test]
fn status_incarnation_is_stable_process_wide() {
    // Brief §8.0.a wire-compat: status is byte-identical across an
    // incarnation. The SDK's incarnation unit is the *process*, not the
    // client instance — re-instantiating Sdk must NOT look like a server
    // restart to anything correlating against incarnation.
    let s = sdk();
    let a = s.status();
    let b = s.status();
    assert_eq!(a.server_info.incarnation, b.server_info.incarnation);
    assert_eq!(a.server_info.started_at, b.server_info.started_at);

    let other = Sdk::new();
    assert_eq!(
        s.status().server_info.incarnation,
        other.status().server_info.incarnation,
        "two Sdk instances in the same process must report the same incarnation"
    );
    assert_eq!(
        s.status().server_info.started_at,
        other.status().server_info.started_at
    );
}

#[test]
fn status_advertises_no_capabilities_in_p0() {
    let resp = sdk().status();
    // Mirrors `cairn status` — store not wired, so no capabilities.
    assert!(
        resp.capabilities.is_empty(),
        "P0 must advertise no capabilities until store wires up"
    );
    assert!(resp.extensions.is_empty());
}

#[test]
fn status_envelope_serializes_to_canonical_shape() {
    let resp = sdk().status();
    let value = serde_json::to_value(&resp).expect("status serializes");
    let obj = value.as_object().expect("envelope is an object");
    assert_eq!(
        obj.get("contract").and_then(|v| v.as_str()),
        Some("cairn.mcp.v1")
    );
    assert!(obj.contains_key("server_info"));
    assert!(obj.contains_key("capabilities"));
    assert!(obj.contains_key("extensions"));
    let server = obj["server_info"].as_object().expect("server_info object");
    for k in ["version", "build", "started_at", "incarnation"] {
        assert!(server.contains_key(k), "server_info.{k} missing");
    }
}

#[test]
fn handshake_mints_unique_nonces() {
    let s = sdk();
    let a = s.handshake();
    let b = s.handshake();
    assert_eq!(a.contract, "cairn.mcp.v1");
    assert_ne!(a.challenge.nonce.0, b.challenge.nonce.0);
    assert_eq!(a.challenge.nonce.0.len(), 24);
    assert!(a.challenge.expires_at > 0);
}

#[test]
fn ingest_invalid_args_returns_typed_error() {
    // Violate exactly-one-of: pass body AND file.
    let args = IngestArgs {
        body: Some("note".to_owned()),
        file: Some("/tmp/x".to_owned()),
        frontmatter: None,
        kind: "note".to_owned(),
        session_id: None,
        tags: None,
        url: None,
    };
    let err = sdk().ingest(&args).expect_err("must reject");
    match err {
        SdkError::InvalidArgs { reason } => {
            assert!(reason.contains("exactly one of"), "reason: {reason}");
        }
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[test]
fn ingest_valid_args_returns_internal_stub() {
    let args = IngestArgs {
        body: Some("note".to_owned()),
        file: None,
        frontmatter: None,
        kind: "note".to_owned(),
        session_id: None,
        tags: None,
        url: None,
    };
    assert_unimplemented("ingest", sdk().ingest(&args));
}

#[test]
fn ingest_rejects_schema_minlength_violations() {
    // The IDL `validate()` only enforces the body/file/url XOR, but the
    // schema additionally requires non-empty body, file, url, kind,
    // session_id, and tags[*]. Direct Rust construction must hit the same
    // floor.
    let bases = || IngestArgs {
        body: Some("note".to_owned()),
        file: None,
        frontmatter: None,
        kind: "note".to_owned(),
        session_id: None,
        tags: None,
        url: None,
    };
    let cases: [(&str, IngestArgs); 11] = [
        ("body", IngestArgs { body: Some(String::new()), ..bases() }),
        ("file", IngestArgs { body: None, file: Some(String::new()), ..bases() }),
        ("url",  IngestArgs { body: None, url: Some(String::new()), ..bases() }),
        ("url",  IngestArgs { body: None, url: Some("not-a-uri".to_owned()), ..bases() }),
        // Schemed-but-empty hier-part / colon-only / scheme-only / leading-digit:
        ("url",  IngestArgs { body: None, url: Some("http:".to_owned()), ..bases() }),
        ("url",  IngestArgs { body: None, url: Some(":rest".to_owned()), ..bases() }),
        ("url",  IngestArgs { body: None, url: Some("1bad:rest".to_owned()), ..bases() }),
        ("kind", IngestArgs { kind: String::new(), ..bases() }),
        ("session_id", IngestArgs { session_id: Some(String::new()), ..bases() }),
        ("tags", IngestArgs { tags: Some(vec![String::new()]), ..bases() }),
        ("frontmatter", IngestArgs { frontmatter: Some(serde_json::json!([1, 2])), ..bases() }),
    ];
    for (needle, args) in cases {
        match sdk().ingest(&args).expect_err("must reject") {
            SdkError::InvalidArgs { reason } => {
                assert!(reason.contains(needle), "reason {reason:?} missing {needle:?}");
            }
            other => panic!("expected InvalidArgs for {needle}, got {other:?}"),
        }
    }
}

#[test]
fn ingest_accepts_well_formed_uri_schemes() {
    // Sanity-check that the URI floor admits real schemes — `http`, `https`,
    // `file`, `cairn+vault` — so we don't accidentally regress to body-only.
    for url in [
        "http://example.com/x",
        "https://example.com/x",
        "file:/tmp/x",
        "cairn+vault://memo",
    ] {
        let args = IngestArgs {
            body: None,
            file: None,
            frontmatter: None,
            kind: "note".to_owned(),
            session_id: None,
            tags: None,
            url: Some(url.to_owned()),
        };
        assert_unimplemented("ingest", sdk().ingest(&args));
    }
}

#[test]
fn search_rejects_empty_query_with_invalid_args() {
    // Wire format requires non-empty query; SDK must surface it as
    // InvalidArgs instead of capability-checking an unvalidated request.
    let args = SearchArgs {
        citations: None,
        cursor: None,
        filters: None,
        limit: None,
        mode: SearchArgsMode::Keyword,
        query: String::new(),
        scope: None,
    };
    match sdk().search(&args).expect_err("must reject") {
        SdkError::InvalidArgs { reason } => {
            assert!(reason.contains("query"), "reason: {reason}");
        }
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[test]
fn search_rejects_out_of_range_limit_with_invalid_args() {
    let args = SearchArgs {
        citations: None,
        cursor: None,
        filters: None,
        limit: Some(0),
        mode: SearchArgsMode::Keyword,
        query: "hello".to_owned(),
        scope: None,
    };
    match sdk().search(&args).expect_err("must reject") {
        SdkError::InvalidArgs { reason } => {
            assert!(reason.contains("limit"), "reason: {reason}");
        }
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[test]
fn search_rejects_unadvertised_modes_with_capability_unavailable() {
    // P0 advertises no capabilities, so every search mode must fail closed
    // with CapabilityUnavailable rather than the generic Internal stub.
    for (mode, expected) in [
        (SearchArgsMode::Keyword, "cairn.mcp.v1.search.keyword"),
        (SearchArgsMode::Semantic, "cairn.mcp.v1.search.semantic"),
        (SearchArgsMode::Hybrid, "cairn.mcp.v1.search.hybrid"),
    ] {
        let args = SearchArgs {
            citations: None,
            cursor: None,
            filters: None,
            limit: None,
            mode,
            query: "hello".to_owned(),
            scope: None,
        };
        let err = sdk().search(&args).expect_err("must fail closed in P0");
        match err {
            SdkError::CapabilityUnavailable {
                capability,
                operation_id,
                ..
            } => {
                assert_eq!(capability, expected);
                assert_eq!(operation_id.0.len(), 26);
            }
            other => panic!("expected CapabilityUnavailable, got {other:?}"),
        }
    }
}

#[test]
fn retrieve_folder_rejects_empty_path_with_invalid_args() {
    let args = RetrieveArgs::Folder {
        path: String::new(),
        depth: None,
    };
    match sdk().retrieve(&args).expect_err("must reject") {
        SdkError::InvalidArgs { reason } => assert!(reason.contains("path"), "reason: {reason}"),
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[test]
fn retrieve_folder_rejects_excess_depth_with_invalid_args() {
    let args = RetrieveArgs::Folder {
        path: "/x".to_owned(),
        depth: Some(17),
    };
    match sdk().retrieve(&args).expect_err("must reject") {
        SdkError::InvalidArgs { reason } => assert!(reason.contains("depth"), "reason: {reason}"),
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[test]
fn retrieve_profile_requires_user_or_agent() {
    let args = RetrieveArgs::Profile {
        user: None,
        agent: None,
    };
    match sdk().retrieve(&args).expect_err("must reject") {
        SdkError::InvalidArgs { reason } => {
            assert!(reason.contains("user, agent"), "reason: {reason}");
        }
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[test]
fn search_rejects_empty_and_filter_with_invalid_args() {
    let args = SearchArgs {
        citations: None,
        cursor: None,
        filters: Some(SearchArgsFilters::And { and: vec![] }),
        limit: None,
        mode: SearchArgsMode::Keyword,
        query: "hi".to_owned(),
        scope: None,
    };
    match sdk().search(&args).expect_err("must reject") {
        SdkError::InvalidArgs { reason } => {
            assert!(reason.contains("filter.and"), "reason: {reason}");
        }
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[test]
fn search_rejects_excessive_filter_depth_with_invalid_args() {
    // Build a 9-level Not chain — exceeds max depth of 8.
    let mut node = SearchArgsFilters::Leaf(serde_json::json!({
        "field": "kind", "op": "eq", "value": "note"
    }));
    for _ in 0..9 {
        node = SearchArgsFilters::Not {
            not: Box::new(node),
        };
    }
    let args = SearchArgs {
        citations: None,
        cursor: None,
        filters: Some(node),
        limit: None,
        mode: SearchArgsMode::Keyword,
        query: "hi".to_owned(),
        scope: None,
    };
    match sdk().search(&args).expect_err("must reject") {
        SdkError::InvalidArgs { reason } => {
            assert!(reason.contains("max boolean depth"), "reason: {reason}");
        }
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[test]
fn search_rejects_malformed_filter_leaf_with_invalid_args() {
    let args = SearchArgs {
        citations: None,
        cursor: None,
        filters: Some(SearchArgsFilters::Leaf(serde_json::json!({
            "field": "",
            "op": "eq",
            "value": "x"
        }))),
        limit: None,
        mode: SearchArgsMode::Keyword,
        query: "hi".to_owned(),
        scope: None,
    };
    match sdk().search(&args).expect_err("must reject") {
        SdkError::InvalidArgs { reason } => {
            assert!(reason.contains("field"), "reason: {reason}");
        }
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[test]
fn search_accepts_extended_filter_operators() {
    // Mirrors the generated grammar: between, array_contains,
    // array_contains_any/all, and array_size_eq must validate cleanly.
    // With no capability advertised in P0 the call lands on
    // CapabilityUnavailable — the point is leaf validation passed.
    let valid_leaves = [
        serde_json::json!({"field": "score", "op": "between", "value": [0, 10]}),
        serde_json::json!({"field": "tags", "op": "array_contains", "value": "rust"}),
        serde_json::json!({"field": "tags", "op": "array_contains", "value": 42}),
        serde_json::json!({"field": "tags", "op": "array_contains_any", "value": ["a", "b"]}),
        serde_json::json!({"field": "tags", "op": "array_contains_all", "value": [1, 2, 3]}),
        serde_json::json!({"field": "tags", "op": "array_size_eq", "value": 0}),
    ];
    for leaf in valid_leaves {
        let args = SearchArgs {
            citations: None,
            cursor: None,
            filters: Some(SearchArgsFilters::Leaf(leaf.clone())),
            limit: None,
            mode: SearchArgsMode::Keyword,
            query: "hi".to_owned(),
            scope: None,
        };
        match sdk().search(&args).expect_err("P0 has no capability") {
            SdkError::CapabilityUnavailable { .. } => {}
            other => panic!("expected CapabilityUnavailable for {leaf:?}, got {other:?}"),
        }
    }
}

#[test]
fn search_rejects_malformed_extended_filter_operators_with_invalid_args() {
    let bad_leaves = [
        // between: wrong arity / non-numeric
        serde_json::json!({"field": "x", "op": "between", "value": [1]}),
        serde_json::json!({"field": "x", "op": "between", "value": [1, "two"]}),
        // array_contains: empty string / wrong type
        serde_json::json!({"field": "x", "op": "array_contains", "value": ""}),
        serde_json::json!({"field": "x", "op": "array_contains", "value": true}),
        // array_contains_any/all: empty / mixed-bad
        serde_json::json!({"field": "x", "op": "array_contains_any", "value": []}),
        serde_json::json!({"field": "x", "op": "array_contains_all", "value": [""]}),
        // array_size_eq: negative / non-integer
        serde_json::json!({"field": "x", "op": "array_size_eq", "value": -1}),
        serde_json::json!({"field": "x", "op": "array_size_eq", "value": "10"}),
    ];
    for leaf in bad_leaves {
        let args = SearchArgs {
            citations: None,
            cursor: None,
            filters: Some(SearchArgsFilters::Leaf(leaf.clone())),
            limit: None,
            mode: SearchArgsMode::Keyword,
            query: "hi".to_owned(),
            scope: None,
        };
        match sdk().search(&args).expect_err("must reject") {
            SdkError::InvalidArgs { .. } => {}
            other => panic!("expected InvalidArgs for {leaf:?}, got {other:?}"),
        }
    }
}

#[test]
fn search_rejects_malformed_cursor_with_invalid_args() {
    // Cursor newtype is publicly constructible; the SDK must re-apply the
    // generated Cursor::Deserialize rules (non-empty, ≤ 512 chars).
    let args = SearchArgs {
        citations: None,
        cursor: Some(Cursor(String::new())),
        filters: None,
        limit: None,
        mode: SearchArgsMode::Keyword,
        query: "hi".to_owned(),
        scope: None,
    };
    match sdk().search(&args).expect_err("must reject") {
        SdkError::InvalidArgs { reason } => assert!(reason.contains("Cursor"), "reason: {reason}"),
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[test]
fn search_rejects_empty_scope_filter_with_invalid_args() {
    // Empty ScopeFilter: every field None — must mirror RawScopeFilter
    // TryFrom's "at least one of [...]" check.
    let args = SearchArgs {
        citations: None,
        cursor: None,
        filters: None,
        limit: None,
        mode: SearchArgsMode::Keyword,
        query: "hi".to_owned(),
        scope: Some(empty_scope_filter()),
    };
    match sdk().search(&args).expect_err("must reject") {
        SdkError::InvalidArgs { reason } => {
            assert!(reason.contains("at least one of"), "reason: {reason}");
        }
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[test]
fn forget_record_rejects_malformed_ulid_with_invalid_args() {
    let args = ForgetArgs::Record {
        record_id: Ulid("not-a-ulid".to_owned()),
    };
    match sdk().forget(&args).expect_err("must reject") {
        SdkError::InvalidArgs { reason } => assert!(reason.contains("ULID"), "reason: {reason}"),
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

fn empty_scope_filter() -> ScopeFilter {
    ScopeFilter {
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
    }
}

#[test]
fn summarize_rejects_empty_record_ids_with_invalid_args() {
    let args = SummarizeArgs {
        citations: None,
        kind: None,
        persist: None,
        record_ids: vec![],
    };
    match sdk().summarize(&args).expect_err("must reject") {
        SdkError::InvalidArgs { reason } => {
            assert!(reason.contains("record_ids"), "reason: {reason}");
        }
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[test]
fn assemble_hot_rejects_oversized_budget_with_invalid_args() {
    let args = AssembleHotArgs {
        budget: Some(4_194_305),
        session_id: None,
    };
    match sdk().assemble_hot(&args).expect_err("must reject") {
        SdkError::InvalidArgs { reason } => assert!(reason.contains("budget"), "reason: {reason}"),
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[test]
fn capture_trace_rejects_empty_from_with_invalid_args() {
    let args = CaptureTraceArgs {
        from: String::new(),
        session_id: None,
    };
    match sdk().capture_trace(&args).expect_err("must reject") {
        SdkError::InvalidArgs { reason } => assert!(reason.contains("from"), "reason: {reason}"),
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[test]
fn forget_session_rejects_empty_session_id_with_invalid_args() {
    let args = ForgetArgs::Session {
        session_id: String::new(),
    };
    match sdk().forget(&args).expect_err("must reject") {
        SdkError::InvalidArgs { reason } => {
            assert!(reason.contains("session_id"), "reason: {reason}");
        }
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[test]
fn retrieve_rejects_unadvertised_target_with_capability_unavailable() {
    let err = sdk()
        .retrieve(&RetrieveArgs::Record { id: ulid() })
        .expect_err("must fail closed in P0");
    match err {
        SdkError::CapabilityUnavailable { capability, .. } => {
            assert_eq!(capability, "cairn.mcp.v1.retrieve.record");
        }
        other => panic!("expected CapabilityUnavailable, got {other:?}"),
    }
}

#[test]
fn summarize_returns_internal_stub() {
    let args = SummarizeArgs {
        citations: None,
        kind: None,
        persist: None,
        record_ids: vec![ulid()],
    };
    assert_unimplemented("summarize", sdk().summarize(&args));
}

#[test]
fn assemble_hot_returns_internal_stub() {
    let args = AssembleHotArgs {
        budget: None,
        session_id: None,
    };
    assert_unimplemented("assemble_hot", sdk().assemble_hot(&args));
}

#[test]
fn capture_trace_returns_internal_stub() {
    let args = CaptureTraceArgs {
        from: "/tmp/trace.log".to_owned(),
        session_id: None,
    };
    assert_unimplemented("capture_trace", sdk().capture_trace(&args));
}

#[test]
fn lint_returns_internal_stub() {
    let args = LintArgs { write_report: None };
    assert_unimplemented("lint", sdk().lint(&args));
}

#[test]
fn sdk_error_code_helper_returns_typed_code() {
    // CapabilityUnavailable carries a typed wire code so callers can branch
    // without parsing strings.
    let cap_err = sdk()
        .retrieve(&RetrieveArgs::Record { id: ulid() })
        .expect_err("cap");
    assert_eq!(cap_err.code(), Some(ErrorCode::CapabilityUnavailable));

    // Unimplemented and InvalidArgs are SDK-side rejections without a wire
    // round-trip — they have no wire code.
    let unimpl = sdk()
        .ingest(&IngestArgs {
            body: Some("note".to_owned()),
            file: None,
            frontmatter: None,
            kind: "note".to_owned(),
            session_id: None,
            tags: None,
            url: None,
        })
        .expect_err("stub");
    assert!(matches!(unimpl, SdkError::Unimplemented { .. }));
    assert_eq!(unimpl.code(), None);

    let invalid = sdk()
        .ingest(&IngestArgs {
            body: Some("a".to_owned()),
            file: Some("b".to_owned()),
            frontmatter: None,
            kind: "note".to_owned(),
            session_id: None,
            tags: None,
            url: None,
        })
        .expect_err("invalid");
    assert!(matches!(invalid, SdkError::InvalidArgs { .. }));
    assert_eq!(invalid.code(), None);
}

#[test]
fn forget_rejects_unadvertised_target_with_capability_unavailable() {
    let err = sdk()
        .forget(&ForgetArgs::Record { record_id: ulid() })
        .expect_err("must fail closed in P0");
    match err {
        SdkError::CapabilityUnavailable { capability, .. } => {
            assert_eq!(capability, "cairn.mcp.v1.forget.record");
        }
        other => panic!("expected CapabilityUnavailable, got {other:?}"),
    }
}

#[track_caller]
fn assert_unimplemented<T: std::fmt::Debug>(verb: &'static str, result: Result<T, SdkError>) {
    let err = result.expect_err("P0 stubs must error until #9 wires the store");
    match err {
        SdkError::Unimplemented {
            verb: actual,
            tracking,
            operation_id,
        } => {
            assert_eq!(actual, verb);
            assert!(tracking.contains("#9"), "tracking: {tracking}");
            assert_eq!(operation_id.0.len(), 26, "operation_id is a ULID");
        }
        other => panic!("expected Unimplemented, got {other:?}"),
    }
}
