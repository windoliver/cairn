use cairn_idl::codegen::{emit_skill, ir, loader};

fn doc() -> ir::Document {
    let raw = loader::load(std::path::Path::new(cairn_idl::SCHEMA_DIR)).unwrap();
    ir::build(&raw).unwrap()
}

#[test]
fn emits_skill_md_with_eight_verb_sections() {
    let files = emit_skill::emit(&doc()).unwrap();
    let skill = files
        .iter()
        .find(|f| f.path.ends_with("skills/cairn/SKILL.md"))
        .unwrap();
    let body = std::str::from_utf8(&skill.bytes).unwrap();
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
            body.contains(&format!("## `cairn {verb}`")),
            "SKILL.md missing section for {verb}"
        );
    }
    // Preludes called out as preludes, not core verbs.
    assert!(body.contains("Protocol preludes"));
    assert!(body.contains("status"));
    assert!(body.contains("handshake"));
}

#[test]
fn version_file_pins_contract_and_pkg() {
    let files = emit_skill::emit(&doc()).unwrap();
    let version = files
        .iter()
        .find(|f| f.path.ends_with("skills/cairn/.version"))
        .unwrap();
    let body = std::str::from_utf8(&version.bytes).unwrap();
    assert!(body.contains("contract: cairn.mcp.v1"));
    assert!(body.contains("cairn-idl:"));
}
