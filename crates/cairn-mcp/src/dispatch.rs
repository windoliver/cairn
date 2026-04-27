//! Verb dispatch for the MCP stdio transport (brief §8, §4 MCPServer contract).
//!
//! All tool calls flow through `verify_signed_intent` before reaching the
//! verb layer, satisfying CLAUDE.md invariant 5 (WAL + two-phase apply).
//! At P0 the store is not wired; every verb returns an `Aborted/Internal`
//! response. The signed-intent path uses a syntactic-only stub (per
//! `cairn_core::verifier` P0 docs).

use cairn_core::generated::common::{Ed25519Signature, Identity, Nonce16Base64, Ulid};
use cairn_core::generated::envelope::{
    Response, ResponsePolicyTrace, ResponseStatus, ResponseVerb, SignedIntent,
    SignedIntentScope, SignedIntentScopeTier,
};
use cairn_core::verifier::verify_signed_intent;

/// Dispatch a `tools/call` to the Cairn verb layer and return a
/// `cairn.mcp.v1` response envelope.
///
/// `name` is the MCP tool name (one of the eight verbs).
/// `args_json` is the raw tool arguments from the MCP client (unused at P0;
/// real arg parsing lands when the store is wired in issue #9).
pub fn dispatch(
    name: &str,
    _args_json: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Response {
    let verb = verb_from_tool_name(name);

    if matches!(verb, ResponseVerb::Unknown) {
        return Response {
            contract: "cairn.mcp.v1".to_owned(),
            data: None,
            error: Some(serde_json::json!({
                "code": "UnknownVerb",
                "message": format!("unknown tool: {name}"),
                "data": { "verb": name },
            })),
            operation_id: fresh_ulid(),
            policy_trace: Vec::<ResponsePolicyTrace>::new(),
            status: ResponseStatus::Rejected,
            target: None,
            verb: ResponseVerb::Unknown,
        };
    }

    // All mutations must flow through the envelope verifier (CLAUDE.md invariant 5).
    // P0: syntactic-only stub intent; real SignedIntent extraction from MCP
    // transport metadata or tool args lands at P1.
    let intent = p0_stub_intent();
    if verify_signed_intent(intent).is_err() {
        return Response {
            contract: "cairn.mcp.v1".to_owned(),
            data: None,
            error: Some(serde_json::json!({
                "code": "MissingSignature",
                "message": "signed intent failed syntactic validation",
            })),
            operation_id: fresh_ulid(),
            policy_trace: Vec::<ResponsePolicyTrace>::new(),
            status: ResponseStatus::Rejected,
            target: None,
            verb,
        };
    }

    p0_unimplemented_response(verb)
}

fn verb_from_tool_name(name: &str) -> ResponseVerb {
    match name {
        "ingest" => ResponseVerb::Ingest,
        "search" => ResponseVerb::Search,
        "retrieve" => ResponseVerb::Retrieve,
        "summarize" => ResponseVerb::Summarize,
        "assemble_hot" => ResponseVerb::AssembleHot,
        "capture_trace" => ResponseVerb::CaptureTrace,
        "lint" => ResponseVerb::Lint,
        "forget" => ResponseVerb::Forget,
        _ => ResponseVerb::Unknown,
    }
}

fn p0_unimplemented_response(verb: ResponseVerb) -> Response {
    Response {
        contract: "cairn.mcp.v1".to_owned(),
        data: None,
        error: Some(serde_json::json!({
            "code": "Internal",
            "message": "store not wired in this P0 build — verb dispatch lands in #9",
        })),
        operation_id: fresh_ulid(),
        policy_trace: Vec::<ResponsePolicyTrace>::new(),
        status: ResponseStatus::Aborted,
        target: None,
        verb,
    }
}

fn fresh_ulid() -> Ulid {
    Ulid(ulid::Ulid::new().to_string())
}

/// P0 stub `SignedIntent` — passes syntactic-only verification.
///
/// Real intent extraction (from MCP transport metadata or an explicit
/// `signed_intent` field in the tool call arguments) lands at P1.
fn p0_stub_intent() -> SignedIntent {
    SignedIntent {
        chain_parents: vec![],
        expires_at: "2099-12-31T23:59:59Z".to_owned(),
        issued_at: "2026-01-01T00:00:00Z".to_owned(),
        issuer: Identity("agt:cairn-mcp:p0:stub:v0".to_owned()),
        key_version: 1,
        nonce: Nonce16Base64("AAAAAAAAAAAAAAAAAAAAAA==".to_owned()),
        operation_id: fresh_ulid(),
        scope: SignedIntentScope {
            tenant: "p0".to_owned(),
            workspace: "p0".to_owned(),
            entity: "p0".to_owned(),
            tier: SignedIntentScopeTier::Private,
        },
        sequence: Some(1),
        server_challenge: None,
        signature: Ed25519Signature(format!("ed25519:{}", "0".repeat(128))),
        target_hash: format!("sha256:{}", "0".repeat(64)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p0_stub_intent_passes_verifier() {
        verify_signed_intent(p0_stub_intent()).expect("stub must pass syntactic checks");
    }

    #[test]
    fn fresh_ulid_is_26_chars() {
        let u = fresh_ulid();
        assert_eq!(u.0.len(), 26);
    }
}
