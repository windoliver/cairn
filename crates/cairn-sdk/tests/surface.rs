//! SDK surface tests.
//!
//! Verifies the acceptance criteria from issue #60:
//! - SDK consumers can call every P0 verb and receive typed results.
//! - SDK version reports the same protocol capability data as `status`.
//! - Typed errors surface for unsupported capabilities (P0 stub: store
//!   not wired).
//! - SDK responses serialize into the same envelope shape the CLI emits.

use cairn_sdk::error::ErrorCode;
use cairn_sdk::generated::common::Ulid;
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
    assert_internal_stub(sdk().ingest(&args));
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
    assert_internal_stub(sdk().summarize(&args));
}

#[test]
fn assemble_hot_returns_internal_stub() {
    let args = AssembleHotArgs {
        budget: None,
        session_id: None,
    };
    assert_internal_stub(sdk().assemble_hot(&args));
}

#[test]
fn capture_trace_returns_internal_stub() {
    let args = CaptureTraceArgs {
        from: "/tmp/trace.log".to_owned(),
        session_id: None,
    };
    assert_internal_stub(sdk().capture_trace(&args));
}

#[test]
fn lint_returns_internal_stub() {
    let args = LintArgs { write_report: None };
    assert_internal_stub(sdk().lint(&args));
}

#[test]
fn sdk_error_code_helper_returns_typed_code() {
    // SdkError::code() lets callers branch on ErrorCode without parsing
    // strings, satisfying the closed-typed-error contract.
    let p0_stub = sdk()
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
    assert_eq!(p0_stub.code(), Some(ErrorCode::Internal));

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
    // Pre-envelope rejections have no wire code.
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
fn assert_internal_stub<T: std::fmt::Debug>(result: Result<T, SdkError>) {
    let err = result.expect_err("P0 stubs must error until #9 wires the store");
    match err {
        SdkError::Protocol {
            code,
            message,
            operation_id,
        } => {
            assert_eq!(code, ErrorCode::Internal);
            assert!(
                message.contains("store not wired"),
                "message must mention store: {message}"
            );
            assert_eq!(operation_id.0.len(), 26, "operation_id is a ULID");
        }
        other => panic!("expected Protocol Internal stub, got {other:?}"),
    }
}
