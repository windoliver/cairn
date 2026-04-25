//! Insta snapshots for representative emitter outputs. Update with
//! `cargo insta review` after intentional IDL or emitter changes.

#![allow(missing_docs)]

use cairn_idl::codegen::{emit_cli, emit_mcp, emit_sdk, emit_skill, ir, loader};

fn doc() -> ir::Document {
    let raw = loader::load(std::path::Path::new(cairn_idl::SCHEMA_DIR)).unwrap();
    ir::build(&raw).unwrap()
}

fn read(files: &[cairn_idl::codegen::GeneratedFile], suffix: &str) -> String {
    let f = files
        .iter()
        .find(|f| f.path.ends_with(suffix))
        .unwrap_or_else(|| panic!("no generated file ending in {suffix}"));
    std::str::from_utf8(&f.bytes).unwrap().to_string()
}

#[test]
fn snapshot_sdk_verbs_mod() {
    let files = emit_sdk::emit(&doc()).unwrap();
    insta::assert_snapshot!(read(&files, "crates/cairn-core/src/generated/verbs/mod.rs"));
}

#[test]
fn snapshot_sdk_ingest() {
    let files = emit_sdk::emit(&doc()).unwrap();
    insta::assert_snapshot!(read(&files, "crates/cairn-core/src/generated/verbs/ingest.rs"));
}

#[test]
fn snapshot_cli_mod() {
    let files = emit_cli::emit(&doc()).unwrap();
    insta::assert_snapshot!(read(&files, "crates/cairn-cli/src/generated/mod.rs"));
}

#[test]
fn snapshot_mcp_mod() {
    let files = emit_mcp::emit(&doc()).unwrap();
    insta::assert_snapshot!(read(&files, "crates/cairn-mcp/src/generated/mod.rs"));
}

#[test]
fn snapshot_mcp_ingest_schema() {
    let files = emit_mcp::emit(&doc()).unwrap();
    insta::assert_snapshot!(read(&files, "crates/cairn-mcp/src/generated/schemas/verbs/ingest.json"));
}

#[test]
fn snapshot_skill_md() {
    let files = emit_skill::emit(&doc()).unwrap();
    insta::assert_snapshot!(read(&files, "skills/cairn/SKILL.md"));
}
