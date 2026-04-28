//! Compat tests for the generated Cairn skill (issue #70).
//!
//! Verifies the example-extractor + CLI/JSON validators in
//! [`cairn_idl::codegen::skill_compat`] against the live SKILL.md so the skill
//! cannot drift away from the IDL contract.

use cairn_idl::codegen::skill_compat::{
    CodeBlock, CompatError, extract_code_blocks, extract_verb_scoped_blocks, validate_cli_block,
    validate_json_block,
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
    let blocks = extract_code_blocks(md).expect("well-formed fences must parse");
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
    let blocks = extract_code_blocks(md).expect("inline-only markdown must parse");
    let inline: Vec<_> = blocks.iter().filter(|b| b.lang == "inline").collect();
    assert_eq!(
        inline.len(),
        2,
        "expected 2 inline cairn spans, got: {blocks:?}"
    );
}

#[test]
fn cli_validator_accepts_canonical_verb_with_known_flag() {
    // search requires both query (positional) and mode.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn search --mode hybrid QUERY".into(),
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
fn cli_validator_rejects_excess_positional_args() {
    // `assemble_hot` has no positional defined; two stray tokens must fail.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn assemble_hot foo bar".into(),
        line: 11,
    };
    let err = validate_cli_block(&block, &doc()).expect_err("excess positional must fail");
    assert!(
        matches!(
            err,
            CompatError::Malformed {
                kind: "cli",
                line: 11,
                ..
            }
        ),
        "expected Malformed cli error at line 11, got: {err:?}"
    );
}

#[test]
fn cli_validator_rejects_multiple_one_of_branches_satisfied() {
    // `ingest`'s `$defs/Args` declares `oneOf: [body, file, url]` — exactly
    // one source must be provided. Supplying both `--body` and `--file` is
    // ambiguous and must trip the gate (regression for the round-9 finding
    // that downgraded `oneOf` to `anyOf`).
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn ingest --kind KIND --body BODY --file PATH".into(),
        line: 1,
    };
    let err = validate_cli_block(&block, &doc())
        .expect_err("two oneOf branches satisfied at once must fail");
    assert!(
        matches!(err, CompatError::Malformed { kind: "cli", .. }),
        "expected Malformed cli error, got: {err:?}"
    );
}

#[test]
fn cli_validator_rejects_positional_alias_conflicting_with_flag() {
    // `cairn ingest`'s positional `source` aliases body|file|url. Supplying
    // both the positional and an aliased flag is the same XOR violation as
    // two flags from the oneOf — the real CLI rejects it. Regression for
    // round-10 finding 1.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn ingest foo --kind KIND --body BAR".into(),
        line: 1,
    };
    let err = validate_cli_block(&block, &doc()).expect_err("positional + aliased flag must fail");
    assert!(
        matches!(err, CompatError::Malformed { kind: "cli", .. }),
        "expected Malformed cli error, got: {err:?}"
    );
}

#[test]
fn cli_validator_accepts_positional_alias_alone() {
    // Positional alone (no body/file/url flag) should satisfy the oneOf via
    // its `aliases_one_of` declaration.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn ingest --kind KIND foo".into(),
        line: 1,
    };
    validate_cli_block(&block, &doc()).expect("positional aliasing oneOf must satisfy exclusivity");
}

#[test]
fn cli_validator_rejects_integer_flag_above_maximum() {
    // `retrieve --depth` has `maximum: 16`. A stale example with --depth 999
    // must fail compat — this is the round-10 finding-2 regression.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn retrieve --folder PATH --depth 999".into(),
        line: 1,
    };
    let err = validate_cli_block(&block, &doc()).expect_err("out-of-range integer flag must fail");
    assert!(
        matches!(err, CompatError::Malformed { kind: "cli", .. }),
        "expected Malformed cli error, got: {err:?}"
    );
}

#[test]
fn cli_validator_rejects_invalid_list_enum_item() {
    // `retrieve --include` is `list<enum(tool_calls,reasoning)>`. A stale
    // example with --include nonsense must fail compat (round-11 finding 2).
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn retrieve --session SESSION_ID --turn 0 --include nonsense".into(),
        line: 1,
    };
    let err =
        validate_cli_block(&block, &doc()).expect_err("invalid list<enum(...)> item must fail");
    assert!(
        matches!(err, CompatError::Malformed { kind: "cli", .. }),
        "expected Malformed cli error, got: {err:?}"
    );
}

