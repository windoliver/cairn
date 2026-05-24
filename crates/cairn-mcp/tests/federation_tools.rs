//! `cairn.federation.v1` MCP tool registration gates (issue #123).
//!
//! These tests verify the three federation verb tools are gated on the
//! `cairn.mcp.v1.extension.federation` capability AND on the
//! `federation_extension_ready()` wiring constant. All five wiring
//! constants are now `true`; the tests verify the positive path and
//! the runtime-availability gate (no `FederationState` = capability
//! unavailable at dispatch time).
#![allow(missing_docs)]

use cairn_core::generated::common::Capabilities;
use cairn_mcp::federation_tools::{
    FEDERATION_CAPABILITY, FEDERATION_TOOL_NAMES, dispatch_ready, is_federation_tool, runtime_ready,
};

#[test]
fn federation_advertises_three_tools() {
    assert_eq!(FEDERATION_TOOL_NAMES.len(), 3);
    assert!(FEDERATION_TOOL_NAMES.contains(&"propose_share"));
    assert!(FEDERATION_TOOL_NAMES.contains(&"accept_share"));
    assert!(FEDERATION_TOOL_NAMES.contains(&"revoke_share"));
}

#[test]
fn federation_tool_names_round_trip_idl_tools() {
    let idl_names: Vec<&str> = cairn_mcp::generated::TOOLS
        .iter()
        .filter(|d| d.capability == Some(FEDERATION_CAPABILITY))
        .map(|d| d.name)
        .collect();
    let mut local: Vec<&str> = FEDERATION_TOOL_NAMES.to_vec();
    let mut idl_sorted = idl_names.clone();
    local.sort_unstable();
    idl_sorted.sort_unstable();
    assert_eq!(local, idl_sorted);
}

#[test]
fn federation_tools_hidden_without_capability() {
    // Empty capability slice → must never be runtime-ready, even if
    // dispatch_ready() is true (which it now is).
    assert!(!runtime_ready(&[]));
}

#[test]
fn federation_tools_follow_runtime_readiness() {
    let caps = [Capabilities::CairnMcpV1ExtensionFederation];
    // dispatch_ready() is now true (all 5 wiring constants flipped).
    assert!(dispatch_ready());
    assert!(
        runtime_ready(&caps),
        "with the capability advertised AND every wiring constant true, runtime_ready must agree",
    );
}

#[test]
fn federation_dispatch_is_wired() {
    // All five federation wiring constants are true. dispatch_ready()
    // returns true, confirming the dispatch path is wired.
    assert!(dispatch_ready());
}

#[test]
fn federation_tool_membership() {
    for name in FEDERATION_TOOL_NAMES {
        assert!(is_federation_tool(name));
    }
    assert!(!is_federation_tool("ingest"));
    assert!(!is_federation_tool("handshake"));
    assert!(!is_federation_tool(""));
}

#[test]
fn federation_tool_decls_carry_expected_capability() {
    // Every IDL TOOLS entry whose name lives in FEDERATION_TOOL_NAMES
    // must advertise the federation capability. Guards against an IDL
    // drift that lands a federation verb without the capability stamp.
    for name in FEDERATION_TOOL_NAMES {
        let decl = cairn_mcp::generated::TOOLS
            .iter()
            .find(|d| d.name == *name)
            .unwrap_or_else(|| panic!("missing IDL tool {name}"));
        assert_eq!(
            decl.capability,
            Some(FEDERATION_CAPABILITY),
            "{name} must advertise {FEDERATION_CAPABILITY}",
        );
    }
}

#[test]
fn federation_tool_schemas_are_well_formed_objects() {
    // The IDL emits the per-verb input schema with a top-level `$ref`
    // into `$defs/Args` so the same document carries both the args
    // shape and the response data shape. The MCP transport materialises
    // the args shape; the test follows the same path.
    for name in FEDERATION_TOOL_NAMES {
        let decl = cairn_mcp::generated::TOOLS
            .iter()
            .find(|d| d.name == *name)
            .unwrap_or_else(|| panic!("missing IDL tool {name}"));
        let schema: serde_json::Value = serde_json::from_slice(decl.input_schema)
            .unwrap_or_else(|err| panic!("schema for {name} must be JSON: {err}"));
        let args = schema
            .get("$defs")
            .and_then(|d| d.get("Args"))
            .unwrap_or_else(|| panic!("schema for {name} missing $defs.Args"));
        assert_eq!(args["type"], "object", "schema for {name}");
        assert!(args["properties"].is_object(), "schema for {name}");
        assert_eq!(
            args["additionalProperties"], false,
            "schema for {name} must reject unknown arguments",
        );
    }
}

