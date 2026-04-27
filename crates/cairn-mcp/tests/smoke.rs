// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use cairn_core as _;

use cairn_mcp::error::McpTransportError;

#[test]
fn transport_error_displays() {
    let e = McpTransportError::Initialize("handshake failed".to_owned());
    assert!(e.to_string().contains("initialize"), "error display: {e}");
}

use cairn_core::generated::envelope::{ResponseStatus, ResponseVerb};

#[test]
fn dispatch_ingest_returns_aborted_p0() {
    let resp = cairn_mcp::dispatch::dispatch("ingest", None);
    assert_eq!(resp.contract, "cairn.mcp.v1");
    assert!(
        matches!(resp.status, ResponseStatus::Aborted),
        "P0 must return Aborted (store not wired): {:?}",
        resp.status
    );
    assert!(
        matches!(resp.verb, ResponseVerb::Ingest),
        "verb echo must be Ingest: {:?}",
        resp.verb
    );
    let err = resp.error.expect("Aborted response must have error");
    assert_eq!(err["code"], "Internal");
}

#[test]
fn dispatch_unknown_verb_returns_rejected() {
    let resp = cairn_mcp::dispatch::dispatch("not_a_real_verb", None);
    assert!(
        matches!(resp.verb, ResponseVerb::Unknown),
        "unrecognized tool name must produce verb=unknown: {:?}",
        resp.verb
    );
    assert!(
        matches!(resp.status, ResponseStatus::Rejected),
        "verb=unknown must be Rejected: {:?}",
        resp.status
    );
    let err = resp.error.expect("Rejected response must have error");
    assert_eq!(err["code"], "UnknownVerb");
}

#[test]
fn dispatch_all_eight_verbs_do_not_panic() {
    for name in [
        "ingest",
        "search",
        "retrieve",
        "summarize",
        "assemble_hot",
        "capture_trace",
        "lint",
        "forget",
    ] {
        let resp = cairn_mcp::dispatch::dispatch(name, None);
        assert_eq!(resp.contract, "cairn.mcp.v1", "bad contract for {name}");
        assert!(
            matches!(resp.status, ResponseStatus::Aborted),
            "{name} must be Aborted at P0: {:?}",
            resp.status
        );
    }
}