#[test]
fn cli_validator_accepts_valid_list_enum_items() {
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn retrieve --session SESSION_ID --turn 0 --include tool_calls,reasoning".into(),
        line: 1,
    };
    validate_cli_block(&block, &doc()).expect("valid list<enum> items must pass");
}

#[test]
fn cli_validator_rejects_duplicate_list_enum_items() {
    // `--include` declares `uniqueItems: true`. Round-12 finding 1.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn retrieve --session SESSION_ID --turn 0 --include tool_calls,tool_calls".into(),
        line: 1,
    };
    let err = validate_cli_block(&block, &doc()).expect_err("duplicate list<enum> items must fail");
    assert!(
        matches!(err, CompatError::Malformed { kind: "cli", .. }),
        "expected Malformed cli error, got: {err:?}"
    );
}

#[test]
fn cli_validator_rejects_empty_list_enum_item() {
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn retrieve --session SESSION_ID --turn 0 --include tool_calls,".into(),
        line: 1,
    };
    let err = validate_cli_block(&block, &doc()).expect_err("empty list item must fail");
    assert!(
        matches!(err, CompatError::Malformed { kind: "cli", .. }),
        "expected Malformed cli error, got: {err:?}"
    );
}

#[test]
fn cli_validator_inspects_line_continuations() {
    // Multiline bash with backslash-newline. Round-12 finding 2: the gate
    // must validate the joined logical command, not just the first line.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn retrieve --session SESSION_ID --turn 0 \\\n  --include nonsense".into(),
        line: 1,
    };
    let err = validate_cli_block(&block, &doc()).expect_err("continuation line must be validated");
    assert!(
        matches!(err, CompatError::Malformed { kind: "cli", .. }),
        "expected Malformed cli error, got: {err:?}"
    );
}

#[test]
fn cli_validator_rejects_unterminated_quote() {
    // Round-12 finding 3: a syntactically broken example must not slip past
    // the gate just because the tokenizer was lenient.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn search --mode keyword \"unterminated".into(),
        line: 1,
    };
    let err = validate_cli_block(&block, &doc()).expect_err("unterminated quote must fail");
    assert!(
        matches!(err, CompatError::Malformed { kind: "cli", .. }),
        "expected Malformed cli error, got: {err:?}"
    );
}

#[test]
fn cli_validator_aggregates_repeated_list_flag_occurrences() {
    // clap's ArgAction::Append exposes `--include a --include b` as the
    // canonical repeated form. Round-13 finding 1: compat must aggregate
    // and apply uniqueItems / minItems across occurrences.
    let block = CodeBlock {
        lang: "bash".into(),
        body:
            "cairn retrieve --session SESSION_ID --turn 0 --include tool_calls --include tool_calls"
                .into(),
        line: 1,
    };
    let err = validate_cli_block(&block, &doc())
        .expect_err("repeated --include with duplicate must fail");
    assert!(
        matches!(err, CompatError::Malformed { kind: "cli", .. }),
        "expected Malformed cli error, got: {err:?}"
    );
}

#[test]
fn cli_validator_preserves_freeform_list_string_with_commas() {
    // Round-14 finding 1: `ingest --tags` is `list<string>`. A literal
    // comma in user data must not be treated as a list delimiter — the
    // gate should accept the value as a single tag.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn ingest --kind KIND --body BODY --tags \"foo,bar\"".into(),
        line: 1,
    };
    validate_cli_block(&block, &doc())
        .expect("freeform list<string> values must accept embedded commas");
}

#[test]
fn cli_validator_rejects_invalid_ulid_positional() {
    // `cairn retrieve <id>` expects a Crockford-base32 ULID. A bogus
    // hand-written value must fail the gate. Round-15 finding 2.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn retrieve not-a-ulid".into(),
        line: 1,
    };
    let err = validate_cli_block(&block, &doc()).expect_err("invalid ULID positional must fail");
    assert!(
        matches!(err, CompatError::Malformed { kind: "cli", .. }),
        "expected Malformed cli error, got: {err:?}"
    );
}

#[test]
fn cli_validator_accepts_valid_ulid_positional() {
    // 26-char Crockford base32 ULID — must validate.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn retrieve 01H8XGJWBWBAQ4N1NQK1A8X9YZ".into(),
        line: 1,
    };
    validate_cli_block(&block, &doc()).expect("valid ULID positional must pass");
}

