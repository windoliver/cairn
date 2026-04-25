#![allow(missing_docs)]

use cairn_idl::codegen::{emit_cli, ir, loader};

fn doc() -> ir::Document {
    let raw = loader::load(std::path::Path::new(cairn_idl::SCHEMA_DIR)).unwrap();
    ir::build(&raw).unwrap()
}

#[test]
fn emits_command_builder_with_eight_subcommands_plus_two_preludes() {
    let files = emit_cli::emit(&doc()).unwrap();
    let mod_rs = files
        .iter()
        .find(|f| f.path.ends_with("crates/cairn-cli/src/generated/mod.rs"))
        .unwrap();
    let body = std::str::from_utf8(&mod_rs.bytes).unwrap();
    assert!(body.contains("pub fn command() -> clap::Command"));
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
            body.contains(&format!("\"{verb}\"")),
            "missing subcommand for {verb}"
        );
    }
    // Preludes present.
    assert!(body.contains("\"status\""));
    assert!(body.contains("\"handshake\""));
}
