//! Wire-level checks for the generated MCP `ToolDecl` registry.
//!
//! Round-4 finding F5: ToolDecl carried only the verb-level `x-cairn-auth`,
//! so a caller could request `lint.write_report=true` (a write-producing
//! mode whose IDL annotation requires `write_capability`) under the verb's
//! `read_only` root auth. Field- / mode-level overrides now ship in
//! `ToolDecl.auth_overrides` and the tests below pin that down.

#![allow(missing_docs)]

use cairn_mcp::generated::{TOOLS, ToolDecl};

fn tool(name: &str) -> &'static ToolDecl {
    TOOLS
        .iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("missing tool: {name}"))
}

#[test]
fn lint_advertises_write_capability_for_write_report() {
    let lint = tool("lint");
    assert_eq!(lint.auth, "read_only");
    let ov = lint
        .auth_overrides
        .iter()
        .find(|o| o.path == "write_report")
        .unwrap_or_else(|| panic!("lint must surface write_report auth override"));
    assert_eq!(ov.auth, "write_capability");
}

#[test]
fn summarize_advertises_write_capability_for_persist() {
    let summarize = tool("summarize");
    assert_eq!(summarize.auth, "rebac");
    let ov = summarize
        .auth_overrides
        .iter()
        .find(|o| o.path == "persist")
        .unwrap_or_else(|| panic!("summarize must surface persist auth override"));
    assert_eq!(ov.auth, "write_capability");
}

#[test]
fn verbs_without_field_level_auth_have_no_overrides() {
    // Verbs whose Args carries no `x-cairn-auth` annotation should advertise
    // an empty overrides slice — the verb-level auth is sufficient.
    for name in [
        "search",
        "ingest",
        "assemble_hot",
        "capture_trace",
        "forget",
    ] {
        let t = tool(name);
        assert!(
            t.auth_overrides.is_empty(),
            "{name} should not surface auth overrides; got {:?}",
            t.auth_overrides
                .iter()
                .map(|o| (o.path, o.auth))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn auth_override_paths_are_stable_strings() {
    // Sanity: every override carries a non-empty path and a known auth literal.
    let known = [
        "read_only",
        "rebac",
        "signed_chain",
        "signed_principal",
        "hardware_key",
        "forget_capability",
        "write_capability",
    ];
    for t in TOOLS {
        for ov in t.auth_overrides {
            assert!(!ov.path.is_empty(), "{}: empty override path", t.name);
            assert!(
                known.contains(&ov.auth),
                "{}: unknown override auth {}",
                t.name,
                ov.auth
            );
        }
    }
}
