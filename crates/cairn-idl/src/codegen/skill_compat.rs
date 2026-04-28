//! Compatibility checks for the generated Cairn skill (issue #70).
//!
//! Extracts CLI and JSON examples from `skills/cairn/SKILL.md` and validates
//! them against the IDL: every `cairn <verb>` invocation must reference a real
//! verb (or protocol prelude) and supply only known flags; every JSON block
//! must parse against the input schema of its declared verb.
//!
//! These checks run alongside drift detection so the skill cannot reference a
//! retired verb, an invented kind, or a flag that no longer exists.

use std::collections::{BTreeMap, BTreeSet};

use crate::codegen::ir::{CliCommand, CliFlag, CliShape, Document, VerbDef};

/// Long flags every CLI invocation may use even though they aren't declared on
/// individual verbs. `--json` is the universal output mode (see `CLAUDE.md`
/// §6.5); `--help` is provided by clap on every subcommand.
const UNIVERSAL_FLAGS: &[&str] = &["json", "help"];

/// One code block extracted from the skill markdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeBlock {
    /// Language tag from the fence (`bash`, `json`, …) or `"inline"` for an
    /// inline code span.
    pub lang: String,
    /// Raw block body without surrounding fences / backticks.
    pub body: String,
    /// 1-indexed line number where the block opens in the source markdown.
    pub line: usize,
}

/// A skill-compat failure.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CompatError {
    /// `cairn <verb>` referenced a verb that is neither in the IDL nor a
    /// recognised protocol prelude.
    #[error("unknown verb `{verb}` at line {line}")]
    UnknownVerb {
        /// The verb token after `cairn`.
        verb: String,
        /// 1-indexed line number of the offending block.
        line: usize,
    },
    /// `cairn <verb> --foo` referenced a flag absent from the IDL `CliShape`.
    #[error("unknown flag `--{flag}` for verb `{verb}` at line {line}")]
    UnknownFlag {
        /// The verb the flag was passed to.
        verb: String,
        /// The unknown long-flag name (without leading `--`).
        flag: String,
        /// 1-indexed line number of the offending block.
        line: usize,
    },
    /// JSON example failed to validate against the input schema for its verb.
    #[error("json example for `{verb}` at line {line}: {detail}")]
    SchemaMismatch {
        /// The verb whose input schema was used.
        verb: String,
        /// Validator detail message.
        detail: String,
        /// 1-indexed line number of the offending block.
        line: usize,
    },
    /// A tagged-union verb (e.g., `retrieve`, `forget`) example matched zero
    /// or more than one variant — the underlying clap `ArgGroup` requires
    /// exactly one discriminator.
    #[error(
        "verb `{verb}` matched {matched_variants} variant(s) at line {line}; tagged-union verbs require exactly one discriminator"
    )]
    AmbiguousVariant {
        /// The verb id.
        verb: String,
        /// Number of variants the example selected.
        matched_variants: usize,
        /// 1-indexed line number.
        line: usize,
    },
    /// The block could not be parsed (malformed JSON, empty CLI command, …).
    #[error("malformed {kind} block at line {line}: {detail}")]
    Malformed {
        /// Block kind (`"cli"` or `"json"`).
        kind: &'static str,
        /// Parser detail.
        detail: String,
        /// 1-indexed line number of the offending block.
        line: usize,
    },
}

/// Extract code blocks from `markdown`. Picks up both fenced blocks
/// (```` ```lang ... ``` ````) and inline spans that look like CLI invocations
/// (`` `cairn ...` ``). Inline spans get the lang tag `"inline"`.
///
/// # Errors
/// Returns [`CompatError::Malformed`] when the source ends with an
/// unterminated fenced block — without this the trailing example would be
/// silently dropped and could hide drift from the gate.
pub fn extract_code_blocks(markdown: &str) -> Result<Vec<CodeBlock>, CompatError> {
    let mut out = Vec::new();
    let mut in_fence = false;
    let mut fence_lang = String::new();
    let mut fence_body = String::new();
    let mut fence_open_line = 0usize;

    for (idx, line) in markdown.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("```") {
            if in_fence {
                out.push(CodeBlock {
                    lang: std::mem::take(&mut fence_lang),
                    body: std::mem::take(&mut fence_body),
                    line: fence_open_line,
                });
                in_fence = false;
            } else {
                in_fence = true;
                fence_lang = rest.trim().to_string();
                fence_body.clear();
                fence_open_line = line_no;
            }
            continue;
        }
        if in_fence {
            if !fence_body.is_empty() {
                fence_body.push('\n');
            }
            fence_body.push_str(line);
        } else if !is_heading(trimmed) {
            // Headings like `## \`cairn retrieve\`` are taxonomy markers, not
            // user-facing examples — skipping them keeps the validator from
            // treating a section title as an invocation.
            out.extend(extract_inline_cairn_spans(line, line_no));
        }
    }

    if in_fence {
        return Err(CompatError::Malformed {
            kind: "fence",
            detail: "unterminated fenced code block".to_string(),
            line: fence_open_line,
        });
    }
    Ok(out)
}

/// True when `trimmed_line` begins with one or more ATX `#` markers followed
/// by a space — i.e., a markdown heading.
fn is_heading(trimmed_line: &str) -> bool {
    let after_hashes = trimmed_line.trim_start_matches('#');
    after_hashes.len() < trimmed_line.len() && after_hashes.starts_with(' ')
}

