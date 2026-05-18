//! SDK surface tests.
//!
//! Verifies the acceptance criteria from issue #60:
//! - SDK consumers can call every P0 verb and receive typed results.
//! - SDK version reports the same protocol capability data as `status`.
//! - Typed errors surface for unsupported capabilities (P0 stub: store
//!   not wired).
//! - SDK responses serialize into the same envelope shape the CLI emits.

use cairn_core::status::default_sensor_capabilities;
use cairn_sdk::error::ErrorCode;
use cairn_sdk::generated::common::{Cursor, ScopeFilter, Ulid};
use cairn_sdk::generated::envelope::ResponseVerb;
use cairn_sdk::generated::verbs::ingest::IngestData;
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
use cairn_sdk::{Sdk, SdkError, VerbResponse, version};

fn sdk() -> Sdk {
    Sdk::new()
}

fn ulid() -> Ulid {
    // Crockford-base32 fixture; structurally valid (26 chars, allowed alphabet).
    Ulid("01HZZ0000000000000000000AB".to_owned())
}

fn ingest_body_args(body: &str) -> IngestArgs {
    IngestArgs {
        batch_size: None,
        body: Some(body.to_owned()),
        dry_run: None,
        exclude: None,
        file: None,
        folder: None,
        frontmatter: None,
        human_review: None,
        include: None,
        kind: "note".to_owned(),
        mode: None,
        no_cache: None,
        no_diff: None,
        recursive: None,
        session_id: None,
        tags: None,
        url: None,
        jsonl: None,
        recording: None,
        harness: None,
        session_id_from: None,
        limit: None,
    }
}

#[test]
fn version_matches_status_server_info() {
    let resp = sdk().status();
    assert_eq!(resp.server_info.version, version());
    assert_eq!(resp.contract, "cairn.mcp.v1");
}

#[test]
fn status_mints_fresh_incarnation_per_call_matching_cli() {
    // P0 parity (issue #60): until the daemon-backed incarnation table
    // lands (issue #9), both `cairn status` and `Sdk::status()` mint a
    // fresh incarnation ULID per invocation. Asserting freshness here
    // pins the cross-surface contract so future drift is caught.
    let s = sdk();
    let a = s.status();
    let b = s.status();
    assert_ne!(
        a.server_info.incarnation, b.server_info.incarnation,
        "incarnation must be minted per call to match CLI"
    );
    // started_at is RFC-3339 with second precision, so two back-to-back
    // calls usually share the same value — assert only that the field is
    // populated and well-formed.
    assert_eq!(a.server_info.started_at.len(), 20);
    assert!(a.server_info.started_at.ends_with('Z'));
}

#[test]
fn verb_response_serializes_as_canonical_envelope() {
    // VerbResponse must round-trip into the wire envelope shape (brief
    // §8.0.b): contract, status=committed, verb, operation_id,
    // policy_trace, data. Adapters and observability code can then
    // forward SDK successes over MCP without hand-rolling serialization.
    let resp: VerbResponse<IngestData> = VerbResponse {
        operation_id: ulid(),
        policy_trace: vec![],
        verb: ResponseVerb::Ingest,
        target: None,
        data: IngestData {
            cache_hits: None,
            cache_misses: None,
            cache_writes: None,
            files_processed: None,
            record_id: ulid(),
            session_id: "sess-1".to_owned(),
            plan_ref: None,
            jsonl_summary: None,
            recording_summary: None,
        },
    };
    let value = serde_json::to_value(&resp).expect("serializes");
    let obj = value.as_object().expect("envelope is object");
    assert_eq!(
        obj.get("contract").and_then(|v| v.as_str()),
        Some("cairn.mcp.v1")
    );
    assert_eq!(
        obj.get("status").and_then(|v| v.as_str()),
        Some("committed")
    );
    assert_eq!(obj.get("verb").and_then(|v| v.as_str()), Some("ingest"));
    for k in ["operation_id", "policy_trace", "data"] {
        assert!(obj.contains_key(k), "envelope missing {k}");
    }
    assert!(obj["data"].is_object());
    // Non-retrieve verbs must NOT emit `target` (schema rejects it elsewhere).
    assert!(!obj.contains_key("target"));
}

