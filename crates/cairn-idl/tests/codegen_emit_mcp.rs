#![allow(missing_docs)]

use cairn_idl::codegen::{emit_mcp, ir, loader};

fn doc() -> ir::Document {
    let raw = loader::load(std::path::Path::new(cairn_idl::SCHEMA_DIR)).unwrap();
    ir::build(&raw).unwrap()
}

#[test]
fn emits_tools_array_and_schemas_subtree() {
    let files = emit_mcp::emit(&doc()).unwrap();
    let names: Vec<String> = files
        .iter()
        .map(|f| f.path.to_string_lossy().to_string())
        .collect();
    assert!(
        names
            .iter()
            .any(|n| n.ends_with("crates/cairn-mcp/src/generated/mod.rs"))
    );
    for verb in [
        "ingest",
        "search",
        "retrieve",
        "summarize",
        "assemble_hot",
        "capture_trace",
        "lint",
        "forget",
    ] {
        assert!(
            names.iter().any(|n| n.ends_with(&format!(
                "crates/cairn-mcp/src/generated/schemas/verbs/{verb}.json"
            ))),
            "missing schema for {verb}"
        );
    }
    assert!(
        names
            .iter()
            .any(|n| n.ends_with("crates/cairn-mcp/src/generated/schemas/prelude/status.json"))
    );
    assert!(
        names
            .iter()
            .any(|n| n.ends_with("crates/cairn-mcp/src/generated/schemas/prelude/handshake.json"))
    );
}

#[test]
fn schemas_use_canonical_json() {
    let files = emit_mcp::emit(&doc()).unwrap();
    let ingest = files
        .iter()
        .find(|f| f.path.ends_with("schemas/verbs/ingest.json"))
        .unwrap();
    let body = std::str::from_utf8(&ingest.bytes).unwrap();
    assert!(
        body.ends_with('\n'),
        "canonical JSON must end with a newline"
    );
    // Parsing must succeed and round-trip via canonical writer == identity.
    let parsed: serde_json::Value = serde_json::from_str(body).unwrap();
    let again = cairn_idl::codegen::fmt::write_json_canonical(&parsed);
    assert_eq!(body, again, "ingest schema is not canonical");
}

#[test]
fn tool_decl_description_includes_skill_triggers() {
    let files = emit_mcp::emit(&doc()).unwrap();
    let mod_rs = files
        .iter()
        .find(|f| f.path.ends_with("crates/cairn-mcp/src/generated/mod.rs"))
        .unwrap();
    let body = std::str::from_utf8(&mod_rs.bytes).unwrap();
    // Any description should include at least one positive trigger phrase.
    assert!(
        body.contains("remember that"),
        "ingest's positive trigger missing"
    );
}
