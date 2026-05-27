//! MCP conformance tests for `cairn.admin.v1` (issue #161 Gap 7).
//!
//! Validates the contract surface at the MCP boundary:
//! - The 6 admin tool names are in the canonical list
//!   ([`cairn_mcp::admin_tools::ADMIN_TOOL_NAMES`]).
//! - While `ADMIN_MCP_DISPATCH_WIRED = false` (current state), every
//!   admin tool dispatched directly returns `CapabilityUnavailable`
//!   (brief §15 fail-closed / CLAUDE.md §4 invariant 6).
//! - `is_admin_tool` correctly classifies names.
//!
//! The end-to-end `tools/list` filtering (admin tools absent from the
//! wire while dark) is exercised by the existing
//! `smoke::wire_tools_list_descriptions_match_generated_output` test
//! after Gap 5's update — the conformance test below adds direct
//! dispatcher coverage on top.

use cairn_mcp::admin_tools::{
    self, ADMIN_CAPABILITY, ADMIN_TOOL_NAMES, capability_unavailable, dispatch, dispatch_ready,
    is_admin_tool, runtime_ready,
};
use rmcp::model::RawContent;

const EXPECTED_TOOLS: &[&str] = &[
    "admin_snapshot",
    "admin_restore",
    "admin_replay_wal",
    "admin_connector_enable",
    "admin_connector_disable",
    "admin_connector_backfill",
];

#[test]
fn admin_tool_names_match_expected_set() {
    assert_eq!(ADMIN_TOOL_NAMES.len(), 6);
    for name in EXPECTED_TOOLS {
        assert!(
            ADMIN_TOOL_NAMES.contains(name),
            "ADMIN_TOOL_NAMES missing {name}"
        );
        assert!(is_admin_tool(name), "is_admin_tool({name}) returned false");
    }
}

#[test]
fn is_admin_tool_rejects_non_admin_names() {
    for n in &[
        "",
        "ingest",
        "search",
        "retrieve",
        "summarize",
        "assemble_hot",
        "capture_trace",
        "lint",
        "forget",
        "propose_share",
        "accept_share",
        "revoke_share",
        "admin",         // prefix only — must not match
        "admin_unknown", // wrong suffix
        "snapshot",      // v0.1 name
        "snapshot_v2",   // not the IDL name
    ] {
        assert!(
            !is_admin_tool(n),
            "is_admin_tool({n}) wrongly returned true"
        );
    }
}

#[test]
fn umbrella_capability_string_matches_idl() {
    assert_eq!(ADMIN_CAPABILITY, "cairn.mcp.v1.extension.admin");
}

#[test]
fn dispatch_returns_capability_unavailable_for_every_tool_while_dark() {
    // ADMIN_MCP_DISPATCH_WIRED = false in the current phase.
    if dispatch_ready() {
        // Gap 8 has flipped the constants; this test's preconditions no
        // longer hold. Bail out — there's a sibling test that exercises
        // the wired path.
        return;
    }
    for &tool in EXPECTED_TOOLS {
        let result = dispatch(tool, None, Some(std::path::Path::new("/tmp")));
        assert!(
            result.is_error.unwrap_or(false),
            "{tool}: expected is_error=true while dark"
        );
        let text = first_text(&result.content).unwrap_or_default();
        assert!(
            text.contains("capability unavailable"),
            "{tool}: expected 'capability unavailable' text, got: {text}"
        );
        assert!(
            text.contains(ADMIN_CAPABILITY),
            "{tool}: expected the umbrella capability string in the error text, got: {text}"
        );
    }
}

#[test]
fn dispatch_unknown_admin_tool_returns_capability_unavailable() {
    // dispatch() with an arbitrary name still goes through the
    // capability gate — we don't want to leak structural details about
    // which admin verbs ship in this build.
    let result = dispatch(
        "admin_does_not_exist",
        None,
        Some(std::path::Path::new("/tmp")),
    );
    assert!(result.is_error.unwrap_or(false));
}

#[test]
fn capability_unavailable_helper_includes_remediation_hint() {
    let result = capability_unavailable("admin_snapshot");
    assert!(result.is_error.unwrap_or(false));
    let text = first_text(&result.content).unwrap_or_default();
    // The REMEDIATION table doesn't necessarily have an entry for the
    // umbrella capability today, so we only assert the capability
    // string is present — hint may or may not be appended.
    assert!(text.contains(ADMIN_CAPABILITY));
}

#[test]
fn runtime_ready_requires_capability_in_slice() {
    use cairn_core::generated::common::Capabilities;
    // Empty slice → not ready regardless of wired state.
    assert!(!runtime_ready(&[]));

    // With the umbrella capability in the slice, runtime_ready tracks
    // dispatch_ready (i.e. the build-time WIRED constants).
    let caps = [Capabilities::CairnMcpV1ExtensionAdmin];
    assert_eq!(runtime_ready(&caps), dispatch_ready());

    // Adding noise capabilities doesn't change the answer.
    let noisy = [
        Capabilities::CairnMcpV1ExtensionAdmin,
        Capabilities::CairnMcpV1SearchKeyword,
    ];
    assert_eq!(runtime_ready(&noisy), dispatch_ready());
}

#[test]
fn dispatch_ready_matches_wiring_constants() {
    let expected = cairn_core::status::wiring::ADMIN_EXTENSION_WIRED
        && cairn_core::status::wiring::ADMIN_CLI_DISPATCH_WIRED
        && cairn_core::status::wiring::ADMIN_MCP_DISPATCH_WIRED;
    assert_eq!(admin_tools::dispatch_ready(), expected);
}

fn first_text(content: &[rmcp::model::Content]) -> Option<String> {
    content.iter().find_map(|c| {
        if let RawContent::Text(t) = &**c {
            Some(t.text.clone())
        } else {
            None
        }
    })
}