#[test]
fn verb_response_rejects_envelope_invalid_target_combinations() {
    use cairn_sdk::generated::envelope::ResponseTarget;
    // verb=retrieve without target is rejected by the wire envelope —
    // the SDK's Serialize impl must surface that as an error rather than
    // emit malformed JSON.
    let missing: VerbResponse<serde_json::Value> = VerbResponse {
        operation_id: ulid(),
        policy_trace: vec![],
        verb: ResponseVerb::Retrieve,
        target: None,
        data: serde_json::json!({}),
    };
    assert!(serde_json::to_value(&missing).is_err());

    // target set on a non-retrieve verb is also rejected.
    let stray: VerbResponse<IngestData> = VerbResponse {
        operation_id: ulid(),
        policy_trace: vec![],
        verb: ResponseVerb::Ingest,
        target: Some(ResponseTarget::Record),
        data: IngestData {
            cache_hits: None,
            cache_misses: None,
            cache_writes: None,
            files_processed: None,
            record_id: ulid(),
            session_id: "s".to_owned(),
            plan_ref: None,
            jsonl_summary: None,
            recording_summary: None,
        },
    };
    assert!(serde_json::to_value(&stray).is_err());
}

#[test]
fn verb_response_rejects_unknown_verb_on_committed_envelope() {
    // The `unknown` sentinel is only valid on rejected responses with
    // error.code=UnknownVerb. A committed VerbResponse must name a real
    // verb — surface mistakes as Serialize errors before they reach the
    // wire.
    let resp: VerbResponse<serde_json::Value> = VerbResponse {
        operation_id: ulid(),
        policy_trace: vec![],
        verb: ResponseVerb::Unknown,
        target: None,
        data: serde_json::json!({}),
    };
    assert!(serde_json::to_value(&resp).is_err());
}

#[test]
fn verb_response_emits_target_for_retrieve_envelope() {
    // Wire envelope requires `target` on every committed verb=retrieve
    // response and forbids it elsewhere — see Response.target in
    // cairn_core::generated::envelope.
    use cairn_sdk::generated::envelope::ResponseTarget;
    let resp: VerbResponse<serde_json::Value> = VerbResponse {
        operation_id: ulid(),
        policy_trace: vec![],
        verb: ResponseVerb::Retrieve,
        target: Some(ResponseTarget::Record),
        data: serde_json::json!({}),
    };
    let value = serde_json::to_value(&resp).expect("serializes");
    assert_eq!(value["verb"].as_str(), Some("retrieve"));
    assert_eq!(value["target"].as_str(), Some("record"));
}

#[test]
fn sdk_new_advertises_no_capabilities() {
    let resp = sdk().status();
    assert_eq!(resp.capabilities, default_sensor_capabilities());
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
    let a = s
        .handshake()
        .expect("handshake under normal clock must succeed");
    let b = s
        .handshake()
        .expect("handshake under normal clock must succeed");
    assert_eq!(a.contract, "cairn.mcp.v1");
    assert_ne!(a.challenge.nonce.0, b.challenge.nonce.0);
    assert_eq!(a.challenge.nonce.0.len(), 24);
    assert!(a.challenge.expires_at > 0);
}

