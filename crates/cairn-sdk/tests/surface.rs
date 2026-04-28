//! SDK surface tests.
//!
//! Verifies the acceptance criteria from issue #60:
//! - SDK consumers can call every P0 verb and receive typed results.
//! - SDK version reports the same protocol capability data as `status`.
//! - Typed errors surface for unsupported capabilities (P0 stub: store
//!   not wired).
//! - SDK responses serialize into the same envelope shape the CLI emits.

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
fn search_returns_internal_stub() {
    let args = SearchArgs {
        citations: None,
        cursor: None,
        filters: None,
        limit: None,
        mode: SearchArgsMode::Keyword,
        query: "hello".to_owned(),
        scope: None,
    };
    assert_internal_stub(sdk().search(&args));
}

#[test]
fn retrieve_returns_internal_stub() {
    let args = RetrieveArgs::Record { id: ulid() };
    assert_internal_stub(sdk().retrieve(&args));
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
fn forget_returns_internal_stub() {
    let args = ForgetArgs::Record { record_id: ulid() };
    assert_internal_stub(sdk().forget(&args));
}

#[track_caller]
fn assert_internal_stub<T: std::fmt::Debug>(result: Result<T, SdkError>) {
    let err = result.expect_err("P0 stubs must error until #9 wires the store");
    match err {
        SdkError::Internal {
            code,
            message,
            operation_id,
        } => {
            assert_eq!(code, "Internal");
            assert!(
                message.contains("store not wired"),
                "message must mention store: {message}"
            );
            assert_eq!(operation_id.0.len(), 26, "operation_id is a ULID");
        }
        other => panic!("expected Internal stub, got {other:?}"),
    }
}
