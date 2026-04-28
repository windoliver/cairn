//! Compat tests for the generated Cairn skill (issue #70).
//!
//! Verifies the example-extractor + CLI/JSON validators in
//! [`cairn_idl::codegen::skill_compat`] against the live SKILL.md so the skill
//! cannot drift away from the IDL contract.

use cairn_idl::codegen::skill_compat::{
    CodeBlock, CompatError, extract_code_blocks, validate_cli_block, validate_json_block,
};
use cairn_idl::codegen::{ir, loader};

fn doc() -> ir::Document {
    let raw = loader::load(std::path::Path::new(cairn_idl::SCHEMA_DIR))
        .expect("invariant: SCHEMA_DIR loads");
    ir::build(&raw).expect("invariant: IR builds")
}

fn live_skill_md() -> String {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("skills")
        .join("cairn")
        .join("SKILL.md");
    std::fs::read_to_string(&path).expect("invariant: SKILL.md exists at workspace path")
}

#[test]
fn extract_finds_fenced_blocks_with_lang_tags() {
    let md = "intro\n\n```bash\ncairn search foo\n```\n\nmore\n\n```json\n{\"a\":1}\n```\n";
    let blocks = extract_code_blocks(md);
    assert!(
        blocks
            .iter()
            .any(|b| b.lang == "bash" && b.body.contains("cairn search")),
        "expected bash block, got: {blocks:?}"
    );
    assert!(
        blocks
            .iter()
            .any(|b| b.lang == "json" && b.body.contains("\"a\":1")),
        "expected json block, got: {blocks:?}"
    );
}

#[test]
fn extract_finds_inline_cairn_spans() {
    let md = "Use `cairn handshake --json` then `cairn status --json`.";
    let blocks = extract_code_blocks(md);
    let inline: Vec<_> = blocks.iter().filter(|b| b.lang == "inline").collect();
    assert_eq!(
        inline.len(),
        2,
        "expected 2 inline cairn spans, got: {blocks:?}"
    );
}

#[test]
fn cli_validator_accepts_canonical_verb_with_known_flag() {
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn search --mode hybrid".into(),
        line: 1,
    };
    validate_cli_block(&block, &doc()).expect("canonical search invocation must validate");
}

#[test]
fn cli_validator_accepts_protocol_preludes() {
    let d = doc();
    for cmd in ["cairn handshake --json", "cairn status --json"] {
        let block = CodeBlock {
            lang: "bash".into(),
            body: cmd.into(),
            line: 1,
        };
        validate_cli_block(&block, &d).unwrap_or_else(|e| panic!("prelude `{cmd}` failed: {e}"));
    }
}

#[test]
fn cli_validator_rejects_unknown_verb() {
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn nonexistent --foo".into(),
        line: 7,
    };
    let err = validate_cli_block(&block, &doc()).expect_err("unknown verb must fail");
    assert!(
        matches!(err, CompatError::UnknownVerb { line: 7, .. }),
        "expected UnknownVerb at line 7, got: {err:?}"
    );
}

#[test]
fn cli_validator_rejects_unknown_flag() {
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn ingest --bogus".into(),
        line: 9,
    };
    let err = validate_cli_block(&block, &doc()).expect_err("unknown flag must fail");
    assert!(
        matches!(err, CompatError::UnknownFlag { line: 9, ref verb, .. } if verb == "ingest"),
        "expected UnknownFlag for ingest at line 9, got: {err:?}"
    );
}

#[test]
fn json_validator_rejects_invalid_payload_for_verb() {
    // ingest input requires fields; bare empty object should fail validation.
    let block = CodeBlock {
        lang: "json".into(),
        body: "{}".into(),
        line: 3,
    };
    let err =
        validate_json_block(&block, &doc(), "ingest").expect_err("empty ingest payload must fail");
    assert!(
        matches!(err, CompatError::SchemaMismatch { line: 3, ref verb, .. } if verb == "ingest"),
        "expected SchemaMismatch for ingest at line 3, got: {err:?}"
    );
}

#[test]
fn live_skill_md_passes_compat_checks() {
    let md = live_skill_md();
    let d = doc();
    for block in extract_code_blocks(&md) {
        match block.lang.as_str() {
            "bash" | "shell" | "sh" | "inline" if block.body.trim_start().starts_with("cairn ") => {
                validate_cli_block(&block, &d).unwrap_or_else(|e| {
                    panic!("SKILL.md CLI block at line {} invalid: {e}", block.line)
                });
            }
            _ => {}
        }
    }
}
