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

#[test]
fn supporting_schema_groups_are_emitted_alongside_verbs() {
    // Cross-file `$ref` paths inside verb schemas (e.g.
    // `../common/scope_filter.json`) need the sibling schema groups to ship
    // under the same on-disk root, otherwise the references dangle when the
    // MCP server validates incoming requests.
    let files = emit_mcp::emit(&doc()).unwrap();
    let names: Vec<String> = files
        .iter()
        .map(|f| f.path.to_string_lossy().to_string())
        .collect();
    for required in [
        "crates/cairn-mcp/src/generated/schemas/common/primitives.json",
        "crates/cairn-mcp/src/generated/schemas/common/scope_filter.json",
        "crates/cairn-mcp/src/generated/schemas/errors/error.json",
        "crates/cairn-mcp/src/generated/schemas/capabilities/capabilities.json",
        "crates/cairn-mcp/src/generated/schemas/extensions/registry.json",
        "crates/cairn-mcp/src/generated/schemas/envelope/request.json",
        "crates/cairn-mcp/src/generated/schemas/envelope/response.json",
        "crates/cairn-mcp/src/generated/schemas/envelope/signed_intent.json",
    ] {
        assert!(
            names.iter().any(|n| n.ends_with(required)),
            "missing supporting schema {required}"
        );
    }
}

#[test]
fn verb_schema_carries_full_idl_file_with_local_defs() {
    // Per-verb schema should be the full source file so `#/$defs/...` refs
    // inside Args/Data resolve against the same JSON document.
    let files = emit_mcp::emit(&doc()).unwrap();
    let retrieve = files
        .iter()
        .find(|f| f.path.ends_with("schemas/verbs/retrieve.json"))
        .unwrap();
    let body = std::str::from_utf8(&retrieve.bytes).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(body).unwrap();
    let defs = parsed
        .get("$defs")
        .and_then(serde_json::Value::as_object)
        .expect("retrieve schema must keep its $defs envelope");
    for required_def in [
        "Args",
        "ArgsRecord",
        "ArgsSession",
        "ArgsTurn",
        "ArgsFolder",
        "ArgsScope",
        "ArgsProfile",
        "Data",
    ] {
        assert!(
            defs.contains_key(required_def),
            "retrieve schema missing $defs.{required_def} — local refs would dangle"
        );
    }
}