#[test]
fn json_validator_resolves_cross_file_primitive_refs() {
    // `summarize.Args` references the Ulid primitive in
    // `common/primitives.json`. The cross-file `$ref` must resolve via the
    // schema retriever (round-15 follow-up), and a non-Ulid value must fail
    // pattern validation. A bare `{}` would also fail required, but using a
    // real Ulid-shaped key isolates the retriever path.
    let block = CodeBlock {
        lang: "json".into(),
        body: r#"{"record_ids": ["not-a-ulid"]}"#.into(),
        line: 5,
    };
    let err = validate_json_block(&block, &doc(), "summarize")
        .expect_err("invalid Ulid in summarize payload must fail via cross-file $ref");
    assert!(
        matches!(err, CompatError::SchemaMismatch { line: 5, ref verb, .. } if verb == "summarize"),
        "expected SchemaMismatch for summarize at line 5, got: {err:?}"
    );
}

#[test]
fn cli_validator_consumes_value_token_after_value_flag() {
    // `--mode` is value-bearing; `hybrid` is its value, not a positional.
    // `search` requires a positional `query`; `query` here serves that role.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn search --mode hybrid query".into(),
        line: 1,
    };
    validate_cli_block(&block, &doc()).expect("flag value must not be miscounted as a positional");
}

#[test]
fn extract_verb_scoped_blocks_attaches_heading_verb() {
    let md = "# Skill\n\n## `cairn ingest`\n\n```json\n{\"kind\":\"fact\"}\n```\n\n## `cairn search`\n\n```bash\ncairn search foo\n```\n";
    let scoped = extract_verb_scoped_blocks(md).expect("well-formed markdown must parse");
    let json = scoped
        .iter()
        .find(|(_, b)| b.lang == "json")
        .expect("json block present");
    assert_eq!(json.0.as_deref(), Some("ingest"));
    let bash = scoped
        .iter()
        .find(|(_, b)| b.lang == "bash")
        .expect("bash block present");
    assert_eq!(bash.0.as_deref(), Some("search"));
}

#[test]
fn extract_rejects_unterminated_fenced_block() {
    let md = "intro\n\n```bash\ncairn search foo\n";
    let err = extract_code_blocks(md).expect_err("unterminated fence must fail");
    assert!(
        matches!(err, CompatError::Malformed { kind: "fence", .. }),
        "expected Malformed fence error, got: {err:?}"
    );
}

#[test]
fn cli_validator_rejects_retrieve_without_discriminator() {
    // `--limit` exists on the session variant but ArgsRecord/ArgsTurn/etc.
    // don't share it; without `--session` (or any other discriminator) the
    // example matches no variant cleanly.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn retrieve --limit 5".into(),
        line: 13,
    };
    // Either AmbiguousVariant (no/multi match) or Malformed surface a real
    // failure for this drift case; we just need it to not silently pass.
    let err = validate_cli_block(&block, &doc())
        .expect_err("retrieve without a unique discriminator must fail");
    assert!(
        matches!(
            err,
            CompatError::AmbiguousVariant { .. } | CompatError::Malformed { .. }
        ),
        "expected AmbiguousVariant or Malformed, got: {err:?}"
    );
}

#[test]
fn cli_validator_rejects_retrieve_with_two_discriminators() {
    // Positional id (ArgsRecord) plus --session (ArgsSession) selects two
    // variants; clap's ArgGroup rejects this and so should the compat gate.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn retrieve 01H8XGJWBWBAQ4N1NQK1A8X9YZ --session s1".into(),
        line: 17,
    };
    let err = validate_cli_block(&block, &doc())
        .expect_err("retrieve with multiple discriminators must fail");
    // Either >1 or 0 matches is a failure surface — both indicate the
    // example doesn't fit any single clap-required variant cleanly.
    assert!(
        matches!(err, CompatError::AmbiguousVariant { matched_variants, .. } if matched_variants != 1),
        "expected AmbiguousVariant with !=1 match, got: {err:?}"
    );
}

#[test]
fn cli_validator_accepts_retrieve_with_single_discriminator() {
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn retrieve --session s1 --limit 5".into(),
        line: 1,
    };
    validate_cli_block(&block, &doc())
        .expect("retrieve with exactly one discriminator must validate");
}