#[test]
fn ingest_invalid_args_returns_typed_error() {
    // Violate exactly-one-of: pass body AND file.
    let args = IngestArgs {
        file: Some("/tmp/x".to_owned()),
        ..ingest_body_args("note")
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
    let args = ingest_body_args("note");
    assert_unimplemented("ingest", sdk().ingest(&args));
}

#[test]
#[allow(clippy::too_many_lines)] // table-driven sweep — each case is a single-line builder, not real fn growth
fn ingest_rejects_schema_minlength_violations() {
    // The IDL `validate()` only enforces the source XOR, but the schema
    // additionally requires non-empty body, file, folder, url, recording, kind,
    // session_id, include/exclude/tags items, and a bounded batch_size.
    // Direct Rust construction must hit the same floor.
    let bases = || ingest_body_args("note");
    let cases: [(&str, IngestArgs); 22] = [
        (
            "body",
            IngestArgs {
                body: Some(String::new()),
                ..bases()
            },
        ),
        (
            "file",
            IngestArgs {
                body: None,
                file: Some(String::new()),
                ..bases()
            },
        ),
        (
            "url",
            IngestArgs {
                body: None,
                url: Some(String::new()),
                ..bases()
            },
        ),
        (
            "folder",
            IngestArgs {
                body: None,
                folder: Some(String::new()),
                ..bases()
            },
        ),
        (
            "recording",
            IngestArgs {
                body: None,
                recording: Some(String::new()),
                ..bases()
            },
        ),
        (
            "include",
            IngestArgs {
                include: Some(vec![String::new()]),
                ..bases()
            },
        ),
        (
            "exclude",
            IngestArgs {
                exclude: Some(vec![String::new()]),
                ..bases()
            },
        ),
        (
            "batch_size",
            IngestArgs {
                batch_size: Some(0),
                ..bases()
            },
        ),
        (
            "batch_size",
            IngestArgs {
                batch_size: Some(65_536),
                ..bases()
            },
        ),
        (
            "url",
            IngestArgs {
                body: None,
                url: Some("not-a-uri".to_owned()),
                ..bases()
            },
        ),
        // Schemed-but-empty hier-part / colon-only / scheme-only / leading-digit:
        (
            "url",
            IngestArgs {
                body: None,
                url: Some("http:".to_owned()),
                ..bases()
            },
        ),
        (
            "url",
            IngestArgs {
                body: None,
                url: Some(":rest".to_owned()),
                ..bases()
            },
        ),
        (
            "url",
            IngestArgs {
                body: None,
                url: Some("1bad:rest".to_owned()),
                ..bases()
            },
        ),
        // Whitespace / control chars in any position must reject:
        (
            "url",
            IngestArgs {
                body: None,
                url: Some("http: ".to_owned()),
                ..bases()
            },
        ),
        (
            "url",
            IngestArgs {
                body: None,
                url: Some("http:\nfoo".to_owned()),
                ..bases()
            },
        ),
        (
            "url",
            IngestArgs {
                body: None,
                url: Some("http:\tfoo".to_owned()),
                ..bases()
            },
        ),
        (
            "url",
            IngestArgs {
                body: None,
                url: Some("http:\u{0007}foo".to_owned()),
                ..bases()
            },
        ),
        // Raw non-ASCII per RFC 3986 §2.1:
        (
            "url",
            IngestArgs {
                body: None,
                url: Some("http://example.com/💥".to_owned()),
                ..bases()
            },
        ),
        (
            "kind",
            IngestArgs {
                kind: String::new(),
                ..bases()
            },
        ),
        (
            "session_id",
            IngestArgs {
                session_id: Some(String::new()),
                ..bases()
            },
        ),
        (
            "tags",
            IngestArgs {
                tags: Some(vec![String::new()]),
                ..bases()
            },
        ),
        (
            "frontmatter",
            IngestArgs {
                frontmatter: Some(serde_json::json!([1, 2])),
                ..bases()
            },
        ),
    ];
    for (needle, args) in cases {
        match sdk().ingest(&args).expect_err("must reject") {
            SdkError::InvalidArgs { reason } => {
                assert!(
                    reason.contains(needle),
                    "reason {reason:?} missing {needle:?}"
                );
            }
            other => panic!("expected InvalidArgs for {needle}, got {other:?}"),
        }
    }
}

#[test]
fn ingest_recording_conflicts_with_other_sources() {
    let args = IngestArgs {
        recording: Some("meeting.mp4".to_owned()),
        ..ingest_body_args("note")
    };
    match sdk().ingest(&args).expect_err("must reject") {
        SdkError::InvalidArgs { reason } => {
            assert!(reason.contains("exactly one of"), "reason: {reason}");
            assert!(reason.contains("recording"), "reason: {reason}");
        }
        other => panic!("expected InvalidArgs, got {other:?}"),
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
            batch_size: None,
            body: None,
            dry_run: None,
            exclude: None,
            file: None,
            folder: None,
            frontmatter: None,
            human_review: None,
            include: None,
            kind: "note".to_owned(),
            mode: None,
            no_cache: None,
            no_diff: None,
            recursive: None,
            session_id: None,
            tags: None,
            url: Some(url.to_owned()),
            jsonl: None,
            recording: None,
            harness: None,
            session_id_from: None,
            limit: None,
        };
        assert_unimplemented("ingest", sdk().ingest(&args));
    }
}

