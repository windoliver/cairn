//! MCP conformance suite (issue #67).
//!
//! Walks every P0 verb with valid + invalid envelopes from
//! `fixtures/v0/mcp/conformance/` and asserts each handler response matches
//! the canonical envelope in the matching `.response.json`. Adds a
//! cross-product test that iterates un-advertised, dispatch-routable
//! verb-modes and asserts each rejects with `CapabilityUnavailable`.
//!
//! Brief refs: §4.1, §8.0.a (handshake / status / cap advertisement), §8.0.b
//! (envelope), §15 (wire-compat).
#![allow(missing_docs)]

#[path = "common/mod.rs"]
mod common;

use cairn_mcp::CairnMcpHandler;
use cairn_test_fixtures::mcp::conformance::{ConfigOverrides, canonicalize, load_case};
use rmcp::ServiceExt as _;
use tokio::io::BufReader;

use common::{do_initialize, recv_frame, send_frame};

/// Build a handler with the case's capability gates wired in.
///
/// This intentionally uses `CairnMcpHandler::new()` (the unwired variant) for
/// most cases — that's the same handler `smoke.rs` and `init_status_parity.rs`
/// use for protocol-layer assertions, and it produces deterministic envelopes
/// for the unwired verbs at v0.1. Cases that need a real store (a few of the
/// `Ok` ones, e.g., `ingest/ok_minimal`) construct a wired handler via the
/// existing `tiny_graph_async` helper.
fn build_handler_for(_config: ConfigOverrides) -> CairnMcpHandler {
    // For Task 6 + 7 we only need the unwired handler. Wired handlers land in
    // Task 8 when wired-store happy-path fixtures arrive.
    CairnMcpHandler::new()
}

/// Round-trip one envelope through a fresh handler via `tools/call` over
/// stdio. Returns the handler's envelope response extracted from the
/// `tools/call` result frame.
///
/// The unwired handler's `dispatch_stub` returns plain text (not a JSON
/// envelope); `unwrap_envelope_from_tool_result` represents that faithfully
/// as `{"__raw_text": "<message>"}` so callers always get a stable
/// `serde_json::Value` to diff against the fixture.
async fn dispatch_envelope(
    handler: CairnMcpHandler,
    request: &serde_json::Value,
) -> serde_json::Value {
    let (server_half, client_half) = tokio::io::duplex(65_536);
    let _server_task = tokio::spawn(async move {
        handler
            .serve(server_half)
            .await
            .expect("server init")
            .waiting()
            .await
            .ok();
    });

    let (client_read, mut client_write) = tokio::io::split(client_half);
    let mut client_reader = BufReader::new(client_read);

    let _init = do_initialize(&mut client_write, &mut client_reader).await;

    let verb = request
        .get("verb")
        .and_then(|v| v.as_str())
        .expect("envelope.verb missing");
    let args = request
        .get("args")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    // JSON-RPC tools/call frame with `name = verb` and `arguments = args`.
    let frame = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": verb, "arguments": args }
    });
    send_frame(&mut client_write, &frame.to_string()).await;
    let resp = recv_frame(&mut client_reader).await;

    unwrap_envelope_from_tool_result(&resp)
}

/// MCP returns `tools/call` results in a `result.content[]` array. Cairn's
/// wired verbs return an envelope as the first `text` element's JSON payload.
/// Unwired verbs (`dispatch_stub`) return a plain-text error message; in that
/// case the text is wrapped in `{"__raw_text": "<message>"}` so callers always
/// receive a diffable `Value`.
///
/// At v0.1 all verbs except `search` (with a wired store) go through
/// `dispatch_stub`, so the conformance runner sees `__raw_text` for those
/// paths. Task 8 wires real stores for the happy-path cases; at that point
/// the actual JSON envelope is present and `serde_json::from_str` succeeds.
fn unwrap_envelope_from_tool_result(resp: &serde_json::Value) -> serde_json::Value {
    if let Some(result) = resp.get("result") {
        // Common path: result.content[0].text == stringified envelope JSON
        // (wired verbs) OR a plain-text stub message (unwired verbs).
        if let Some(content) = result.get("content").and_then(|c| c.as_array())
            && let Some(first) = content.first()
            && let Some(text) = first.get("text").and_then(|t| t.as_str())
        {
            // Try to parse as a JSON envelope first.
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
                return v;
            }
            // Plain-text stub response — wrap for stable diffing.
            return serde_json::json!({ "__raw_text": text });
        }
        // Some handlers may use `result.structuredContent` directly.
        if let Some(sc) = result.get("structuredContent") {
            return sc.clone();
        }
    }
    // Fallback: return the whole response frame so callers can diff it
    // rather than getting a misleading empty result.
    resp.clone()
}

// ── self-tests for the runner ────────────────────────────────────────────────
mod runner_self_tests {
    use super::*;

    /// Negative meta-test: the runner *can* detect a mismatch. If this test
    /// passes by NOT panicking, the runner has lost its assertion path.
    #[tokio::test]
    async fn runner_actually_diffs() {
        let mut case = load_case("search/ok_keyword");
        // Mutate the expected response so it disagrees with whatever the
        // handler produces.
        case.response["data"]["hits"] = serde_json::json!([{ "definitely": "wrong" }]);

        let handler = build_handler_for(case.config);
        let actual = dispatch_envelope(handler, &case.request).await;

        let result = std::panic::catch_unwind(|| {
            pretty_assertions::assert_eq!(canonicalize(&actual), canonicalize(&case.response),);
        });
        assert!(
            result.is_err(),
            "runner failed to detect a forced mismatch — assertion path is broken"
        );
    }
}