#[test]
fn cli_validator_rejects_value_flag_without_value() {
    // `--mode` takes a value; trailing nothing must fail compat (clap would
    // also reject this at runtime).
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn search --mode".into(),
        line: 21,
    };
    let err = validate_cli_block(&block, &doc()).expect_err("missing flag value must fail");
    assert!(
        matches!(
            err,
            CompatError::Malformed {
                kind: "cli",
                line: 21,
                ..
            }
        ),
        "expected Malformed cli error, got: {err:?}"
    );
}

#[test]
fn cli_validator_accepts_retrieve_profile_anyof_branch() {
    // ArgsProfile selects via `--profile` and requires one of {user,agent}.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn retrieve --profile --user USER".into(),
        line: 1,
    };
    validate_cli_block(&block, &doc())
        .expect("retrieve --profile --user must validate via anyOf branch");
}

#[test]
fn cli_validator_accepts_summarize_repeatable_positional() {
    // summarize.record_ids is a repeatable positional; multiple positional
    // tokens must validate, not trip the positional-cap check.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn summarize ID1 ID2 ID3".into(),
        line: 1,
    };
    validate_cli_block(&block, &doc()).expect("repeatable positional must accept multiple tokens");
}

#[test]
fn cli_validator_rejects_invalid_enum_value() {
    // search --mode is enum(keyword,semantic,hybrid); `bogus` must fail.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn search --mode bogus".into(),
        line: 23,
    };
    let err = validate_cli_block(&block, &doc()).expect_err("invalid enum must fail");
    assert!(
        matches!(
            err,
            CompatError::Malformed {
                kind: "cli",
                line: 23,
                ..
            }
        ),
        "expected Malformed cli error, got: {err:?}"
    );
}

#[test]
fn cli_validator_accepts_uppercase_placeholder_for_enum() {
    // The generated skill renders enum-typed flags with placeholder values
    // (e.g. `--mode MODE`). Those must pass to keep the generator's own
    // examples valid.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn search --mode MODE QUERY".into(),
        line: 1,
    };
    validate_cli_block(&block, &doc()).expect("placeholder enum values must validate");
}

#[test]
fn cli_validator_accepts_quoted_positional_value() {
    // `search` takes an optional positional `query`; a quoted multi-word
    // string must remain one positional, not split into two.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn search --mode hybrid \"project status\"".into(),
        line: 1,
    };
    validate_cli_block(&block, &doc())
        .expect("quoted positional must not be split into multiple tokens");
}

#[test]
fn cli_validator_accepts_retrieve_profile_agent_branch() {
    // ArgsProfile's anyOf permits either user OR agent — both branches
    // should validate, not just the first.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn retrieve --profile --agent AGENT".into(),
        line: 1,
    };
    validate_cli_block(&block, &doc())
        .expect("retrieve --profile --agent must validate via second anyOf branch");
}

#[test]
fn cli_validator_rejects_invalid_u8_value() {
    // retrieve --depth is u8; non-integer value must fail.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn retrieve --folder PATH --depth nope".into(),
        line: 1,
    };
    let err = validate_cli_block(&block, &doc()).expect_err("non-integer u8 must fail");
    assert!(
        matches!(err, CompatError::Malformed { kind: "cli", .. }),
        "expected Malformed cli error, got: {err:?}"
    );
}

#[test]
fn cli_validator_pairs_variants_by_content() {
    // Pairs each CliCommand to its schema variant by content. If pairing
    // fell back to array-position, a session-only invocation could trip the
    // validator when the schema/CLI orders ever diverged. The current happy
    // path proves the *content-based* pairing actually validates the right
    // variant — both `--session SESSION_ID` (Session) and `--session
    // SESSION_ID --turn TURN_ID` (Turn) must succeed.
    let d = doc();
    for line in [
        "cairn retrieve --session SESSION_ID",
        "cairn retrieve --session SESSION_ID --turn TURN_ID",
    ] {
        let block = CodeBlock {
            lang: "bash".into(),
            body: line.into(),
            line: 1,
        };
        validate_cli_block(&block, &d)
            .unwrap_or_else(|e| panic!("expected `{line}` to validate, got: {e}"));
    }
}

#[test]
fn live_skill_md_passes_compat_checks() {
    let md = live_skill_md();
    let d = doc();
    for block in extract_code_blocks(&md).expect("live SKILL.md must parse") {
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
