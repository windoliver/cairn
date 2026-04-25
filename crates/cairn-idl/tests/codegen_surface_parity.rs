//! Four-surface parity test — the acceptance criterion of #35.
//!
//! Independently inspects each emitter's output and confirms:
//!   (1) all four surfaces enumerate the same eight verb ids in IDL order,
//!   (2) `status` and `handshake` appear separately, never as core verbs.

use cairn_idl::codegen::{ir, loader, emit_cli, emit_mcp, emit_sdk, emit_skill};

const EXPECTED_VERBS: &[&str] = &[
    "ingest", "search", "retrieve", "summarize",
    "assemble_hot", "capture_trace", "lint", "forget",
];

fn doc() -> ir::Document {
    let raw = loader::load(std::path::Path::new(cairn_idl::SCHEMA_DIR)).unwrap();
    ir::build(&raw).unwrap()
}

#[test]
fn doc_lists_eight_verbs_in_idl_order() {
    let d = doc();
    let ids: Vec<&str> = d.verbs.iter().map(|v| v.id.as_str()).collect();
    assert_eq!(ids, EXPECTED_VERBS);
}

#[test]
fn sdk_verb_registry_lists_eight_verbs() {
    let files = emit_sdk::emit(&doc()).unwrap();
    let body_bytes = &files
        .iter()
        .find(|f| f.path.ends_with("crates/cairn-core/src/generated/verbs/mod.rs"))
        .unwrap()
        .bytes;
    let body = std::str::from_utf8(body_bytes).unwrap();

    // Extract the VerbId variants in source order.
    let start = body.find("pub enum VerbId").unwrap();
    let body = &body[start..];
    let block_start = body.find('{').unwrap() + 1;
    let block_end = body.find('}').unwrap();
    let block = &body[block_start..block_end];
    let variants: Vec<String> = block
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with("//"))
        .map(|s| s.trim_end_matches(',').trim().to_string())
        .collect();

    let expected: Vec<String> = EXPECTED_VERBS
        .iter()
        .map(|v| cairn_idl::codegen::ir::pascal_case(v))
        .collect();
    assert_eq!(variants, expected, "SDK VerbId mismatch");
    assert!(!body.contains("Status"), "Status leaked into VerbId");
    assert!(!body.contains("Handshake"), "Handshake leaked into VerbId");
}

#[test]
fn cli_subcommand_tree_lists_eight_verbs_plus_two_preludes() {
    let files = emit_cli::emit(&doc()).unwrap();
    let body_bytes = &files
        .iter()
        .find(|f| f.path.ends_with("crates/cairn-cli/src/generated/mod.rs"))
        .unwrap()
        .bytes;
    let body = std::str::from_utf8(body_bytes).unwrap();
    let mut idx_per_verb: Vec<(usize, &str)> = EXPECTED_VERBS
        .iter()
        .map(|v| {
            let needle = format!("\"{v}\"");
            let i = body.find(&needle).unwrap_or_else(|| panic!("CLI missing verb {v}"));
            (i, *v)
        })
        .collect();
    idx_per_verb.sort();
    let order: Vec<&str> = idx_per_verb.into_iter().map(|(_, v)| v).collect();
    assert_eq!(order, EXPECTED_VERBS, "CLI verb subcommand order != IDL order");
    // Preludes present.
    assert!(body.contains("\"status\""));
    assert!(body.contains("\"handshake\""));
}

#[test]
fn mcp_tools_array_lists_eight_verbs_in_idl_order() {
    let files = emit_mcp::emit(&doc()).unwrap();
    let body_bytes = &files
        .iter()
        .find(|f| f.path.ends_with("crates/cairn-mcp/src/generated/mod.rs"))
        .unwrap()
        .bytes;
    let body = std::str::from_utf8(body_bytes).unwrap();
    let mut idx_per_verb: Vec<(usize, &str)> = EXPECTED_VERBS
        .iter()
        .map(|v| {
            let needle = format!("name: \"{v}\"");
            let i = body.find(&needle).unwrap_or_else(|| panic!("MCP missing verb {v}"));
            (i, *v)
        })
        .collect();
    idx_per_verb.sort();
    let order: Vec<&str> = idx_per_verb.into_iter().map(|(_, v)| v).collect();
    assert_eq!(order, EXPECTED_VERBS, "MCP TOOLS order != IDL order");
    // Preludes are NOT in TOOLS — they're protocol preludes, not tools.
    assert!(!body.contains("name: \"status\""), "status leaked into TOOLS");
    assert!(!body.contains("name: \"handshake\""), "handshake leaked into TOOLS");
}

#[test]
fn skill_md_lists_eight_verb_sections_plus_separate_prelude_section() {
    let files = emit_skill::emit(&doc()).unwrap();
    let body_bytes = &files
        .iter()
        .find(|f| f.path.ends_with("skills/cairn/SKILL.md"))
        .unwrap()
        .bytes;
    let body = std::str::from_utf8(body_bytes).unwrap();
    for verb in EXPECTED_VERBS {
        assert!(body.contains(&format!("## `cairn {verb}`")), "SKILL.md missing section for {verb}");
    }
    assert!(body.contains("Protocol preludes"));
    // status / handshake are mentioned only inside the preludes section.
    let preludes_section_start = body.find("Protocol preludes").unwrap();
    assert!(body[preludes_section_start..].contains("status"));
    assert!(body[preludes_section_start..].contains("handshake"));
    // Neither appears as a `## \`cairn …\`` header.
    assert!(!body.contains("## `cairn status`"));
    assert!(!body.contains("## `cairn handshake`"));
}
