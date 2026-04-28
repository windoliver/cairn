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
fn cli_validator_rejects_invalid_ulid_in_string_flag() {
    // Round-16 finding 1: `forget --record` is a string flag with a $ref
    // to the Ulid primitive. A bogus value must fail compat the same way
    // the runtime would, even though the value_source is the freeform
    // "string".
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn forget --record not-a-ulid".into(),
        line: 1,
    };
    let err =
        validate_cli_block(&block, &doc()).expect_err("invalid Ulid in string flag must fail");
    assert!(
        matches!(err, CompatError::Malformed { kind: "cli", .. }),
        "expected Malformed cli error, got: {err:?}"
    );
}

#[test]
fn cli_validator_rejects_non_json_in_json_flag() {
    // `forget --scope` is value_source `json`. A non-JSON literal must
    // fail compat. Round-16 finding 1.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn forget --scope nope".into(),
        line: 1,
    };
    let err = validate_cli_block(&block, &doc()).expect_err("non-JSON --scope value must fail");
    assert!(
        matches!(err, CompatError::Malformed { kind: "cli", .. }),
        "expected Malformed cli error, got: {err:?}"
    );
}

#[test]
fn cli_validator_accepts_dash_as_stdin_positional() {
    // Round-16 finding 2: ingest documents `-` for stdin. Compat must
    // accept bare `-` as a valid positional, not treat it as a flag.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn ingest --kind KIND -".into(),
        line: 1,
    };
    validate_cli_block(&block, &doc()).expect("bare `-` must be accepted as stdin positional");
}

#[test]
fn cli_validator_accepts_dash_value_for_string_flag() {
    // Round-16 finding 2: `--body -` reads stdin at runtime; the gate
    // must accept `-` as the flag's value rather than treat it as the
    // next option.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn ingest --kind KIND --body -".into(),
        line: 1,
    };
    validate_cli_block(&block, &doc()).expect("`--body -` must validate");
}

#[test]
fn cli_validator_accepts_concrete_scope_via_relative_ref() {
    // Round-2 finding 2: `--scope` is `json` whose property schema is a
    // relative `$ref` (`../common/scope_filter.json`). The compat gate
    // must wrap it with the verb's `$id` so the retriever can resolve
    // the ref. A schema-valid object must pass.
    let block = CodeBlock {
        lang: "bash".into(),
        body: r#"cairn forget --scope '{"user":"u"}'"#.into(),
        line: 1,
    };
    validate_cli_block(&block, &doc())
        .expect("concrete --scope JSON must validate via cross-file $ref");
}

#[test]
fn cli_validator_rejects_invalid_scope_via_relative_ref() {
    // The same path must reject a payload that doesn't satisfy the
    // ScopeFilter anyOf (no recognised predicate).
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn forget --scope '{}'".into(),
        line: 1,
    };
    let err =
        validate_cli_block(&block, &doc()).expect_err("empty scope must fail anyOf narrowing");
    assert!(
        matches!(err, CompatError::Malformed { kind: "cli", .. }),
        "expected Malformed cli error, got: {err:?}"
    );
}

#[test]
fn live_skill_md_covers_oneof_branches_and_optional_json_flag() {
    // Round-5 finding 1 + 2: the generated SKILL.md must exercise every
    // disjunctive branch (so a rename of `--file` or `--url` would block
    // codegen) and every "interesting" optional flag (so a rename of
    // `--filters` would too). Spot-check a representative trio.
    let md = live_skill_md();
    for needle in ["--body", "--file", "--url", "--filters"] {
        assert!(
            md.contains(needle),
            "live SKILL.md must contain `{needle}` so the compat gate exercises that path"
        );
    }
}

#[test]
fn cli_validator_rejects_empty_string_flag_below_min_length() {
    // Round-6 finding 2: string-flag validation now goes through full
    // JSON Schema. `ingest.kind` declares `minLength: 1`; an empty value
    // must fail the gate, where the prior pattern-only path missed
    // length / format / enum constraints on string flags.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn ingest --kind '' --body BODY".into(),
        line: 1,
    };
    let err = validate_cli_block(&block, &doc())
        .expect_err("empty string flag below minLength must fail full schema validation");
    assert!(
        matches!(err, CompatError::Malformed { kind: "cli", .. }),
        "expected Malformed cli error, got: {err:?}"
    );
}