#[tokio::test]
async fn search_rejects_empty_query_with_invalid_args() {
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
        explain: None,
        include_reasoning: None,
    };
    match sdk().search(&args).await.expect_err("must reject") {
        SdkError::InvalidArgs { reason } => {
            assert!(reason.contains("query"), "reason: {reason}");
        }
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[tokio::test]
async fn search_rejects_out_of_range_limit_with_invalid_args() {
    let args = SearchArgs {
        citations: None,
        cursor: None,
        filters: None,
        limit: Some(0),
        mode: SearchArgsMode::Keyword,
        query: "hello".to_owned(),
        scope: None,
        explain: None,
        include_reasoning: None,
    };
    match sdk().search(&args).await.expect_err("must reject") {
        SdkError::InvalidArgs { reason } => {
            assert!(reason.contains("limit"), "reason: {reason}");
        }
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[tokio::test]
async fn search_explain_rejects_when_policy_trace_capability_unadvertised() {
    // `Sdk::new` advertises no capabilities (see
    // `sdk_new_advertises_no_capabilities`). `args.explain == Some(true)`
    // is gated on `cairn.mcp.v1.policy_trace` per the
    // `x-cairn-capability-when-true` annotation in
    // `crates/cairn-idl/schema/verbs/search.json`; the dispatcher
    // additionally requires the search-mode capability. Either missing
    // capability is a valid fail-closed signal.
    let args = SearchArgs {
        citations: None,
        cursor: None,
        filters: None,
        limit: None,
        mode: SearchArgsMode::Keyword,
        query: "hello".to_owned(),
        scope: None,
        explain: Some(true),
        include_reasoning: None,
    };
    let err = sdk()
        .search(&args)
        .await
        .expect_err("no store wired → must fail closed");
    match err {
        SdkError::CapabilityUnavailable { capability, .. } => {
            assert!(
                capability == "cairn.mcp.v1.search.keyword"
                    || capability == "cairn.mcp.v1.policy_trace",
                "expected mode or policy_trace capability error; got {capability}"
            );
        }
        other => panic!("expected CapabilityUnavailable, got {other:?}"),
    }
}

#[tokio::test]
async fn search_explain_false_rejects_unadvertised_keyword_mode() {
    // `explain: Some(false)` must NOT trigger the policy_trace gate
    // (only `Some(true)` does). With no store wired, the keyword mode
    // is also unadvertised, so the call still fails closed — but the
    // failing capability must be the search mode, not policy_trace.
    let args = SearchArgs {
        citations: None,
        cursor: None,
        filters: None,
        limit: None,
        mode: SearchArgsMode::Keyword,
        query: "hello".to_owned(),
        scope: None,
        explain: Some(false),
        include_reasoning: None,
    };
    let err = sdk()
        .search(&args)
        .await
        .expect_err("no store wired → must fail closed");
    match err {
        SdkError::CapabilityUnavailable { capability, .. } => {
            assert_eq!(
                capability, "cairn.mcp.v1.search.keyword",
                "explain=false must surface the mode capability, not policy_trace"
            );
        }
        other => panic!("expected CapabilityUnavailable, got {other:?}"),
    }
}

#[tokio::test]
async fn search_rejects_unadvertised_modes_with_capability_unavailable() {
    // `Sdk::new` advertises no capabilities (no store wired). Every
    // search mode must therefore fail closed with CapabilityUnavailable
    // rather than the generic Internal/Unimplemented stub. Mirrors the
    // original P0 contract restored after the round-1 review found that
    // `Sdk::new` was over-advertising defaults.
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
            explain: None,
            include_reasoning: None,
        };
        let err = sdk()
            .search(&args)
            .await
            .expect_err("no store wired → must fail closed");
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
fn retrieve_tool_call_rejects_empty_fields_with_invalid_args() {
    let args = RetrieveArgs::ToolCall {
        session_id: String::new(),
        turn_id: "turn-1".to_owned(),
        tool_call_id: "call-1".to_owned(),
    };
    match sdk().retrieve(&args).expect_err("must reject") {
        SdkError::InvalidArgs { reason } => {
            assert!(reason.contains("session_id"), "reason: {reason}");
        }
        other => panic!("expected InvalidArgs, got {other:?}"),
    }

    let args = RetrieveArgs::ToolCall {
        session_id: "session-1".to_owned(),
        turn_id: String::new(),
        tool_call_id: "call-1".to_owned(),
    };
    match sdk().retrieve(&args).expect_err("must reject") {
        SdkError::InvalidArgs { reason } => {
            assert!(reason.contains("turn_id"), "reason: {reason}");
        }
        other => panic!("expected InvalidArgs, got {other:?}"),
    }

    let args = RetrieveArgs::ToolCall {
        session_id: "session-1".to_owned(),
        turn_id: "turn-1".to_owned(),
        tool_call_id: String::new(),
    };
    match sdk().retrieve(&args).expect_err("must reject") {
        SdkError::InvalidArgs { reason } => {
            assert!(reason.contains("tool_call_id"), "reason: {reason}");
        }
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[tokio::test]
async fn search_rejects_empty_and_filter_with_invalid_args() {
    let args = SearchArgs {
        citations: None,
        cursor: None,
        filters: Some(SearchArgsFilters::And { and: vec![] }),
        limit: None,
        mode: SearchArgsMode::Keyword,
        query: "hi".to_owned(),
        scope: None,
        explain: None,
        include_reasoning: None,
    };
    match sdk().search(&args).await.expect_err("must reject") {
        SdkError::InvalidArgs { reason } => {
            assert!(reason.contains("filter.and"), "reason: {reason}");
        }
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[tokio::test]
async fn search_rejects_excessive_filter_depth_with_invalid_args() {
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
        explain: None,
        include_reasoning: None,
    };
    match sdk().search(&args).await.expect_err("must reject") {
        SdkError::InvalidArgs { reason } => {
            assert!(reason.contains("max boolean depth"), "reason: {reason}");
        }
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[tokio::test]
async fn search_rejects_malformed_filter_leaf_with_invalid_args() {
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
        explain: None,
        include_reasoning: None,
    };
    match sdk().search(&args).await.expect_err("must reject") {
        SdkError::InvalidArgs { reason } => {
            assert!(reason.contains("field"), "reason: {reason}");
        }
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[tokio::test]
async fn search_accepts_extended_filter_operators() {
    // Mirrors the generated grammar: between, array_contains,
    // array_contains_any/all, and array_size_eq must validate cleanly.
    // `Sdk::new` advertises no capabilities, so the call lands on
    // CapabilityUnavailable — the point is that leaf validation passed
    // before the fail-closed gate triggered.
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
            explain: None,
            include_reasoning: None,
        };
        match sdk()
            .search(&args)
            .await
            .expect_err("no store wired → CapabilityUnavailable")
        {
            SdkError::CapabilityUnavailable { .. } => {}
            other => panic!("expected CapabilityUnavailable for {leaf:?}, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn search_rejects_malformed_extended_filter_operators_with_invalid_args() {
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
        // `exists` is not part of the canonical filter grammar — must reject.
        serde_json::json!({"field": "x", "op": "exists", "value": true}),
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
            explain: None,
            include_reasoning: None,
        };
        match sdk().search(&args).await.expect_err("must reject") {
            SdkError::InvalidArgs { .. } => {}
            other => panic!("expected InvalidArgs for {leaf:?}, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn search_rejects_malformed_cursor_with_invalid_args() {
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
        explain: None,
        include_reasoning: None,
    };
    match sdk().search(&args).await.expect_err("must reject") {
        SdkError::InvalidArgs { reason } => assert!(reason.contains("Cursor"), "reason: {reason}"),
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[tokio::test]
async fn search_rejects_empty_scope_filter_with_invalid_args() {
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
        explain: None,
        include_reasoning: None,
    };
    match sdk().search(&args).await.expect_err("must reject") {
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
        dry_run: None,
        human_review: None,
        no_diff: None,
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
        recipe: None,
        session_id: None,
        explain: None,
    };
    match sdk().assemble_hot(&args).expect_err("must reject") {
        SdkError::InvalidArgs { reason } => assert!(reason.contains("budget"), "reason: {reason}"),
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[test]
fn capture_trace_rejects_empty_from_with_invalid_args() {
    let args = CaptureTraceArgs {
        from: Some(String::new()),
        blocks: None,
        session_id: None,
    };
    match sdk().capture_trace(&args).expect_err("must reject") {
        SdkError::InvalidArgs { reason } => assert!(reason.contains("from"), "reason: {reason}"),
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[test]
fn capture_trace_rejects_from_plus_session_with_invalid_args() {
    let args = CaptureTraceArgs {
        from: Some("/tmp/trace.log".to_owned()),
        blocks: None,
        session_id: Some("01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned()),
    };
    match sdk().capture_trace(&args).expect_err("must reject") {
        SdkError::InvalidArgs { reason } => {
            assert!(reason.contains("session_id"), "reason: {reason}");
        }
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[test]
fn forget_session_rejects_empty_session_id_with_invalid_args() {
    let args = ForgetArgs::Session {
        session_id: String::new(),
        dry_run: None,
        human_review: None,
        no_diff: None,
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
fn assemble_hot_rejects_any_budget_until_loader_lands() {
    // Stub-body assembler cannot honor budget yet; SDK must fail explicitly
    // rather than silently drop a knob the caller asked to enforce.
    let args = AssembleHotArgs {
        budget: Some(1024),
        recipe: None,
        session_id: None,
        explain: None,
    };
    match sdk().assemble_hot(&args).expect_err("must reject") {
        SdkError::InvalidArgs { reason } => {
            assert!(reason.contains("budget"), "reason: {reason}");
            assert!(reason.contains("not yet honored"), "reason: {reason}");
        }
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[test]
fn assemble_hot_rejects_any_session_id_until_loader_lands() {
    let args = AssembleHotArgs {
        budget: None,
        recipe: None,
        session_id: Some("01J0000000000000000000000A".to_owned()),
        explain: None,
    };
    match sdk().assemble_hot(&args).expect_err("must reject") {
        SdkError::InvalidArgs { reason } => {
            assert!(reason.contains("session_id"), "reason: {reason}");
            assert!(reason.contains("not yet honored"), "reason: {reason}");
        }
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[test]
fn assemble_hot_returns_unimplemented_in_sdk() {
    // The SDK cannot safely couple a vault root to the supplied config
    // from this layer — that wiring lands with #193. Until then, every
    // SDK construction returns Unimplemented for assemble_hot; the CLI
    // is the canonical surface (it loads config from the same root it
    // probes, so vault-binding cannot diverge).
    let args = AssembleHotArgs {
        budget: None,
        recipe: None,
        session_id: None,
        explain: None,
    };
    assert_unimplemented("assemble_hot", sdk().assemble_hot(&args));

    // Wiring a store does not change the answer — config and vault.id
    // would still be decoupled.
    let sdk_wired = Sdk::with_store(
        std::sync::Arc::new(noop_store::NoopStore),
        cairn_core::config::CairnConfig::default(),
    );
    assert_unimplemented("assemble_hot", sdk_wired.assemble_hot(&args));
}

#[test]
fn capture_trace_returns_internal_stub() {
    let args = CaptureTraceArgs {
        from: Some("/tmp/trace.log".to_owned()),
        blocks: None,
        session_id: None,
    };
    assert_unimplemented("capture_trace", sdk().capture_trace(&args));
}

#[test]
fn lint_returns_internal_stub() {
    let args = LintArgs {
        fix: None,
        write_report: None,
    };
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
    let unimpl = sdk().ingest(&ingest_body_args("note")).expect_err("stub");
    assert!(matches!(unimpl, SdkError::Unimplemented { .. }));
    assert_eq!(unimpl.code(), None);

    let invalid = sdk()
        .ingest(&IngestArgs {
            file: Some("b".to_owned()),
            ..ingest_body_args("a")
        })
        .expect_err("invalid");
    assert!(matches!(invalid, SdkError::InvalidArgs { .. }));
    assert_eq!(invalid.code(), None);
}

#[test]
fn forget_rejects_unadvertised_target_with_capability_unavailable() {
    let sdk = Sdk::with_store(
        std::sync::Arc::new(noop_store::NoopStore),
        cairn_core::config::CairnConfig::default(),
    );
    assert!(
        !sdk.status()
            .capabilities
            .contains(&cairn_sdk::generated::common::Capabilities::CairnMcpV1ForgetRecord),
        "SDK status must not advertise forget.record until SDK dispatch is wired"
    );

    let err = sdk
        .forget(&ForgetArgs::Record {
            record_id: ulid(),
            dry_run: None,
            human_review: None,
            no_diff: None,
        })
        .expect_err("must fail closed in P0");
    match err {
        SdkError::CapabilityUnavailable { capability, .. } => {
            assert_eq!(capability, "cairn.mcp.v1.forget.record");
        }
        other => panic!("expected CapabilityUnavailable, got {other:?}"),
    }
}

#[test]
fn retrieve_capabilities_filtered_until_sdk_dispatch_is_wired() {
    let sdk = Sdk::with_store(
        std::sync::Arc::new(noop_store::NoopStore),
        cairn_core::config::CairnConfig::default(),
    );
    let status = sdk.status();
    for cap in [
        cairn_sdk::generated::common::Capabilities::CairnMcpV1RetrieveSession,
        cairn_sdk::generated::common::Capabilities::CairnMcpV1RetrieveTurn,
        cairn_sdk::generated::common::Capabilities::CairnMcpV1RetrieveToolCall,
    ] {
        assert!(
            !status.capabilities.contains(&cap),
            "SDK status must not advertise {cap:?} until SDK retrieve dispatch is wired"
        );
    }

    let err = sdk
        .retrieve(&RetrieveArgs::Session {
            cursor: None,
            include: None,
            include_reasoning: None,
            limit: None,
            order: None,
            rehydrate: None,
            session_id: "session-1".to_owned(),
        })
        .expect_err("must fail closed until SDK retrieve dispatch is wired");
    match err {
        SdkError::CapabilityUnavailable { capability, .. } => {
            assert_eq!(capability, "cairn.mcp.v1.retrieve.session");
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

mod noop_store {
    //! Stub `MemoryStore` used to gate `assemble_hot` behind a wired store
    //! without exercising any store method (`assemble_hot` never calls into
    //! the store; it just checks `is_some()`). Every method panics.

    use cairn_core::contract::memory_store::CONTRACT_VERSION;
    use cairn_core::contract::memory_store::{
        Edge, EdgeDir, EdgeKey, HybridSearchArgs, HybridSearchPage, KeywordSearchArgs,
        KeywordSearchPage, ListArgs, ListPage, MemoryStore, MemoryStoreCapabilities, RecordVersion,
        SemanticSearchArgs, SemanticSearchPage, StoreError, TombstoneReason, UpsertOutcome,
    };
    use cairn_core::contract::version::{ContractVersion, VersionRange};
    use cairn_core::domain::record::MemoryRecord;
    use cairn_core::domain::{RecordId, TargetId};

    pub struct NoopStore;

    #[async_trait::async_trait]
    impl MemoryStore for NoopStore {
        fn name(&self) -> &'static str {
            "noop"
        }
        fn capabilities(&self) -> &MemoryStoreCapabilities {
            static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
                fts: false,
                vector: false,
                graph_edges: false,
                transactions: false,
                per_record_consent_model: false,
                graph_search: false,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            VersionRange::new(
                CONTRACT_VERSION,
                ContractVersion::new(CONTRACT_VERSION.major, CONTRACT_VERSION.minor + 1, 0),
            )
        }
        async fn upsert(&self, _r: &MemoryRecord) -> Result<UpsertOutcome, StoreError> {
            unimplemented!("NoopStore: upsert")
        }
        async fn get(&self, _id: &RecordId) -> Result<Option<MemoryRecord>, StoreError> {
            unimplemented!("NoopStore: get")
        }
        async fn list(&self, _args: &ListArgs) -> Result<ListPage, StoreError> {
            unimplemented!("NoopStore: list")
        }
        async fn tombstone(
            &self,
            _id: &RecordId,
            _reason: TombstoneReason,
        ) -> Result<(), StoreError> {
            unimplemented!("NoopStore: tombstone")
        }
        async fn versions(&self, _target: &TargetId) -> Result<Vec<RecordVersion>, StoreError> {
            unimplemented!("NoopStore: versions")
        }
        async fn put_edge(&self, _edge: &Edge) -> Result<(), StoreError> {
            unimplemented!("NoopStore: put_edge")
        }
        async fn remove_edge(&self, _key: &EdgeKey) -> Result<bool, StoreError> {
            unimplemented!("NoopStore: remove_edge")
        }
        async fn neighbours(&self, _id: &RecordId, _dir: EdgeDir) -> Result<Vec<Edge>, StoreError> {
            unimplemented!("NoopStore: neighbours")
        }
        async fn search_keyword(
            &self,
            _args: &KeywordSearchArgs<'_>,
        ) -> Result<KeywordSearchPage, StoreError> {
            unimplemented!("NoopStore: search_keyword")
        }
        async fn search_semantic(
            &self,
            _args: &SemanticSearchArgs<'_>,
        ) -> Result<SemanticSearchPage, StoreError> {
            unimplemented!("NoopStore: search_semantic")
        }
        async fn search_hybrid(
            &self,
            _args: &HybridSearchArgs<'_>,
        ) -> Result<HybridSearchPage, StoreError> {
            unimplemented!("NoopStore: search_hybrid")
        }
    }
}
