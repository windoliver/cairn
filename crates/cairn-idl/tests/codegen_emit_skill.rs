//! Snapshot-style integration tests for `emit_skill` — checks every verb
//! section is present in the rendered SKILL.md and the conventions/version
//! companion files are emitted with stable provenance markers.

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

#[test]
fn skill_md_contains_trigger_table() {
    let files = emit_skill::emit(&doc()).unwrap();
    let skill = files
        .iter()
        .find(|f| f.path.ends_with("skills/cairn/SKILL.md"))
        .unwrap();
    let body = std::str::from_utf8(&skill.bytes).unwrap();
    assert!(body.contains("## When to call cairn"), "missing trigger table heading");
    assert!(body.contains("cairn ingest --kind user"), "missing remember-user row");
    assert!(body.contains("cairn ingest --kind rule"), "missing remember-rule row");
    assert!(body.contains("cairn ingest --kind feedback"), "missing correction row");
    assert!(body.contains("cairn forget --record"), "missing forget row");
    assert!(body.contains("cairn assemble_hot"), "missing assemble_hot row");
    assert!(body.contains("cairn capture_trace"), "missing capture_trace row");
}

#[test]
fn skill_md_contains_output_format_section() {
    let files = emit_skill::emit(&doc()).unwrap();
    let skill = files
        .iter()
        .find(|f| f.path.ends_with("skills/cairn/SKILL.md"))
        .unwrap();
    let body = std::str::from_utf8(&skill.bytes).unwrap();
    assert!(body.contains("## Output format"), "missing output-format heading");
    assert!(body.contains("--json"), "output section must mention --json flag");
    assert!(body.contains("\"hits\""), "output section must show JSON response shape");
}

#[test]
fn skill_md_contains_non_negotiable_rules() {
    let files = emit_skill::emit(&doc()).unwrap();
    let skill = files
        .iter()
        .find(|f| f.path.ends_with("skills/cairn/SKILL.md"))
        .unwrap();
    let body = std::str::from_utf8(&skill.bytes).unwrap();
    assert!(body.contains("Non-negotiable"), "missing non-negotiable rules heading");
    assert!(body.contains("Never invent record IDs"), "rule 1 missing");
    assert!(body.contains("cairn forget"), "rule 2 (confirm before forget) missing");
    assert!(body.contains("stderr"), "rule 3 (surface stderr) missing");
    assert!(body.contains("CAIRN_IDENTITY"), "rule 4 (identity env var) missing");
    assert!(body.contains("trigger list"), "rule 5 (don't over-ingest) missing");
}

#[test]
fn examples_include_retrieve_context_and_lint_memory() {
    let files = emit_skill::emit(&doc()).unwrap();
    let retrieve = files
        .iter()
        .find(|f| f.path.ends_with("examples/05-retrieve-context.md"))
        .expect("missing 05-retrieve-context.md example");
    let lint = files
        .iter()
        .find(|f| f.path.ends_with("examples/06-lint-memory.md"))
        .expect("missing 06-lint-memory.md example");
    let retrieve_body = std::str::from_utf8(&retrieve.bytes).unwrap();
    let lint_body = std::str::from_utf8(&lint.bytes).unwrap();
    assert!(retrieve_body.contains("assemble_hot"), "retrieve example must call assemble_hot");
    assert!(lint_body.contains("cairn lint"), "lint example must call cairn lint");
}