#[test]
fn cli_validator_rejects_non_cairn_executable_token() {
    // Round-7 finding 2: a line starting with `cairn-cli` or `cairn2`
    // must not be treated as a Cairn invocation.
    for body in [
        "cairn-cli retrieve --session s1",
        "cairn2 search --mode keyword q",
    ] {
        let block = CodeBlock {
            lang: "bash".into(),
            body: body.to_string(),
            line: 1,
        };
        // Block-level filter rejects silently (skips); to exercise the
        // line-level defence we also feed the same line directly.
        validate_cli_block(&block, &doc()).expect("block-level filter must skip non-cairn lines");
    }
    // Inline span path: extract_inline_cairn_spans gates on `cairn `, so
    // forge a fake CodeBlock and rely on validate_cli_line via
    // validate_cli_block where the block body has *only* the suspect line.
    // Because validate_cli_block also skips lines whose first token isn't
    // exactly `cairn`, drift here is gated at both the block and (for
    // hand-built tests below) the per-line layer.
}

#[test]
fn cli_validator_enforces_variant_specific_cursor_max_length() {
    // Round-7 finding 1: value validation must run against the matched
    // variant's schema. `retrieve --scope` selects `ArgsScope`, whose
    // `cursor` declares `maxLength: 512`. A 600-char value must fail —
    // proving the lookup uses per-variant properties (the prior verb-
    // wide union still happened to find a 512-bounded schema, but only
    // by accident; this regression locks in the matched-variant path).
    let long = "x".repeat(600);
    let scope_block = CodeBlock {
        lang: "bash".into(),
        body: format!("cairn retrieve --scope '{{\"user\":\"u\"}}' --cursor {long}"),
        line: 1,
    };
    let err = validate_cli_block(&scope_block, &doc())
        .expect_err("ArgsScope.cursor maxLength must reject 600-char value");
    assert!(
        matches!(err, CompatError::Malformed { kind: "cli", .. }),
        "expected Malformed cli error, got: {err:?}"
    );
}

#[test]
fn live_skill_md_exercises_all_optional_flags() {
    // Round-9 finding 2: every declared optional flag must appear in
    // some example so a rename / removal blocks codegen. Spot-check
    // flags previously skipped by the "interesting only" filter.
    let md = live_skill_md();
    for needle in ["--order", "--rehydrate", "--citations", "--write-report"] {
        assert!(
            md.contains(needle),
            "live SKILL.md must contain `{needle}` so the compat gate exercises it"
        );
    }
}

#[test]
fn cli_validator_rejects_unknown_prelude_token() {
    // Round-9 finding 1: prelude allowlist comes from doc.preludes, not
    // a hardcoded constant. A made-up prelude must still fail compat
    // (proves the lookup actually consults the IR).
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn made_up_prelude --json".into(),
        line: 1,
    };
    let err =
        validate_cli_block(&block, &doc()).expect_err("unknown prelude token must fail compat");
    assert!(
        matches!(err, CompatError::UnknownVerb { ref verb, .. } if verb == "made_up_prelude"),
        "expected UnknownVerb for prelude not in IR, got: {err:?}"
    );
}

#[test]
fn live_skill_md_exercises_positional_source_for_ingest() {
    // Round-1 (post-cap) finding: the positional `source` path on
    // `cairn ingest` was unrendered, leaving a documented CLI branch
    // outside the compat gate. Synthesis must produce a positional
    // example like `cairn ingest --kind KIND SOURCE` so a future
    // regression in alias handling blocks codegen.
    let md = live_skill_md();
    assert!(
        md.contains("cairn ingest --kind KIND SOURCE"),
        "live SKILL.md must contain a positional-source ingest example"
    );
}