/// Scan one line for `` `cairn …` `` inline spans.
fn extract_inline_cairn_spans(line: &str, line_no: usize) -> Vec<CodeBlock> {
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            // Find matching closing backtick.
            let start = i + 1;
            let Some(rel_end) = line[start..].find('`') else {
                break;
            };
            let end = start + rel_end;
            let span = &line[start..end];
            if span.trim_start().starts_with("cairn ") {
                out.push(CodeBlock {
                    lang: "inline".to_string(),
                    body: span.to_string(),
                    line: line_no,
                });
            }
            i = end + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// Extract the verb file's top-level `$id` so JSON-typed flag values can
/// resolve relative `$ref`s through the schema retriever.
fn verb_base_id(verb_def: &VerbDef) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(&verb_def.args_schema_bytes)
        .ok()
        .and_then(|s| {
            s.get("$id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
}

/// Pull the verb args schema's top-level `$defs` (if any) so JSON examples
/// referencing internal `#/$defs/<Name>` refs can compile against a wrapper
/// schema. Without this, validating `--filters '{...}'` for a property whose
/// schema is `$ref: "#/$defs/filter"` would fail compilation even though the
/// example is valid.
fn verb_owner_defs(verb_def: &VerbDef) -> Option<serde_json::Value> {
    let schema: serde_json::Value = serde_json::from_slice(&verb_def.args_schema_bytes).ok()?;
    schema.get("$defs").cloned()
}

/// Build a self-contained validator schema by wrapping `prop_schema` with the
/// owning verb file's `$id` and `$defs`. This lets a property whose body uses
/// either internal (`#/$defs/...`) or relative (`../common/...`) `$ref`s
/// compile and validate against the cross-file retriever.
fn wrap_property_schema(
    prop_schema: &serde_json::Value,
    base_id: Option<&str>,
    owner_defs: Option<&serde_json::Value>,
) -> serde_json::Value {
    let mut wrapper = serde_json::Map::new();
    wrapper.insert(
        "$schema".into(),
        serde_json::Value::String("https://json-schema.org/draft/2020-12/schema".into()),
    );
    if let Some(id) = base_id {
        wrapper.insert("$id".into(), serde_json::Value::String(id.to_string()));
    }
    if let Some(defs) = owner_defs {
        wrapper.insert("$defs".into(), defs.clone());
    }
    if let Some(prop_obj) = prop_schema.as_object() {
        for (k, v) in prop_obj {
            // Don't let the property override the wrapper's `$defs` / `$id`.
            if k == "$defs" || k == "$id" || k == "$schema" {
                continue;
            }
            wrapper.insert(k.clone(), v.clone());
        }
    }
    serde_json::Value::Object(wrapper)
}

/// Validate one CLI block against the IDL.
///
/// Accepts any `cairn <verb> [flags]` line where `<verb>` is either a
/// canonical verb id or a protocol prelude (`status`, `handshake`) and every
/// `--long` flag matches a `CliFlag::long` for that verb's [`CliShape`].
///
/// # Errors
/// Returns [`CompatError`] when the verb is unknown, a flag is unknown, or the
/// block is malformed.
pub fn validate_cli_block(block: &CodeBlock, doc: &Document) -> Result<(), CompatError> {
    // Join shell line-continuations (`\` at EOL) into single logical commands
    // first — without this, `cairn retrieve --session S \\n  --include bogus`
    // would only validate the leading line and silently accept the stale
    // continuation.
    for logical in join_continuations(&block.body) {
        let line = logical.trim();
        if line.is_empty() {
            continue;
        }
        let owned = shell_split(line, block.line)?;
        for segment in command_segments(&owned) {
            // Skip env-var assignments (`FOO=bar …`) and known wrappers
            // (`time cairn …`, `env -i cairn …`, `sudo -u alice cairn …`)
            // until we hit `cairn`. If the wrapped invocation isn't
            // actually `cairn` (e.g., `echo cairn …`, `sudo -u alice ls`)
            // the segment is skipped — round-3 found the prior bare-token
            // skip mistreated `sudo -u alice cairn …` as wrapper option
            // pointing at `alice`.
            let Some(cmd_pos) = locate_cairn_command(segment) else {
                continue;
            };
            validate_cli_line_tokens(&segment[cmd_pos..], block.line, doc)?;
        }
    }
    Ok(())
}

/// Split a token stream on shell control operators (`;`, `&&`, `||`, `|`,
/// `&`) and strip any trailing comment (`#…`). Each returned segment is a
/// candidate simple command. Round-2 finding: without this `cairn search
/// foo && jq …` would feed `&&` and `jq` into the CLI scanner.
fn command_segments(tokens: &[Token]) -> Vec<&[Token]> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, tok) in tokens.iter().enumerate() {
        // Operator tokens are emitted unquoted by shell — a quoted `;` is
        // literal data, not a separator.
        if !tok.quoted && matches!(tok.value.as_str(), ";" | "&&" | "||" | "|" | "&" | "|&") {
            if start < i {
                out.push(&tokens[start..i]);
            }
            start = i + 1;
        } else if !tok.quoted && tok.value.starts_with('#') {
            // Trailing shell comment — drop it and everything after.
            if start < i {
                out.push(&tokens[start..i]);
            }
            return out;
        }
    }
    if start < tokens.len() {
        out.push(&tokens[start..]);
    }
    out
}

/// Position of the literal `cairn` command word in a simple-command token
/// stream. Walks past env-var assignments and parses each known wrapper's
/// option syntax (consuming option-values where the wrapper takes them) so
/// the *actual* wrapped command is identified — round-5 finding: prior
/// scanning consumed every later token until it found a `cairn` literal,
/// which falsely classified `env printenv cairn` and `sudo grep cairn file`
/// as Cairn invocations.
fn locate_cairn_command(segment: &[Token]) -> Option<usize> {
    let mut i = 0;
    while i < segment.len() {
        let tok = &segment[i];
        let val = tok.value.as_str();
        if !tok.quoted && is_env_var_assignment(val) {
            i += 1;
            continue;
        }
        if !tok.quoted
            && let Some(opts_with_value) = wrapper_options_with_value(val)
        {
            i += 1;
            // Consume this wrapper's options. Stop at the first non-option
            // non-assignment token; that's the wrapped command word.
            while i < segment.len() {
                let opt = &segment[i];
                if opt.quoted || !opt.value.starts_with('-') {
                    break;
                }
                if opt.value == "--" {
                    i += 1;
                    break;
                }
                // `--long=value` form is self-contained — never consume the
                // next token. Only the bare `--long`/`-x` forms can take the
                // following token as a value (round-6 finding 2).
                let has_inline = opt.value.starts_with("--") && opt.value.contains('=');
                let opt_name = if has_inline {
                    opt.value
                        .split_once('=')
                        .map_or(opt.value.as_str(), |(n, _)| n)
                } else {
                    opt.value.as_str()
                };
                let consumes_value = !has_inline && opts_with_value.contains(&opt_name);
                i += 1;
                if consumes_value && i < segment.len() {
                    i += 1;
                }
            }
            continue;
        }
        // Non-wrapper, non-env token — this is the (possibly wrapped)
        // command word. Validate iff it's literally `cairn`.
        return if !tok.quoted && val == "cairn" {
            Some(i)
        } else {
            None
        };
    }
    None
}

/// Per-wrapper allowlist of options that consume the *next* token as a
/// value. Returns `Some(&[])` for known wrappers without value-bearing
/// options (`nohup`, `exec`), or `None` for non-wrappers.
fn wrapper_options_with_value(name: &str) -> Option<&'static [&'static str]> {
    match name {
        "env" => Some(&[
            "-u",
            "-S",
            "-C",
            "--unset",
            "--chdir",
            "--split-string",
            "--block-signal",
            "--default-signal",
            "--ignore-signal",
        ]),
        "sudo" => Some(&[
            "-u",
            "-U",
            "-g",
            "-D",
            "-p",
            "-h",
            "-r",
            "-t",
            "-T",
            "-C",
            "-c",
            "--user",
            "--other-user",
            "--group",
            "--chdir",
            "--prompt",
            "--host",
            "--role",
            "--type",
            "--command-timeout",
            "--close-from",
            "--login-class",
        ]),
        "time" => Some(&["-o", "-f", "--output", "--format"]),
        "nohup" | "exec" => Some(&[]),
        _ => None,
    }
}

fn is_env_var_assignment(tok: &str) -> bool {
    let Some(eq_pos) = tok.find('=') else {
        return false;
    };
    if eq_pos == 0 {
        return false;
    }
    let key = &tok[..eq_pos];
    key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && key
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
}

