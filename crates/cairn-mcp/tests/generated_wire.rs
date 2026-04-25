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

// ── Round-5 finding F3: ToolDecl surfaces field-level x-cairn-capability ───

fn capabilities_for(name: &str) -> Vec<(&'static str, &'static str)> {
    let t = tool(name);
    t.capability_overrides
        .iter()
        .map(|o| (o.path, o.capability))
        .collect()
}

#[test]
fn forget_surfaces_capability_per_mode() {
    let mut got = capabilities_for("forget");
    got.sort();
    let mut want = vec![
        ("mode=record", "cairn.mcp.v1.forget.record"),
        ("mode=session", "cairn.mcp.v1.forget.session"),
        ("mode=scope", "cairn.mcp.v1.forget.scope"),
    ];
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn retrieve_surfaces_capability_per_target() {
    let mut got = capabilities_for("retrieve");
    got.sort();
    let mut want = vec![
        ("target=record", "cairn.mcp.v1.retrieve.record"),
        ("target=session", "cairn.mcp.v1.retrieve.session"),
        ("target=turn", "cairn.mcp.v1.retrieve.turn"),
        ("target=folder", "cairn.mcp.v1.retrieve.folder"),
        ("target=scope", "cairn.mcp.v1.retrieve.scope"),
        ("target=profile", "cairn.mcp.v1.retrieve.profile"),
    ];
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn search_surfaces_capability_per_mode() {
    let mut got = capabilities_for("search");
    got.sort();
    let mut want = vec![
        ("mode=keyword", "cairn.mcp.v1.search.keyword"),
        ("mode=semantic", "cairn.mcp.v1.search.semantic"),
        ("mode=hybrid", "cairn.mcp.v1.search.hybrid"),
    ];
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn root_capability_null_verbs_have_capability_overrides() {
    // Every verb whose root `capability` is None must carry capability
    // overrides — otherwise the MCP transport has no way to gate the call.
    for name in ["search", "retrieve", "forget"] {
        let t = tool(name);
        assert!(
            t.capability.is_none(),
            "{name} should have no root capability"
        );
        assert!(
            !t.capability_overrides.is_empty(),
            "{name} must carry capability overrides when root capability is None"
        );
    }
}

#[test]
fn capability_override_paths_are_stable_strings() {
    // Every override carries a non-empty path and a `cairn.mcp.v1.*` capability.
    for t in TOOLS {
        for ov in t.capability_overrides {
            assert!(!ov.path.is_empty(), "{}: empty cap override path", t.name);
            assert!(
                ov.capability.starts_with("cairn.mcp.v1."),
                "{}: capability `{}` should start with cairn.mcp.v1.",
                t.name,
                ov.capability
            );
        }
    }
}