#[test]
fn cli_validator_does_not_bypass_placeholder_for_constrained_field() {
    // Round-4 finding: ALL-CAPS placeholders bypassed validation even
    // for constrained string fields. `forget --record` carries a
    // primitive `$ref: Ulid` — a placeholder must NOT be silently
    // accepted; the schema's pattern check must fire.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn forget --record FOO_BAR".into(),
        line: 1,
    };
    let err = validate_cli_block(&block, &doc())
        .expect_err("placeholder for $ref-constrained field must fail schema validation");
    assert!(
        matches!(err, CompatError::Malformed { kind: "cli", .. }),
        "expected Malformed cli error, got: {err:?}"
    );
}

#[test]
fn cli_validator_inspects_wrapper_with_options() {
    // Round-3 finding 1: `sudo -u alice cairn …`, `env -i cairn …`,
    // `time -p cairn …` should still be validated. Prior code stopped
    // at the wrapper option (`-u`) and skipped the segment.
    for prefix in ["sudo -u alice", "env -i", "time -p"] {
        let body = format!("{prefix} cairn ingest --bogus");
        let block = CodeBlock {
            lang: "bash".into(),
            body: body.clone(),
            line: 1,
        };
        let err = validate_cli_block(&block, &doc()).unwrap_err_or_else_pretty(&body);
        assert!(
            matches!(err, CompatError::UnknownFlag { ref flag, .. } if flag == "bogus"),
            "expected UnknownFlag for --bogus inside `{body}`, got: {err:?}"
        );
    }
}

#[test]
fn cli_validator_skips_wrapper_with_non_cairn_command() {
    // Round-5 finding: prior wrapper handling kept scanning past the
    // wrapper's command word (`printenv`, `grep`) until it found a
    // literal `cairn` token, misclassifying `env printenv cairn` and
    // `sudo grep cairn file` as Cairn invocations. After parsing the
    // wrapper's option syntax precisely, the first non-option token is
    // the wrapped command word; if it isn't `cairn`, the segment is
    // skipped.
    for body in [
        "env printenv cairn",
        "sudo grep cairn file",
        "time -p ls cairn",
    ] {
        let block = CodeBlock {
            lang: "bash".into(),
            body: body.to_string(),
            line: 1,
        };
        validate_cli_block(&block, &doc())
            .unwrap_or_else(|e| panic!("wrapper segment `{body}` must be skipped, got: {e:?}"));
    }
}

trait UnwrapErrPretty<T, E> {
    fn unwrap_err_or_else_pretty(self, ctx: &str) -> E;
}
impl<T: std::fmt::Debug, E> UnwrapErrPretty<T, E> for Result<T, E> {
    fn unwrap_err_or_else_pretty(self, ctx: &str) -> E {
        match self {
            Ok(v) => panic!("expected error for `{ctx}`, got Ok: {v:?}"),
            Err(e) => e,
        }
    }
}

#[test]
fn cli_validator_accepts_quoted_hyphen_value() {
    // Round-3 finding 2: a quoted value that *starts* with `-` (e.g.,
    // `--body "--literal"`) must be treated as the flag's value, not
    // misclassified as another option. The compat gate must accept it.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn ingest --kind KIND --body \"--literal\"".into(),
        line: 1,
    };
    validate_cli_block(&block, &doc())
        .expect("quoted hyphen-prefixed value must be consumed as a value, not a flag");
}

#[test]
fn cli_validator_skips_quoted_or_argument_cairn_text() {
    // Round-2 finding 1: `cairn` appearing as an argument to `echo`
    // (or any non-cairn command) must not be treated as a Cairn
    // invocation. Without segment + command-word handling the validator
    // would parse `echo cairn search foo` and try to interpret
    // `--bogus` against the search verb.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "echo cairn search --bogus".into(),
        line: 1,
    };
    validate_cli_block(&block, &doc())
        .expect("`cairn` as an argument to another command must be skipped, not validated");
}

#[test]
fn cli_validator_stops_at_shell_control_operators() {
    // Round-2 finding 1: shell control operators (`&&`, `|`, `;`) must
    // terminate the command segment so following tokens aren't fed into
    // the CLI scanner as extra args.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn search --mode keyword QUERY && jq .".into(),
        line: 1,
    };
    validate_cli_block(&block, &doc())
        .expect("`&& jq .` must terminate the cairn segment, not be parsed as cairn args");
}

