//! `cairn.federation.v1` MCP tool registration gates (issue #123 T17).
//!
//! These tests verify the three federation verb tools are gated on the
//! `cairn.mcp.v1.extension.federation` capability AND on the
//! `federation_extension_ready()` wiring constant. While T17 lays only
//! the registration code (T19 flips the gates on), the tests follow the
//! gate at runtime — they assert the negative path today and the
//! positive path once T19 lands.
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
    // Empty capability slice → must never be runtime-ready, even if a
    // future bug flips the build-time wiring constant prematurely.
    assert!(!runtime_ready(&[]));
}

#[test]
fn federation_tools_follow_runtime_readiness() {
    let caps = [Capabilities::CairnMcpV1ExtensionFederation];
    if !dispatch_ready() {
        assert!(
            !runtime_ready(&caps),
            "federation tools must stay hidden until the runtime is wired",
        );
        // T19 will flip `FEDERATION_MCP_TOOLS_WIRED` and the other
        // federation wiring constants. Until then the dispatch stub
        // documents itself as not-ready so this branch is the only one
        // exercised in CI.
        return;
    }
    assert!(
        runtime_ready(&caps),
        "with the capability advertised AND every wiring constant true, runtime_ready must agree",
    );
}

#[test]
fn federation_dispatch_stub_cannot_be_marked_ready() {
    // T17 deliberately leaves `dispatch_ready()` returning false — the
    // stub `dispatch()` body must be replaced with real verb dispatch
    // before the wiring constants flip. If this test starts failing,
    // remove the stub and add real non-error dispatch coverage before
    // enabling federation readiness.
    assert!(
        !dispatch_ready(),
        "remove federation_tools::dispatch stub and add real non-error dispatch coverage before enabling federation readiness",
    );
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