#[test]
fn federation_tool_names_snapshot() {
    let names: Vec<&str> = FEDERATION_TOOL_NAMES.to_vec();
    insta::assert_yaml_snapshot!(names);
}

/// Build a minimal [`FederationState`] backed by an in-memory `SQLite` store
/// and a loopback transport. Shared by the two dispatch integration tests
/// below.
async fn make_federation_state() -> cairn_mcp::handler::FederationState {
    use std::sync::Arc;

    use cairn_core::domain::identity::Identity;
    use cairn_core::domain::identity::keys::SigningKey;
    use cairn_store_sqlite::open_in_memory;
    use cairn_test_fixtures::federation::LoopbackTransport;
    use rand_core::OsRng;

    let store = Arc::new(open_in_memory().await.expect("in-memory store"));
    let transport = Arc::new(LoopbackTransport::new());
    let signing_key = SigningKey::generate(&mut OsRng);
    let identity = Identity::parse("hmn:test-issuer").expect("valid identity prefix");

    cairn_mcp::handler::FederationState {
        store,
        transport: transport
            as Arc<dyn cairn_core::contract::federation_transport::FederationTransport>,
        signing_key,
        identity,
        key_version: 1,
    }
}

/// `propose_share` with a non-existent record ID must return a well-formed
/// error `CallToolResult` — never panic and never return an empty content
/// slice.
#[tokio::test]
async fn dispatch_propose_share_with_state_returns_typed_response() {
    let state = make_federation_state().await;

    // Minimal valid-shaped args. The ULID exists syntactically but there are
    // no records in the store, so the verb must fail with an error result.
    let args = serde_json::json!({
        "record_ids": ["01HQZX9F5N0000000000000000"],
        "scope": { "agent": "agt:test" },
        "grant_tier": "team",
        "expires_at": "2027-01-01T00:00:00Z"
    });

    let result = cairn_mcp::federation_tools::dispatch(
        "propose_share",
        Some(args.as_object().expect("args is object").clone()),
        Some(&state),
    )
    .await;

    // The verb must produce an error result (no records found), never panic.
    assert!(
        result.is_error.unwrap_or(false),
        "propose_share with nonexistent records should be an error"
    );
    assert!(
        !result.content.is_empty(),
        "error response must carry at least one content item"
    );
}

/// `revoke_share` with an unknown link ID must return a well-formed error
/// `CallToolResult` — never panic and never return an empty content slice.
#[tokio::test]
async fn dispatch_revoke_share_with_state_returns_typed_response() {
    let state = make_federation_state().await;

    let args = serde_json::json!({
        "link_id": "01HQZX9F5N0000000000000000"
    });

    let result = cairn_mcp::federation_tools::dispatch(
        "revoke_share",
        Some(args.as_object().expect("args is object").clone()),
        Some(&state),
    )
    .await;

    // Must fail with UnknownLink or similar, never panic.
    assert!(
        result.is_error.unwrap_or(false),
        "revoke_share with nonexistent link should be an error"
    );
    assert!(
        !result.content.is_empty(),
        "error response must carry at least one content item"
    );
}

#[tokio::test]
async fn federation_dispatch_returns_capability_unavailable_when_no_state() {
    let result = cairn_mcp::federation_tools::dispatch("propose_share", None, None).await;
    assert!(result.is_error.unwrap_or(false));
    let text = result
        .content
        .first()
        .and_then(|c| {
            if let rmcp::model::RawContent::Text(t) = &**c {
                Some(t.text.as_str())
            } else {
                None
            }
        })
        .unwrap_or("");
    assert!(
        text.contains("capability unavailable"),
        "expected capability unavailable, got: {text}"
    );
}