/// Collapse shell line-continuations: a line ending in an unescaped trailing
/// backslash is concatenated with the next line (the backslash + newline are
/// replaced by a single space).
fn join_continuations(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for raw in body.lines() {
        if let Some(stripped) = raw.strip_suffix('\\') {
            current.push_str(stripped);
            current.push(' ');
        } else {
            current.push_str(raw);
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Validate one whitespace-tokenised `cairn …` line.
fn validate_cli_line_tokens(
    owned: &[Token],
    source_line: usize,
    doc: &Document,
) -> Result<(), CompatError> {
    let mut tokens = owned.iter();
    let verb = consume_executable_and_verb(&mut tokens, source_line)?;

    // Resolve the token to its CliCommand variants. Match on the configured
    // `CliCommand::command` (so a future IDL rename that diverges from
    // `verb.id` flows through), with a fallback to `verb.id`. Preludes carry
    // no flags or positionals.
    let verb_def: Option<&VerbDef> = verb_for_command(doc, verb);
    // Preludes are derived from the IR (not a hardcoded list) so a future
    // change to the prelude set in `Document::preludes` flows through compat
    // and a stale `cairn <prelude>` example fails the gate. Round-9 found
    // the previous constant could fall out of sync silently.
    let is_prelude = doc.preludes.iter().any(|p| p.id == verb);
    let cmds: Vec<&CliCommand> = if is_prelude {
        Vec::new()
    } else if let Some(def) = verb_def {
        match &def.cli {
            CliShape::Single(c) => vec![c],
            CliShape::Variants(vs) => vs.iter().collect(),
        }
    } else {
        return Err(CompatError::UnknownVerb {
            verb: verb.to_string(),
            line: source_line,
        });
    };
    let allowed_flags: BTreeMap<&str, &CliFlag> = cmds
        .iter()
        .flat_map(|c| c.flags.iter().map(|f| (f.long.as_str(), f)))
        .collect();
    // Positional capacity: 0 if no variant declares one; usize::MAX if any
    // variant marks its positional repeatable (clap `num_args(1..)`); else 1.
    let positional_capacity = if cmds
        .iter()
        .any(|c| c.positional.as_ref().is_some_and(|p| p.repeatable))
    {
        usize::MAX
    } else {
        usize::from(cmds.iter().any(|c| c.positional.is_some()))
    };

    let TokenScan {
        positional_count,
        used_field_names,
        positional_values,
        flag_occurrences,
        list_flag_values,
    } = scan_tokens(verb, source_line, tokens, &allowed_flags)?;

    if positional_count > positional_capacity {
        return Err(CompatError::Malformed {
            kind: "cli",
            detail: format!(
                "verb `{verb}` accepts {positional_capacity} positional arg(s), got {positional_count}"
            ),
            line: source_line,
        });
    }

    // Resolve which variant (if any) the example targets so value validation
    // uses the matched variant's schema. For tagged-union verbs with two
    // variants reusing the same field name (e.g., `retrieve --cursor` under
    // both `ArgsSession` and `ArgsScope` with different schemas), the prior
    // verb-wide property union could validate against the wrong one and let
    // drift through the gate (round-7 finding).
    let matched_idx = if cmds.len() == 1 {
        Some(0usize)
    } else if let Some(def) = verb_def {
        find_matching_variant_index(def, &cmds, &used_field_names, positional_count)
    } else {
        None
    };
    let per_cmd_schemas = verb_def
        .map(|def| variant_property_schemas(def, &cmds))
        .unwrap_or_default();
    let cmd_props = matched_idx
        .and_then(|idx| per_cmd_schemas.get(idx))
        .cloned()
        .unwrap_or_default();

    let base_id = verb_def.and_then(verb_base_id);
    let owner_defs = verb_def.and_then(verb_owner_defs);

    apply_value_validation(
        verb,
        source_line,
        &cmds,
        &allowed_flags,
        &cmd_props,
        &positional_values,
        &flag_occurrences,
        &list_flag_values,
        base_id.as_deref(),
        owner_defs.as_ref(),
    )?;

    // Tagged-union verbs (`retrieve`, `forget`) carry multiple variants; clap
    // models the discriminator as an `ArgGroup` requiring exactly one.
    if cmds.len() > 1
        && let Some(verb_def) = verb_def
    {
        let matched = count_matching_variants(verb_def, &cmds, &used_field_names, positional_count);
        if matched != 1 {
            return Err(CompatError::AmbiguousVariant {
                verb: verb.to_string(),
                matched_variants: matched,
                line: source_line,
            });
        }
    } else if cmds.len() == 1
        && let Some(verb_def) = verb_def
        && let Some(cmd) = cmds.first()
    {
        validate_single_shape_required(
            verb,
            verb_def,
            cmd,
            &used_field_names,
            positional_count,
            source_line,
        )?;
    }
    Ok(())
}

/// Consume the leading two tokens of a `cairn …` line, enforcing the
/// executable name is exactly `cairn` and a verb token follows. Defense in
/// depth alongside the block-level filter in `validate_cli_block`.
fn consume_executable_and_verb<'a, I: Iterator<Item = &'a Token>>(
    tokens: &mut I,
    source_line: usize,
) -> Result<&'a str, CompatError> {
    match tokens.next() {
        Some(t) if !t.quoted && t.value == "cairn" => {}
        Some(t) => {
            return Err(CompatError::Malformed {
                kind: "cli",
                detail: format!("first token must be `cairn`, got `{}`", t.value),
                line: source_line,
            });
        }
        None => {
            return Err(CompatError::Malformed {
                kind: "cli",
                detail: "empty CLI invocation".to_string(),
                line: source_line,
            });
        }
    }
    tokens
        .next()
        .map(|t| t.value.as_str())
        .ok_or(CompatError::Malformed {
            kind: "cli",
            detail: "missing verb after `cairn`".to_string(),
            line: source_line,
        })
}

/// Validate every positional + flag value collected from one CLI line
/// against the matched variant's property schemas. Splits out the
/// per-occurrence loop so `validate_cli_line` stays under the workspace's
/// 100-line lint cap.
#[allow(clippy::too_many_arguments)]
fn apply_value_validation(
    verb: &str,
    source_line: usize,
    cmds: &[&CliCommand],
    allowed_flags: &BTreeMap<&str, &CliFlag>,
    cmd_props: &BTreeMap<String, serde_json::Value>,
    positional_values: &[String],
    flag_occurrences: &[FlagOccurrence],
    list_flag_values: &BTreeMap<String, Vec<String>>,
    base_id: Option<&str>,
    owner_defs: Option<&serde_json::Value>,
) -> Result<(), CompatError> {
    validate_positional_values(
        verb,
        cmds,
        cmd_props,
        positional_values,
        base_id,
        owner_defs,
        source_line,
    )?;
    for occ in flag_occurrences {
        let prop = cmd_props.get(&occ.field_name);
        validate_flag_value(
            &occ.long_name,
            &occ.value,
            &occ.value_source,
            prop,
            base_id,
            owner_defs,
            source_line,
        )?;
    }
    for (long_name, values) in list_flag_values {
        let Some(flag) = allowed_flags.get(long_name.as_str()) else {
            continue;
        };
        let prop = cmd_props.get(flag.name.as_str());
        let src = flag.value_source.as_str();
        if list_enum_options(src).is_some() {
            // Closed `list<enum(...)>` — the per-element membership +
            // uniqueItems / minItems checks live in
            // `validate_list_enum_value`. Reuse the existing path by
            // joining occurrences and routing through `validate_flag_value`.
            let combined = values.join(",");
            validate_flag_value(
                long_name,
                &combined,
                src,
                prop,
                base_id,
                owner_defs,
                source_line,
            )?;
        } else {
            // Generic `list<...>` (e.g., `list<string>`): build the array
            // from the raw occurrences and validate against the property
            // schema so `items.minLength`, `minItems`, `uniqueItems`, etc.
            // are all enforced. Round-10 finding 2.
            let bad = move |detail: String| CompatError::Malformed {
                kind: "cli",
                detail: format!("flag `--{long_name}`: {detail}"),
                line: source_line,
            };
            if let Some(prop) = prop {
                check_list_flag_value(values, prop, base_id, owner_defs, &bad)?;
            }
        }
    }
    Ok(())
}

/// Validate a generic `list<...>` flag's accumulated values as a JSON array
/// against the property schema (items, minItems, uniqueItems, etc.) using
/// the wrapped-schema + retriever path.
fn check_list_flag_value(
    values: &[String],
    prop_schema: &serde_json::Value,
    base_id: Option<&str>,
    owner_defs: Option<&serde_json::Value>,
    bad: &dyn Fn(String) -> CompatError,
) -> Result<(), CompatError> {
    let array = serde_json::Value::Array(
        values
            .iter()
            .map(|v| serde_json::Value::String(v.clone()))
            .collect(),
    );
    let wrapper = wrap_property_schema(prop_schema, base_id, owner_defs);
    let validator = jsonschema::draft202012::options()
        .with_retriever(SchemaDirRetriever)
        .build(&wrapper)
        .map_err(|e| bad(format!("schema compile: {e}")))?;
    if let Err(err) = validator.validate(&array) {
        return Err(bad(format!("array {array} violates schema: {err}")));
    }
    Ok(())
}

/// Single matched variant index for tagged-union verbs, or `None` when zero
/// or multiple variants match. Mirrors `count_matching_variants` so both
/// agree on which examples are unambiguous.
fn find_matching_variant_index(
    verb_def: &VerbDef,
    cmds: &[&CliCommand],
    used_field_names: &BTreeSet<String>,
    positional_count: usize,
) -> Option<usize> {
    let matched: Vec<usize> =
        matching_variant_indices(verb_def, cmds, used_field_names, positional_count);
    if matched.len() == 1 {
        Some(matched[0])
    } else {
        None
    }
}

/// Single-shape verb: enforce schema-required fields, anyOf (≥1), and
/// oneOf (==1) directly so an example missing a required arg or violating
/// exclusivity trips the gate just like the CLI would.
fn validate_single_shape_required(
    verb: &str,
    verb_def: &VerbDef,
    cmd: &CliCommand,
    used_field_names: &BTreeSet<String>,
    positional_count: usize,
    source_line: usize,
) -> Result<(), CompatError> {
    let spec = single_required_spec(verb_def);
    let positional_used = positional_count > 0;
    let positional_name: Option<&str> = cmd.positional.as_ref().map(|p| p.name.as_str());
    let positional_aliases: BTreeSet<&str> = cmd
        .positional
        .as_ref()
        .map(|p| p.aliases_one_of.iter().map(String::as_str).collect())
        .unwrap_or_default();

    // XOR: a positional with `aliases_one_of` (e.g., ingest's `source` →
    // body|file|url) cannot coexist with any of the flags it aliases — the
    // real CLI rejects that combination. Catch it here so SKILL.md can't
    // ship an example like `cairn ingest foo --body bar`.
    if positional_used
        && let Some(conflict) = positional_aliases
            .iter()
            .find(|a| used_field_names.contains(**a))
    {
        return Err(CompatError::Malformed {
            kind: "cli",
            detail: format!(
                "verb `{verb}` positional `{}` conflicts with aliased flag `--{conflict}`",
                positional_name.unwrap_or("?")
            ),
            line: source_line,
        });
    }

    let satisfied = |field: &String| {
        used_field_names.contains(field)
            || (positional_used && positional_name == Some(field.as_str()))
    };
    for field in &spec.base {
        if !satisfied(field) {
            return Err(CompatError::Malformed {
                kind: "cli",
                detail: format!("verb `{verb}` missing required field `{field}`"),
                line: source_line,
            });
        }
    }
    if !spec.any_of.is_empty()
        && !spec
            .any_of
            .iter()
            .any(|branch| branch.iter().all(satisfied))
    {
        return Err(CompatError::Malformed {
            kind: "cli",
            detail: format!("verb `{verb}` missing required field from anyOf branches"),
            line: source_line,
        });
    }
    if !spec.one_of.is_empty() {
        // Branches matched directly by used flags (and a same-named positional).
        let mut matched = spec
            .one_of
            .iter()
            .filter(|branch| branch.iter().all(satisfied))
            .count();
        // Positional aliasing: a single positional value collapses to exactly
        // one runtime branch (CLI dispatches body/file/url at runtime), so add
        // one to the count when the positional aliases any oneOf branch. The
        // XOR guard above already ensures positional and aliased flags don't
        // coexist, so this can't double-count.
        let positional_aliases_branch = positional_used
            && spec.one_of.iter().any(|branch| {
                branch
                    .iter()
                    .any(|f| positional_aliases.contains(f.as_str()))
            });
        if positional_aliases_branch {
            matched += 1;
        }
        if matched != 1 {
            return Err(CompatError::Malformed {
                kind: "cli",
                detail: format!(
                    "verb `{verb}` must satisfy exactly one oneOf branch (matched {matched})"
                ),
                line: source_line,
            });
        }
    }
    Ok(())
}

/// Pull the single-shape required spec for a verb whose `Args` lives directly
/// at `$defs/Args` (no `oneOf`).
fn single_required_spec(verb_def: &VerbDef) -> VariantSpec {
    let Ok(schema) = serde_json::from_slice::<serde_json::Value>(&verb_def.args_schema_bytes)
    else {
        return VariantSpec::default();
    };
    let Some(args) = schema.get("$defs").and_then(|d| d.get("Args")) else {
        return VariantSpec::default();
    };
    VariantSpec {
        base: required_excluding_const(args),
        any_of: any_of_required_branches(args),
        one_of: one_of_required_branches(args),
    }
}

/// Validate every positional value collected from one CLI line against the
/// owning command's positional schema. ALL-CAPS placeholders bypass — the
/// user is meant to substitute their own value. Pick the first command that
/// declares a positional: tagged-union verbs share the field name across
/// variants (e.g., `id` for `retrieve.record`), so any one is sufficient.
fn validate_positional_values(
    verb: &str,
    cmds: &[&CliCommand],
    prop_schemas: &BTreeMap<String, serde_json::Value>,
    positional_values: &[String],
    base_id: Option<&str>,
    owner_defs: Option<&serde_json::Value>,
    source_line: usize,
) -> Result<(), CompatError> {
    if positional_values.is_empty() {
        return Ok(());
    }
    let Some(pos_field) = cmds.iter().find_map(|c| c.positional.as_ref()) else {
        return Ok(());
    };
    let Some(prop) = prop_schemas.get(&pos_field.name) else {
        return Ok(());
    };
    for value in positional_values {
        validate_positional_value(
            verb,
            &pos_field.name,
            value,
            prop,
            base_id,
            owner_defs,
            source_line,
        )?;
    }
    Ok(())
}

/// Validate a positional argument value against its (per-item) property
/// schema. Skips `ALL_CAPS` placeholders. For array-typed positionals (e.g.,
/// `summarize`'s `record_ids`) the per-item schema (`items`) is consulted.
///
/// Goes through the full JSON Schema validator with the owning verb's `$id`
/// and `$defs` wrapped in so that constraints like `minLength`, `enum`,
/// numeric bounds, structural requirements, and any `$ref` (cross-file or
/// internal) are all enforced — not just `pattern`. Without this, a
/// schema-invalid example like `cairn search ""` would slip past the gate.
fn validate_positional_value(
    verb: &str,
    field: &str,
    value: &str,
    prop_schema: &serde_json::Value,
    base_id: Option<&str>,
    owner_defs: Option<&serde_json::Value>,
    line: usize,
) -> Result<(), CompatError> {
    if is_placeholder_for_schema(value, Some(prop_schema)) {
        return Ok(());
    }
    let item_schema =
        if prop_schema.get("type").and_then(serde_json::Value::as_str) == Some("array") {
            prop_schema.get("items").unwrap_or(prop_schema)
        } else {
            prop_schema
        };
    let wrapper = wrap_property_schema(item_schema, base_id, owner_defs);
    let validator = jsonschema::draft202012::options()
        .with_retriever(SchemaDirRetriever)
        .build(&wrapper)
        .map_err(|e| CompatError::Malformed {
            kind: "cli",
            detail: format!("verb `{verb}` positional `{field}` schema compile: {e}"),
            line,
        })?;
    // CLI positional values arrive as strings; validate them as JSON strings.
    let json_value = serde_json::Value::String(value.to_string());
    if let Err(err) = validator.validate(&json_value) {
        return Err(CompatError::Malformed {
            kind: "cli",
            detail: format!(
                "verb `{verb}` positional `{field}` value `{value}` violates schema: {err}"
            ),
            line,
        });
    }
    Ok(())
}

/// `referencing::Retrieve` impl that resolves cross-file `$ref`s by mapping
/// the `cairn.dev/schema/cairn.mcp.v1/...` URI back to a path under the
/// crate's compile-time `SCHEMA_DIR`. Without this any SKILL.md JSON example
/// that touches a primitive (`Ulid`, `Identity`, …) would fail to compile.
#[derive(Debug)]
struct SchemaDirRetriever;

impl referencing::Retrieve for SchemaDirRetriever {
    fn retrieve(
        &self,
        uri: &referencing::Uri<String>,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
        // Verb files declare `$id: https://cairn.dev/schema/cairn.mcp.v1/<rel>`.
        // Strip the well-known prefix and read the matching file from the
        // crate's schema bundle.
        const PREFIX: &str = "https://cairn.dev/schema/cairn.mcp.v1/";
        let raw = uri.as_str();
        let rel = raw.strip_prefix(PREFIX).ok_or_else(|| {
            format!(
                "skill-compat retriever: unexpected URI `{raw}` (only `{PREFIX}*` is supported)"
            )
        })?;
        // Reject path-traversal escapes BEFORE filesystem touch: a `$ref`
        // containing `..`, an absolute component, or a Windows-style drive
        // could otherwise read arbitrary local files during codegen
        // (round-10 finding 1). Canonicalize and verify the result still
        // sits under SCHEMA_DIR as defence in depth.
        let rel_path = std::path::Path::new(rel);
        for component in rel_path.components() {
            use std::path::Component;
            match component {
                Component::Normal(_) => {}
                _ => {
                    return Err(format!(
                        "skill-compat retriever: rejecting non-normal path component in `{rel}`"
                    )
                    .into());
                }
            }
        }
        let root = std::path::Path::new(crate::SCHEMA_DIR);
        let joined = root.join(rel_path);
        let canon_root = root
            .canonicalize()
            .map_err(|e| format!("skill-compat retriever: canonicalize SCHEMA_DIR: {e}"))?;
        let canon_target = joined.canonicalize().map_err(|e| {
            format!(
                "skill-compat retriever: canonicalize `{}`: {e}",
                joined.display()
            )
        })?;
        if !canon_target.starts_with(&canon_root) {
            return Err(format!(
                "skill-compat retriever: `{}` resolves outside schema root",
                canon_target.display()
            )
            .into());
        }
        let bytes = std::fs::read(&canon_target).map_err(|e| {
            format!(
                "skill-compat retriever: read `{}`: {e}",
                canon_target.display()
            )
        })?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| {
            format!(
                "skill-compat retriever: parse `{}`: {e}",
                canon_target.display()
            )
        })?;
        Ok(value)
    }
}

/// Token-scan result for one CLI line. Value validation is deferred to
/// the caller so the matched-variant property schemas can be applied.
struct TokenScan {
    positional_count: usize,
    used_field_names: BTreeSet<String>,
    positional_values: Vec<String>,
    flag_occurrences: Vec<FlagOccurrence>,
    /// Per-flag aggregated values for `list<enum(...)>` flags (clap's
    /// `ArgAction::Append`). Other `list<...>` sources keep per-occurrence
    /// semantics and live in `flag_occurrences`.
    list_flag_values: BTreeMap<String, Vec<String>>,
}

/// One `--long [value]` occurrence picked up by `scan_tokens`. Stored
/// pre-validation so the variant matcher can pick the right schema first.
struct FlagOccurrence {
    long_name: String,
    field_name: String,
    value_source: String,
    value: String,
}

/// True when `value` is an `ALL_CAPS_PLACEHOLDER` token (the convention
/// `emit_skill` uses for generated examples). Requires at least one uppercase
/// letter so a digit-only string (`"999"`) doesn't accidentally bypass
/// integer-bounds validation.
fn is_placeholder(value: &str) -> bool {
    let mut has_upper = false;
    for c in value.chars() {
        if c.is_ascii_uppercase() {
            has_upper = true;
        } else if !(c.is_ascii_digit() || c == '_') {
            return false;
        }
    }
    has_upper
}

/// True when `value` looks like an `ALL_CAPS_PLACEHOLDER` *and* the field
/// schema would actually accept it. Round-4 finding: the prior bypass let
/// constrained string/path fields like `format: "uri"` (`ingest.url`) and
/// `$ref: Cursor` ship with ALL-CAPS placeholders that wouldn't satisfy the
/// schema at runtime. By gating the bypass on "unconstrained string", any
/// constrained field forces `emit_skill` to provide a concrete `cli_exemplar`.
fn is_placeholder_for_schema(value: &str, prop_schema: Option<&serde_json::Value>) -> bool {
    if !is_placeholder(value) {
        return false;
    }
    let Some(prop) = prop_schema else {
        return true;
    };
    // For array-typed fields, dispatch on the items schema.
    let target = if prop.get("type").and_then(serde_json::Value::as_str) == Some("array") {
        prop.get("items").unwrap_or(prop)
    } else {
        prop
    };
    // Any of these makes the placeholder unsafe to bypass:
    if target.get("format").is_some()
        || target.get("pattern").is_some()
        || target.get("enum").is_some()
        || target.get("$ref").is_some()
        || target.get("const").is_some()
    {
        return false;
    }
    if let Some(max_len) = target.get("maxLength").and_then(serde_json::Value::as_u64)
        && (value.chars().count() as u64) > max_len
    {
        return false;
    }
    true
}

/// Validate `value` against the IDL `value_source` declared on a flag, plus
/// numeric bounds (`minimum` / `maximum`) lifted from the property schema
/// when supplied. Closed forms (`enum(...)`, `u*`, `i*`, `integer`, `bool`)
/// are checked strictly; freeform sources (`string`, `path`, `json`,
/// `list<...>`) accept anything. ALL-CAPS placeholders bypass the check
/// because the generated skill examples use them and they aren't real values.
fn validate_flag_value(
    flag: &str,
    value: &str,
    source: &str,
    prop_schema: Option<&serde_json::Value>,
    base_id: Option<&str>,
    owner_defs: Option<&serde_json::Value>,
    line: usize,
) -> Result<(), CompatError> {
    if is_placeholder_for_schema(value, prop_schema) {
        return Ok(());
    }
    let bad = |detail: String| CompatError::Malformed {
        kind: "cli",
        detail: format!("flag `--{flag}`: {detail}"),
        line,
    };
    if let Some(rest) = source.strip_prefix("enum(")
        && let Some(inner) = rest.strip_suffix(')')
    {
        let allowed: BTreeSet<&str> = inner.split(',').map(str::trim).collect();
        if !allowed.contains(value) {
            return Err(bad(format!("value `{value}` is not in {{{inner}}}")));
        }
    } else if is_unsigned_int_source(source) {
        let parsed = value
            .parse::<u64>()
            .map_err(|_| bad(format!("value `{value}` is not a non-negative integer")))?;
        check_integer_bounds(flag, i128::from(parsed), prop_schema, line)?;
    } else if is_signed_int_source(source) {
        let parsed = value
            .parse::<i64>()
            .map_err(|_| bad(format!("value `{value}` is not an integer")))?;
        check_integer_bounds(flag, i128::from(parsed), prop_schema, line)?;
    } else if let Some(allowed) = list_enum_options(source) {
        validate_list_enum_value(value, allowed, prop_schema, &bad)?;
    } else if source == "string" || source == "path" {
        // Freeform string/path: validate against the full property schema
        // (minLength, maxLength, pattern, format, enum, $ref, …) so an
        // example like `cairn ingest --url not-a-uri` fails the gate the
        // way the runtime would. Round-6 found the prior pattern-only
        // path missed `format: "uri"` and `maxLength` constraints.
        if let Some(prop) = prop_schema {
            check_string_flag_value(value, prop, base_id, owner_defs, &bad)?;
        }
    } else if source == "json"
        && let Some(prop) = prop_schema
    {
        // JSON-typed flag (e.g., `forget.scope`): parse the literal and
        // validate it against the full property schema with our
        // cross-file retriever wired in. A non-JSON value or one that
        // misses required narrowing fails compat the way the runtime
        // would.
        check_json_flag_value(value, prop, base_id, owner_defs, &bad)?;
    }
    Ok(())
}

/// Validate a string/path flag value via full JSON Schema validation —
/// wraps the property in the owning verb's `$id` + `$defs` so any
/// constraint (`minLength`, `maxLength`, `pattern`, `format`, `enum`,
/// `$ref`) is enforced. Mirrors the positional path so generated and
/// hand-written CLI examples are checked the same way.
fn check_string_flag_value(
    value: &str,
    prop_schema: &serde_json::Value,
    base_id: Option<&str>,
    owner_defs: Option<&serde_json::Value>,
    bad: &dyn Fn(String) -> CompatError,
) -> Result<(), CompatError> {
    let wrapper = wrap_property_schema(prop_schema, base_id, owner_defs);
    let validator = jsonschema::draft202012::options()
        .with_retriever(SchemaDirRetriever)
        .build(&wrapper)
        .map_err(|e| bad(format!("schema compile: {e}")))?;
    let json_value = serde_json::Value::String(value.to_string());
    if let Err(err) = validator.validate(&json_value) {
        return Err(bad(format!("value `{value}` violates schema: {err}")));
    }
    Ok(())
}

/// Validate a JSON-typed flag value: parse + validate against the full
/// property schema (with the cross-file retriever wired in so any `$ref`
/// resolves).
fn check_json_flag_value(
    value: &str,
    prop_schema: &serde_json::Value,
    base_id: Option<&str>,
    owner_defs: Option<&serde_json::Value>,
    bad: &dyn Fn(String) -> CompatError,
) -> Result<(), CompatError> {
    let parsed: serde_json::Value =
        serde_json::from_str(value).map_err(|e| bad(format!("not valid JSON: {e}")))?;
    // Wrap the property schema with the owning verb's `$id` and `$defs` so
    // both relative cross-file refs (`../common/...`) and internal refs
    // (`#/$defs/<Name>`) resolve. Dropping `$defs` here previously broke
    // valid examples like `cairn search --filters '{...}'` whose property
    // is `$ref: "#/$defs/filter"`.
    let schema = wrap_property_schema(prop_schema, base_id, owner_defs);
    let validator = jsonschema::draft202012::options()
        .with_retriever(SchemaDirRetriever)
        .build(&schema)
        .map_err(|e| bad(format!("schema compile: {e}")))?;
    if let Err(err) = validator.validate(&parsed) {
        return Err(bad(format!("schema mismatch: {err}")));
    }
    Ok(())
}

/// Validate a `list<enum(a,b,c)>` CLI value: reject empty items, enforce
/// membership, and honour `uniqueItems` / `minItems` from the property schema.
fn validate_list_enum_value(
    value: &str,
    allowed: &str,
    prop_schema: Option<&serde_json::Value>,
    bad: &dyn Fn(String) -> CompatError,
) -> Result<(), CompatError> {
    let allowed_set: BTreeSet<&str> = allowed.split(',').map(str::trim).collect();
    let unique_required = prop_schema
        .and_then(|p| p.get("uniqueItems"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut count = 0usize;
    for item in value.split(',') {
        let item = item.trim();
        if item.is_empty() {
            return Err(bad("empty list item".to_string()));
        }
        if !allowed_set.contains(item) {
            return Err(bad(format!("list item `{item}` is not in {{{allowed}}}")));
        }
        if unique_required && !seen.insert(item) {
            return Err(bad(format!("duplicate list item `{item}`")));
        }
        count += 1;
    }
    let min_items = prop_schema
        .and_then(|p| p.get("minItems"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let min_items = usize::try_from(min_items).unwrap_or(usize::MAX);
    if count < min_items {
        return Err(bad(format!(
            "list has {count} item(s), schema requires at least {min_items}"
        )));
    }
    Ok(())
}

/// Parse a `list<enum(a,b,c)>` value-source into the inner `a,b,c`. Returns
/// `None` for any other shape so the caller can fall through to the
/// freeform/unchecked path.
pub(crate) fn list_enum_options(source: &str) -> Option<&str> {
    source
        .strip_prefix("list<enum(")
        .and_then(|rest| rest.strip_suffix(")>"))
}

/// Enforce JSON-Schema `minimum` / `maximum` for an integer-typed flag value.
/// Bounds come from the verb's `properties.<field>` schema, so a stale skill
/// example with `--depth 999` for a `0..=16` field fails compat.
fn check_integer_bounds(
    flag: &str,
    value: i128,
    prop_schema: Option<&serde_json::Value>,
    line: usize,
) -> Result<(), CompatError> {
    let Some(prop) = prop_schema else {
        return Ok(());
    };
    let bad = |detail: String| CompatError::Malformed {
        kind: "cli",
        detail: format!("flag `--{flag}`: {detail}"),
        line,
    };
    if let Some(min) = prop.get("minimum").and_then(serde_json::Value::as_i64)
        && value < i128::from(min)
    {
        return Err(bad(format!("value {value} below minimum {min}")));
    }
    if let Some(max) = prop.get("maximum").and_then(serde_json::Value::as_i64)
        && value > i128::from(max)
    {
        return Err(bad(format!("value {value} above maximum {max}")));
    }
    Ok(())
}

/// Per-`CliCommand` property-schema maps. For single-shape verbs returns one
/// map (from `$defs/Args/properties`). For tagged-union verbs pairs each
/// `CliCommand` to its matched schema variant via `pair_cmd_to_variant` and
/// returns that variant's `properties` map. This lets value validation use
/// the *correct* schema per variant — the prior verb-wide flatten could
/// validate, e.g., a session `--cursor` value against the scope variant's
/// schema, which was a real false-negative path (round-7 finding).
pub(crate) fn variant_property_schemas(
    verb_def: &VerbDef,
    cmds: &[&CliCommand],
) -> Vec<BTreeMap<String, serde_json::Value>> {
    let empty = || (0..cmds.len()).map(|_| BTreeMap::new()).collect();
    let Ok(schema) = serde_json::from_slice::<serde_json::Value>(&verb_def.args_schema_bytes)
    else {
        return empty();
    };
    let Some(defs) = schema.get("$defs").and_then(serde_json::Value::as_object) else {
        return empty();
    };
    let Some(args) = defs.get("Args") else {
        return empty();
    };
    if cmds.len() == 1 {
        return vec![extract_properties(args)];
    }
    let Some(one_of) = args.get("oneOf").and_then(serde_json::Value::as_array) else {
        return empty();
    };
    let variant_schemas: Vec<&serde_json::Value> = one_of
        .iter()
        .filter_map(|entry| {
            entry
                .get("$ref")
                .and_then(serde_json::Value::as_str)
                .and_then(|p| p.strip_prefix("#/$defs/"))
                .and_then(|name| defs.get(name))
        })
        .collect();
    cmds.iter()
        .map(|cmd| {
            pair_cmd_to_variant(cmd, &variant_schemas)
                .map(extract_properties)
                .unwrap_or_default()
        })
        .collect()
}

fn extract_properties(schema: &serde_json::Value) -> BTreeMap<String, serde_json::Value> {
    schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .map(|m| {
            m.iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default()
}

/// Build a flat `flag-name → property-schema` map by unioning every
/// `$defs/*/properties` block in the verb's args schema. For tagged-union
/// verbs (e.g., `retrieve`) the same logical field can appear under several
/// variants — first definition wins, since downstream we only need the type
/// shape and bounds, which are identical across variants by convention.
pub(crate) fn collect_arg_property_schemas(
    verb_def: &VerbDef,
) -> BTreeMap<String, serde_json::Value> {
    let mut out = BTreeMap::new();
    let Ok(schema) = serde_json::from_slice::<serde_json::Value>(&verb_def.args_schema_bytes)
    else {
        return out;
    };
    let Some(defs) = schema.get("$defs").and_then(serde_json::Value::as_object) else {
        return out;
    };
    for (name, def) in defs {
        // Only walk arg-side defs (`Args` itself, and tagged-union variants
        // named `Args*` like `ArgsFolder`). Response defs live alongside
        // (`Data`, `Hit`, `FolderItem`…) and would shadow arg properties of
        // the same name with response-side bounds.
        if !(name == "Args" || name.starts_with("Args")) {
            continue;
        }
        if let Some(props) = def.get("properties").and_then(serde_json::Value::as_object) {
            for (k, v) in props {
                out.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
    }
    out
}

pub(crate) fn is_unsigned_int_source(s: &str) -> bool {
    matches!(s, "u8" | "u16" | "u32" | "u64" | "usize")
}

pub(crate) fn is_signed_int_source(s: &str) -> bool {
    matches!(s, "i8" | "i16" | "i32" | "i64" | "isize" | "integer")
}

/// One shell-split token plus whether *any* portion of it appeared inside
/// quotes. Knowing the quote origin lets the parser distinguish a real
/// `--option` token from a quoted argument that *starts* with `-` (e.g.,
/// `--body "--literal"` — round-3 finding).
#[derive(Debug, Clone)]
pub(crate) struct Token {
    pub value: String,
    pub quoted: bool,
}

/// Shell-aware tokenizer for one CLI example line. Handles single- and
/// double-quoted strings and backslash-escaped characters; otherwise behaves
/// like `split_whitespace`. Returns an error on an unmatched quote or a
/// dangling backslash so a syntactically broken example like
/// `cairn search "unterminated` can't slip past the gate.
fn shell_split(line: &str, source_line: usize) -> Result<Vec<Token>, CompatError> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut buf_quoted = false;
    let mut in_single = false;
    let mut in_double = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if !in_single => {
                let Some(next) = chars.next() else {
                    return Err(CompatError::Malformed {
                        kind: "cli",
                        detail: "trailing backslash with nothing to escape".to_string(),
                        line: source_line,
                    });
                };
                buf.push(next);
            }
            '\'' if !in_double => {
                in_single = !in_single;
                buf_quoted = true;
            }
            '"' if !in_single => {
                in_double = !in_double;
                buf_quoted = true;
            }
            ws if !in_single && !in_double && ws.is_whitespace() => {
                // Empty *quoted* tokens are real arguments (`''` is a
                // zero-length value); empty *unquoted* runs of whitespace
                // are not.
                if !buf.is_empty() || buf_quoted {
                    out.push(Token {
                        value: std::mem::take(&mut buf),
                        quoted: std::mem::take(&mut buf_quoted),
                    });
                }
            }
            _ => buf.push(c),
        }
    }
    if in_single || in_double {
        return Err(CompatError::Malformed {
            kind: "cli",
            detail: format!(
                "unterminated {} quote",
                if in_single { "single" } else { "double" }
            ),
            line: source_line,
        });
    }
    if !buf.is_empty() || buf_quoted {
        out.push(Token {
            value: buf,
            quoted: buf_quoted,
        });
    }
    Ok(out)
}

/// Walk the post-verb tokens, returning a [`TokenScan`] with the positional
/// count, the set of IDL field names the example referenced via long-flags,
/// and per-flag occurrences (deferred so the caller can validate against the
/// matched variant's schema). Rejects unknown `--flag` tokens up-front.
// Splitting the long-flag arm into a separate helper would force the caller
// to thread `iter`, `used_field_names`, `flag_occurrences`, and
// `list_flag_values` as out-params — net more code. Local allow.
#[allow(clippy::too_many_lines)]
fn scan_tokens<'a>(
    verb: &str,
    source_line: usize,
    tokens: impl Iterator<Item = &'a Token>,
    allowed_flags: &BTreeMap<&str, &CliFlag>,
) -> Result<TokenScan, CompatError> {
    let mut positional_count = 0usize;
    let mut used_field_names: BTreeSet<String> = BTreeSet::new();
    let mut positional_values: Vec<String> = Vec::new();
    let mut flag_occurrences: Vec<FlagOccurrence> = Vec::new();
    let mut list_flag_values: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut iter = tokens.peekable();
    while let Some(tok) = iter.next() {
        // A *quoted* token is always argument data — never treat it as a
        // `--option`. Round-3 finding: `--body "--literal"` lost its
        // quoting context and the literal value was reparsed as a flag.
        let looks_like_long = !tok.quoted && tok.value.starts_with("--") && tok.value.len() > 2;
        let looks_like_short = !tok.quoted
            && tok.value.starts_with('-')
            && tok.value != "-"
            && !tok.value.starts_with("--");
        if looks_like_long {
            let flag_body = tok.value.strip_prefix("--").unwrap_or("");
            let (name, has_inline_value) = flag_body
                .split_once('=')
                .map_or((flag_body, false), |(n, _)| (n, true));
            if name.is_empty() {
                continue;
            }
            let (arity, value_source, field_name) = if UNIVERSAL_FLAGS.contains(&name) {
                (0usize, None, None)
            } else if let Some(flag) = allowed_flags.get(name) {
                used_field_names.insert(flag.name.clone());
                let arity = usize::from(flag.value_source != "bool");
                (
                    arity,
                    Some(flag.value_source.as_str()),
                    Some(flag.name.as_str()),
                )
            } else {
                return Err(CompatError::UnknownFlag {
                    verb: verb.to_string(),
                    flag: name.to_string(),
                    line: source_line,
                });
            };
            // Round-6 finding 1: boolean flags (`ArgAction::SetTrue`) reject
            // inline `=value` syntax at runtime — clap surfaces it as an
            // error. The compat gate must do the same so a stale example
            // like `--rehydrate=false` or `--json=no` cannot ship.
            if arity == 0 && has_inline_value {
                return Err(CompatError::Malformed {
                    kind: "cli",
                    detail: format!("boolean flag `--{name}` does not accept an inline value"),
                    line: source_line,
                });
            }
            let value: Option<String> = if arity == 1 && !has_inline_value {
                // Non-boolean flags require a value — either inline (`--x=v`)
                // or the next token. A quoted token is always a value
                // (even if it starts with `-`); an unquoted token is a
                // value only if it isn't another option.
                let v = match iter.peek() {
                    Some(n)
                        if n.quoted
                            || !n.value.starts_with('-')
                            || n.value == "-"
                            || n.value == "--" =>
                    {
                        let v = n.value.clone();
                        let _ = iter.next();
                        Some(v)
                    }
                    _ => None,
                };
                let Some(v) = v else {
                    return Err(CompatError::Malformed {
                        kind: "cli",
                        detail: format!("flag `--{name}` requires a value"),
                        line: source_line,
                    });
                };
                Some(v)
            } else if arity == 1 {
                flag_body.split_once('=').map(|(_, v)| v.to_string())
            } else {
                None
            };
            if let (Some(value), Some(src), Some(field)) = (value, value_source, field_name) {
                // Aggregate every `list<...>` source so the array-as-a-
                // whole is validated against the property schema.
                if src.starts_with("list<") {
                    list_flag_values
                        .entry(name.to_string())
                        .or_default()
                        .push(value);
                } else {
                    flag_occurrences.push(FlagOccurrence {
                        long_name: name.to_string(),
                        field_name: field.to_string(),
                        value_source: src.to_string(),
                        value,
                    });
                }
            }
        } else if looks_like_short {
            // Single-dash token (e.g., `-x`). We don't model short
            // options; surface as unknown so a stale alias blocks compat.
            return Err(CompatError::UnknownFlag {
                verb: verb.to_string(),
                flag: tok.value.trim_start_matches('-').to_string(),
                line: source_line,
            });
        } else {
            // Quoted token, bare `-`, or anything not starting with `-`
            // — treat as positional.
            positional_count += 1;
            positional_values.push(tok.value.clone());
        }
    }
    Ok(TokenScan {
        positional_count,
        used_field_names,
        positional_values,
        flag_occurrences,
        list_flag_values,
    })
}

/// Count how many variants of a tagged-union verb the example "selects".
///
/// A variant is selected when:
/// - every base required field (post const-strip + discriminator-flag) is
///   satisfied (long-flag or positional), AND
/// - if `anyOf` branches exist, *at least one* branch is fully satisfied
///   (disjunctive: `ArgsProfile` accepts either `--user` or `--agent`), AND
/// - every flag the example used belongs to this variant's own flag set
///   (so a strict superset like `--session --turn` selects only `ArgsTurn`,
///   not also `ArgsSession`), AND
/// - the example's positional count fits this variant's positional capacity.
fn count_matching_variants(
    verb_def: &VerbDef,
    cmds: &[&CliCommand],
    used_field_names: &BTreeSet<String>,
    positional_count: usize,
) -> usize {
    matching_variant_indices(verb_def, cmds, used_field_names, positional_count).len()
}

fn matching_variant_indices(
    verb_def: &VerbDef,
    cmds: &[&CliCommand],
    used_field_names: &BTreeSet<String>,
    positional_count: usize,
) -> Vec<usize> {
    let specs = variant_required_specs(verb_def, cmds);
    cmds.iter()
        .enumerate()
        .filter(|(idx, cmd)| {
            let spec = &specs[*idx];
            if spec.base.is_empty() && spec.any_of.is_empty() && spec.one_of.is_empty() {
                return false;
            }
            let variant_flag_names: BTreeSet<&str> =
                cmd.flags.iter().map(|f| f.name.as_str()).collect();
            let positional_name: Option<&str> = cmd.positional.as_ref().map(|p| p.name.as_str());

            let satisfied = |field: &String| {
                used_field_names.contains(field)
                    || (positional_count > 0 && positional_name == Some(field.as_str()))
            };

            // 1. Base required fields all satisfied.
            if !spec.base.iter().all(satisfied) {
                return false;
            }
            // 2. If anyOf branches exist, at least one must be fully satisfied.
            if !spec.any_of.is_empty()
                && !spec
                    .any_of
                    .iter()
                    .any(|branch| branch.iter().all(satisfied))
            {
                return false;
            }
            // 2b. If oneOf branches exist, exactly one must be fully satisfied.
            if !spec.one_of.is_empty() {
                let matched = spec
                    .one_of
                    .iter()
                    .filter(|branch| branch.iter().all(satisfied))
                    .count();
                if matched != 1 {
                    return false;
                }
            }
            // 3. No foreign flags — every used flag belongs to this variant.
            if !used_field_names
                .iter()
                .all(|f| variant_flag_names.contains(f.as_str()))
            {
                return false;
            }
            // 4. Positional count must fit this variant.
            let positional_capacity = match &cmd.positional {
                Some(p) if p.repeatable => usize::MAX,
                Some(_) => 1,
                None => 0,
            };
            positional_count <= positional_capacity
        })
        .map(|(idx, _)| idx)
        .collect()
}

/// Per-variant required-field decomposition: `base` is the schema-required
/// set after stripping const discriminators (and lifting the discriminator
/// flag); `any_of` is the inclusive-or branches (≥1 satisfied); `one_of` is
/// the exclusive-or branches (exactly 1 satisfied — JSON Schema `oneOf`
/// semantics, e.g., `ingest`'s `body | file | url`).
#[derive(Debug, Default, Clone)]
pub(crate) struct VariantSpec {
    pub base: BTreeSet<String>,
    pub any_of: Vec<BTreeSet<String>>,
    pub one_of: Vec<BTreeSet<String>>,
}

/// Pull `required: [...]` from a JSON-Schema object, dropping any field whose
/// `properties.<field>` schema declares a `const` (i.e., the discriminator —
/// `target` for `retrieve`, `mode` for `forget`).
pub(crate) fn required_excluding_const(schema: &serde_json::Value) -> BTreeSet<String> {
    let Some(arr) = schema.get("required").and_then(serde_json::Value::as_array) else {
        return BTreeSet::new();
    };
    let props = schema
        .get("properties")
        .and_then(serde_json::Value::as_object);
    arr.iter()
        .filter_map(serde_json::Value::as_str)
        .filter(|name| {
            props
                .and_then(|p| p.get(*name))
                .and_then(|s| s.get("const"))
                .is_none()
        })
        .map(str::to_string)
        .collect()
}

/// Extract `anyOf` branches' `required` field sets (inclusive-or — ≥1 must
/// be satisfied). E.g., `ArgsProfile`'s user-or-agent constraint.
pub(crate) fn any_of_required_branches(schema: &serde_json::Value) -> Vec<BTreeSet<String>> {
    branch_required_sets(schema, "anyOf")
}

/// Extract inline `oneOf` branches' `required` field sets (exclusive-or —
/// exactly 1 must be satisfied). E.g., `ingest`'s `body | file | url`. Arms
/// that are `$ref`s are taxonomy-level dispatch, not exclusivity branches,
/// so they aren't returned here.
pub(crate) fn one_of_required_branches(schema: &serde_json::Value) -> Vec<BTreeSet<String>> {
    branch_required_sets(schema, "oneOf")
}

fn branch_required_sets(schema: &serde_json::Value, key: &str) -> Vec<BTreeSet<String>> {
    let Some(arr) = schema.get(key).and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for branch in arr {
        if branch.get("$ref").is_some() {
            continue;
        }
        if let Some(req) = branch.get("required").and_then(serde_json::Value::as_array) {
            out.push(
                req.iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect::<BTreeSet<String>>(),
            );
        }
    }
    out
}

/// Look up the const-discriminator value (e.g., `target: { "const": "profile" }`)
/// → `"profile"`. Returns `None` when no property carries a `const`.
pub(crate) fn discriminator_const(schema: &serde_json::Value) -> Option<String> {
    let props = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)?;
    for value in props.values() {
        if let Some(c) = value.get("const").and_then(serde_json::Value::as_str) {
            return Some(c.to_string());
        }
    }
    None
}

/// For a tagged-union verb, return per-variant `VariantSpec` (base + anyOf
/// branches) in `cmds` order. Discriminator fields (whose schema is a
/// `const`) are stripped from `base`, and the variant's discriminator flag
/// is lifted in when its `const` value matches a flag on this `CliCommand`
/// (e.g., `target: "profile"` → `--profile`).
pub(crate) fn variant_required_specs(verb_def: &VerbDef, cmds: &[&CliCommand]) -> Vec<VariantSpec> {
    let empty_specs = || (0..cmds.len()).map(|_| VariantSpec::default()).collect();
    let Ok(schema) = serde_json::from_slice::<serde_json::Value>(&verb_def.args_schema_bytes)
    else {
        return empty_specs();
    };
    let Some(defs) = schema.get("$defs").and_then(serde_json::Value::as_object) else {
        return empty_specs();
    };
    let Some(one_of) = defs
        .get("Args")
        .and_then(|a| a.get("oneOf"))
        .and_then(serde_json::Value::as_array)
    else {
        return empty_specs();
    };

    // Pair each CliCommand to its schema variant by *content* (matching the
    // CLI flag/positional name set against each variant's non-const property
    // names) rather than by array position — without this, a schema reorder
    // of `oneOf` would silently mispair specs to commands and let drift slip
    // through the gate.
    let variant_schemas: Vec<&serde_json::Value> = one_of
        .iter()
        .filter_map(|entry| {
            entry
                .get("$ref")
                .and_then(serde_json::Value::as_str)
                .and_then(|p| p.strip_prefix("#/$defs/"))
                .and_then(|name| defs.get(name))
        })
        .collect();

    cmds.iter()
        .map(|cmd| {
            let Some(variant_schema) = pair_cmd_to_variant(cmd, &variant_schemas) else {
                return VariantSpec::default();
            };
            let mut base = required_excluding_const(variant_schema);
            if let Some(disc) = discriminator_const(variant_schema)
                && let Some(flag) = cmd.flags.iter().find(|f| f.long == disc || f.name == disc)
            {
                base.insert(flag.name.clone());
            }
            let any_of = any_of_required_branches(variant_schema);
            let one_of = one_of_required_branches(variant_schema);
            VariantSpec {
                base,
                any_of,
                one_of,
            }
        })
        .collect()
}

/// Pick the schema variant whose non-const property names are a **subset**
/// of the `CliCommand`'s flag/positional name set. The CLI may carry extra
/// discriminator-only flags (e.g., `--profile` for `ArgsProfile`) that don't
/// appear as JSON properties, so strict equality is too tight. **Fails
/// closed** when zero or more than one variant qualifies — that surfaces a
/// schema/CLI drift as a missing spec → 0-variant match → `AmbiguousVariant`
/// rather than letting the validator silently bind the wrong variant.
fn pair_cmd_to_variant<'a>(
    cmd: &CliCommand,
    variants: &[&'a serde_json::Value],
) -> Option<&'a serde_json::Value> {
    let cmd_fields: BTreeSet<String> = cmd
        .flags
        .iter()
        .map(|f| f.name.clone())
        .chain(cmd.positional.as_ref().map(|p| p.name.clone()))
        .collect();
    let mut candidates: Vec<&'a serde_json::Value> = Vec::new();
    for &v in variants {
        let Some(props) = v.get("properties").and_then(serde_json::Value::as_object) else {
            continue;
        };
        let variant_fields: BTreeSet<String> = props
            .iter()
            .filter(|(_, ps)| ps.get("const").is_none())
            .map(|(k, _)| k.clone())
            .collect();
        if variant_fields.is_subset(&cmd_fields) {
            candidates.push(v);
        }
    }
    // Pick the most-specific (largest) variant among candidates so a CLI
    // surface like {session_id, limit, order, rehydrate, include, cursor}
    // pairs to ArgsSession (5 fields) instead of an unrelated subset.
    candidates.sort_by_key(|v| {
        std::cmp::Reverse(
            v.get("properties")
                .and_then(serde_json::Value::as_object)
                .map_or(0, |p| {
                    p.iter().filter(|(_, ps)| ps.get("const").is_none()).count()
                }),
        )
    });
    let mut iter = candidates.into_iter();
    let best = iter.next()?;
    // If a second candidate exists at the same field-count, ambiguous → fail.
    if let Some(next) = iter.next()
        && variant_field_count(best) == variant_field_count(next)
    {
        return None;
    }
    Some(best)
}

fn variant_field_count(v: &serde_json::Value) -> usize {
    v.get("properties")
        .and_then(serde_json::Value::as_object)
        .map_or(0, |p| {
            p.iter().filter(|(_, ps)| ps.get("const").is_none()).count()
        })
}

/// Walk `markdown`, returning each code block paired with the verb id from the
/// most recent `cairn <verb>` H2 heading (or `None` when the block sits
/// outside any verb section). Used by the codegen drift gate to validate JSON
/// payload examples against the right schema.
///
/// # Errors
/// Returns [`CompatError::Malformed`] when the source ends with an
/// unterminated fenced block.
pub fn extract_verb_scoped_blocks(
    markdown: &str,
) -> Result<Vec<(Option<String>, CodeBlock)>, CompatError> {
    let mut out = Vec::new();
    let mut current_verb: Option<String> = None;
    let mut in_fence = false;
    let mut fence_lang = String::new();
    let mut fence_body = String::new();
    let mut fence_open_line = 0usize;
    let mut fence_verb: Option<String> = None;

    for (idx, line) in markdown.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = line.trim_start();

        if !in_fence && let Some(rest) = trimmed.strip_prefix("## ") {
            current_verb = parse_verb_heading(rest);
        }

        if let Some(rest) = trimmed.strip_prefix("```") {
            if in_fence {
                out.push((
                    fence_verb.take(),
                    CodeBlock {
                        lang: std::mem::take(&mut fence_lang),
                        body: std::mem::take(&mut fence_body),
                        line: fence_open_line,
                    },
                ));
                in_fence = false;
            } else {
                in_fence = true;
                fence_lang = rest.trim().to_string();
                fence_body.clear();
                fence_open_line = line_no;
                fence_verb.clone_from(&current_verb);
            }
            continue;
        }
        if in_fence {
            if !fence_body.is_empty() {
                fence_body.push('\n');
            }
            fence_body.push_str(line);
        } else if !is_heading(trimmed) {
            for span in extract_inline_cairn_spans(line, line_no) {
                out.push((current_verb.clone(), span));
            }
        }
    }
    if in_fence {
        return Err(CompatError::Malformed {
            kind: "fence",
            detail: "unterminated fenced code block".to_string(),
            line: fence_open_line,
        });
    }
    Ok(out)
}

/// Parse a section heading of the form `cairn <verb>` (with or without
/// surrounding backticks) and return the verb id when present.
fn parse_verb_heading(heading: &str) -> Option<String> {
    let stripped = heading.trim().trim_matches('`');
    let rest = stripped.strip_prefix("cairn ")?;
    let verb = rest.split_whitespace().next()?;
    if verb.is_empty() {
        return None;
    }
    Some(verb.to_string())
}

/// Validate one JSON block against the input schema of `verb`.
///
/// `verb` must name a verb in `doc`; the block body is parsed as JSON and
/// validated against that verb's `args_schema_bytes`.
///
/// # Errors
/// Returns [`CompatError::SchemaMismatch`] on validation failure or
/// [`CompatError::Malformed`] on parse failure.
pub fn validate_json_block(
    block: &CodeBlock,
    doc: &Document,
    verb: &str,
) -> Result<(), CompatError> {
    let Some(verb_def) = doc.verbs.iter().find(|v| v.id == verb) else {
        return Err(CompatError::UnknownVerb {
            verb: verb.to_string(),
            line: block.line,
        });
    };
    let payload: serde_json::Value =
        serde_json::from_str(&block.body).map_err(|e| CompatError::Malformed {
            kind: "json",
            detail: e.to_string(),
            line: block.line,
        })?;
    let full_schema: serde_json::Value = serde_json::from_slice(&verb_def.args_schema_bytes)
        .map_err(|e| CompatError::Malformed {
            kind: "json",
            detail: format!("verb `{verb}` schema parse: {e}"),
            line: block.line,
        })?;
    // Wrap `$defs/Args` (the actual input shape) with the original `$defs`
    // bundle so intra-file `$ref`s still resolve. Validating against the raw
    // file root would let `{}` through because the top-level has no `required`.
    // Preserve the original `$id` so any cross-file `$ref` (e.g.,
    // `../common/primitives.json#/$defs/Ulid`) can be resolved by a retriever
    // relative to the verb file's URL.
    let defs = full_schema
        .get("$defs")
        .cloned()
        .unwrap_or(serde_json::json!({}));
    let mut schema = serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$defs": defs,
        "$ref": "#/$defs/Args",
    });
    if let Some(id) = full_schema.get("$id").cloned()
        && let Some(obj) = schema.as_object_mut()
    {
        obj.insert("$id".to_string(), id);
    }
    let validator = jsonschema::draft202012::options()
        .with_retriever(SchemaDirRetriever)
        .build(&schema)
        .map_err(|e| CompatError::SchemaMismatch {
            verb: verb.to_string(),
            detail: format!("schema compile: {e}"),
            line: block.line,
        })?;
    if let Err(err) = validator.validate(&payload) {
        return Err(CompatError::SchemaMismatch {
            verb: verb.to_string(),
            detail: err.to_string(),
            line: block.line,
        });
    }
    Ok(())
}

/// Look up the verb whose `CliCommand::command` matches `cmd_token`. Falls
/// back to `verb.id` for back-compat. Returns `None` when neither matches.
#[must_use]
pub fn verb_for_command<'a>(doc: &'a Document, cmd_token: &str) -> Option<&'a VerbDef> {
    doc.verbs
        .iter()
        .find(|v| match &v.cli {
            CliShape::Single(c) => c.command == cmd_token,
            CliShape::Variants(vs) => vs.iter().any(|c| c.command == cmd_token),
        })
        .or_else(|| doc.verbs.iter().find(|v| v.id == cmd_token))
}

/// Look up the `CliCommand` set for a given verb id, returning `None` for
/// unknown verbs. Tagged-union verbs (`retrieve`) expose multiple commands.
#[must_use]
pub fn cli_commands_for<'a>(doc: &'a Document, verb: &str) -> Option<Vec<&'a CliCommand>> {
    let v = doc.verbs.iter().find(|v| v.id == verb)?;
    Some(match &v.cli {
        CliShape::Single(cmd) => vec![cmd],
        CliShape::Variants(cmds) => cmds.iter().collect(),
    })
}