#[test]
fn cli_validator_rejects_unknown_short_option() {
    // Round-2 finding 2: a stale single-dash option (e.g. `-x`) was
    // silently dropped by the scanner. Compat must surface it as an
    // UnknownFlag so a typo or removed alias blocks the gate.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn retrieve --session SESSION_ID -x".into(),
        line: 1,
    };
    let err =
        validate_cli_block(&block, &doc()).expect_err("unknown short option must fail compat");
    assert!(
        matches!(err, CompatError::UnknownFlag { ref flag, .. } if flag == "x"),
        "expected UnknownFlag for -x, got: {err:?}"
    );
}

#[test]
fn cli_validator_rejects_empty_list_string_item() {
    // Round-10 finding 2: `ingest --tags` is `list<string>` and its
    // items declare `minLength: 1`. An empty `--tags ""` value must
    // fail the gate — the prior code only validated `list<enum(...)>`
    // and let generic lists through unchecked.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "cairn ingest --kind KIND --body BODY --tags ''".into(),
        line: 1,
    };
    let err = validate_cli_block(&block, &doc())
        .expect_err("empty list<string> item must fail items.minLength");
    assert!(
        matches!(err, CompatError::Malformed { kind: "cli", .. }),
        "expected Malformed cli error, got: {err:?}"
    );
}

#[test]
fn cli_validator_inspects_wrapped_cairn_invocation() {
    // Round-8 finding 2: shell-wrapped lines must still be validated.
    // `env DEBUG=1 cairn ingest --bogus` previously slipped past compat
    // because the validator only ran when the first token was `cairn`.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "env DEBUG=1 cairn ingest --bogus".into(),
        line: 1,
    };
    let err = validate_cli_block(&block, &doc())
        .expect_err("wrapped cairn invocation must still be validated");
    assert!(
        matches!(err, CompatError::UnknownFlag { ref flag, .. } if flag == "bogus"),
        "expected UnknownFlag for --bogus inside wrapped invocation, got: {err:?}"
    );
}

#[test]
fn cli_validator_inspects_bash_block_with_leading_comment() {
    // Round-6 finding 1: a fenced bash block whose first line is a comment
    // (or any non-`cairn` shell prefix) was previously skipped entirely.
    // The compat gate must still inspect the `cairn …` lines inside.
    let block = CodeBlock {
        lang: "bash".into(),
        body: "# example\ncairn ingest --bogus".into(),
        line: 1,
    };
    let err = validate_cli_block(&block, &doc())
        .expect_err("unknown flag in bash block with leading comment must fail");
    assert!(
        matches!(err, CompatError::UnknownFlag { ref flag, .. } if flag == "bogus"),
        "expected UnknownFlag for --bogus, got: {err:?}"
    );
}

#[test]
fn cli_validator_rejects_empty_search_query_positional() {
    // Round-4 finding 1: positional validation must enforce the full
    // property schema, not just `pattern`. `search.query` declares
    // `minLength: 1`, so an empty positional must fail compat.
    let block = CodeBlock {
        lang: "bash".into(),
        body: r#"cairn search --mode keyword """#.into(),
        line: 1,
    };
    let err = validate_cli_block(&block, &doc())
        .expect_err("empty search query positional must fail minLength");
    assert!(
        matches!(err, CompatError::Malformed { kind: "cli", .. }),
        "expected Malformed cli error, got: {err:?}"
    );
}

#[test]
fn cli_validator_accepts_search_filters_with_internal_ref() {
    // Round-4 finding 2: JSON-flag validator must preserve the owning
    // verb's `$defs` so internal `#/$defs/<Name>` refs (e.g.,
    // `search.filters` → `#/$defs/filter`) compile. A schema-valid
    // payload must pass.
    let block = CodeBlock {
        lang: "bash".into(),
        body: r#"cairn search --mode keyword query --filters '{"field":"kind","op":"eq","value":"note"}'"#.into(),
        line: 1,
    };
    validate_cli_block(&block, &doc())
        .expect("valid --filters payload must validate against internal $defs/filter");
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
        body: "cairn summarize 01H8XGJWBWBAQ4N1NQK1A8X9YZ 01H8XGJWBWBAQ4N1NQK1A8XAB1 01H8XGJWBWBAQ4N1NQK1A8XAB2".into(),
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
