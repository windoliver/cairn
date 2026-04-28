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

/// Recognised protocol preludes — emitted by `emit_skill` but not part of the
/// eight-verb IDL surface.
const PRELUDES: &[&str] = &["status", "handshake"];

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
    for raw_line in block.body.lines() {
        let line = raw_line.trim();
        if line.is_empty() || !line.starts_with("cairn") {
            continue;
        }
        validate_cli_line(line, block.line, doc)?;
    }
    Ok(())
}

/// Validate one whitespace-tokenised `cairn …` line.
fn validate_cli_line(line: &str, source_line: usize, doc: &Document) -> Result<(), CompatError> {
    let owned = shell_split(line);
    let mut tokens = owned.iter().map(String::as_str);
    let _ = tokens.next(); // "cairn"
    let Some(verb) = tokens.next() else {
        return Err(CompatError::Malformed {
            kind: "cli",
            detail: "missing verb after `cairn`".to_string(),
            line: source_line,
        });
    };

    // Resolve the token to its CliCommand variants. Match on the configured
    // `CliCommand::command` (so a future IDL rename that diverges from
    // `verb.id` flows through), with a fallback to `verb.id`. Preludes carry
    // no flags or positionals.
    let verb_def: Option<&VerbDef> = verb_for_command(doc, verb);
    let cmds: Vec<&CliCommand> = if PRELUDES.contains(&verb) {
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

    // Tagged-union verbs (`retrieve`, `forget`) carry multiple variants; clap
    // models the discriminator as an `ArgGroup` requiring exactly one.
    // Approximate that here by requiring exactly one variant whose
    // schema-required fields (other than `target`) are all satisfied by the
    // example. Catches both missing-discriminator (`cairn retrieve --limit 5`)
    // and multi-discriminator (`cairn retrieve abc --session s1`).
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
    }
    Ok(())
}

/// Token-scan result for one CLI line.
struct TokenScan {
    positional_count: usize,
    used_field_names: BTreeSet<String>,
}

/// True when `value` is an `ALL_CAPS_PLACEHOLDER` token (the convention
/// `emit_skill` uses for generated examples). We skip strict typing for these
/// — the placeholder isn't meant to be a real value.
fn is_placeholder(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Validate `value` against the IDL `value_source` declared on a flag. Only
/// the closed forms (`enum(...)`, `u32`, `u64`, `bool`) are checked
/// strictly; freeform sources (`string`, `path`, `json`, `list<...>`) accept
/// anything. ALL-CAPS placeholders bypass the check because the generated
/// skill examples use them and they aren't real values.
fn validate_flag_value(
    flag: &str,
    value: &str,
    source: &str,
    line: usize,
) -> Result<(), CompatError> {
    if is_placeholder(value) {
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
    } else if is_unsigned_int_source(source) && value.parse::<u64>().is_err() {
        return Err(bad(format!(
            "value `{value}` is not a non-negative integer"
        )));
    } else if is_signed_int_source(source) && value.parse::<i64>().is_err() {
        return Err(bad(format!("value `{value}` is not an integer")));
    }
    Ok(())
}

fn is_unsigned_int_source(s: &str) -> bool {
    matches!(s, "u8" | "u16" | "u32" | "u64" | "usize")
}

fn is_signed_int_source(s: &str) -> bool {
    matches!(s, "i8" | "i16" | "i32" | "i64" | "isize" | "integer")
}

/// Shell-aware tokenizer for one CLI example line. Handles single- and
/// double-quoted strings and backslash-escaped characters; otherwise behaves
/// like `split_whitespace`. Without this `cairn search "project status"`
/// would parse as two positionals instead of one.
fn shell_split(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if !in_single => {
                if let Some(next) = chars.next() {
                    buf.push(next);
                }
            }
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            ws if !in_single && !in_double && ws.is_whitespace() => {
                if !buf.is_empty() {
                    out.push(std::mem::take(&mut buf));
                }
            }
            _ => buf.push(c),
        }
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    out
}

/// Walk the post-verb tokens, returning the positional count and the set of
/// IDL field names the example referenced via long-flags. Rejects unknown
/// `--flag` tokens up-front.
fn scan_tokens<'a>(
    verb: &str,
    source_line: usize,
    tokens: impl Iterator<Item = &'a str>,
    allowed_flags: &BTreeMap<&str, &CliFlag>,
) -> Result<TokenScan, CompatError> {
    let mut positional_count = 0usize;
    let mut used_field_names: BTreeSet<String> = BTreeSet::new();
    let mut iter = tokens.peekable();
    while let Some(tok) = iter.next() {
        if let Some(flag_body) = tok.strip_prefix("--") {
            let (name, has_inline_value) = flag_body
                .split_once('=')
                .map_or((flag_body, false), |(n, _)| (n, true));
            if name.is_empty() {
                continue;
            }
            let (arity, value_source) = if UNIVERSAL_FLAGS.contains(&name) {
                (0usize, None)
            } else if let Some(flag) = allowed_flags.get(name) {
                used_field_names.insert(flag.name.clone());
                let arity = usize::from(flag.value_source != "bool");
                (arity, Some(flag.value_source.as_str()))
            } else {
                return Err(CompatError::UnknownFlag {
                    verb: verb.to_string(),
                    flag: name.to_string(),
                    line: source_line,
                });
            };
            if arity == 1 && !has_inline_value {
                // Non-boolean flags require a value — either inline (`--x=v`)
                // or the next non-flag token. Without one clap would reject
                // the example at runtime, so fail compat too.
                let value = match iter.peek() {
                    Some(n) if !n.starts_with('-') => {
                        let v = *n;
                        let _ = iter.next();
                        Some(v)
                    }
                    _ => None,
                };
                let Some(value) = value else {
                    return Err(CompatError::Malformed {
                        kind: "cli",
                        detail: format!("flag `--{name}` requires a value"),
                        line: source_line,
                    });
                };
                if let Some(src) = value_source {
                    validate_flag_value(name, value, src, source_line)?;
                }
            } else if arity == 1
                && let Some(src) = value_source
                && let Some((_, value)) = flag_body.split_once('=')
            {
                validate_flag_value(name, value, src, source_line)?;
            }
        } else if !tok.starts_with('-') {
            positional_count += 1;
        }
    }
    Ok(TokenScan {
        positional_count,
        used_field_names,
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
    let specs = variant_required_specs(verb_def, cmds);
    cmds.iter()
        .enumerate()
        .filter(|(idx, cmd)| {
            let spec = &specs[*idx];
            if spec.base.is_empty() && spec.any_of.is_empty() {
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
        .count()
}

/// Per-variant required-field decomposition: `base` is the schema-required
/// set after stripping const discriminators (and lifting the discriminator
/// flag); `any_of` is the optional set of disjunctive branches.
#[derive(Debug, Default)]
pub(crate) struct VariantSpec {
    pub base: BTreeSet<String>,
    pub any_of: Vec<BTreeSet<String>>,
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

/// Extract every `anyOf` branch's `required` field set from a variant schema.
/// For shapes like `ArgsProfile` (`required: ["target"]` plus
/// `anyOf: [{required: ["user"]}, {required: ["agent"]}]`), the branch
/// requirements are what actually distinguishes the variant on the CLI.
/// Returns an empty `Vec` when the schema has no `anyOf`.
pub(crate) fn any_of_required_branches(schema: &serde_json::Value) -> Vec<BTreeSet<String>> {
    let Some(arr) = schema.get("anyOf").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|branch| {
            branch
                .get("required")
                .and_then(serde_json::Value::as_array)
                .map(|r| {
                    r.iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_string)
                        .collect::<BTreeSet<String>>()
                })
        })
        .collect()
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

    let mut out = Vec::with_capacity(cmds.len());
    for (idx, entry) in one_of.iter().enumerate() {
        let Some(variant_schema) = entry
            .get("$ref")
            .and_then(serde_json::Value::as_str)
            .and_then(|p| p.strip_prefix("#/$defs/"))
            .and_then(|name| defs.get(name))
        else {
            out.push(VariantSpec::default());
            continue;
        };
        let mut base = required_excluding_const(variant_schema);
        // Lift the discriminator into base when its const value matches a CLI
        // flag on this variant — that's how shapes like ArgsProfile select
        // themselves on the command line (`--profile ...`).
        if let Some(disc) = discriminator_const(variant_schema)
            && let Some(cmd) = cmds.get(idx)
            && let Some(flag) = cmd.flags.iter().find(|f| f.long == disc || f.name == disc)
        {
            base.insert(flag.name.clone());
        }
        let any_of = any_of_required_branches(variant_schema);
        out.push(VariantSpec { base, any_of });
    }
    out.resize_with(cmds.len(), VariantSpec::default);
    out
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
    let defs = full_schema
        .get("$defs")
        .cloned()
        .unwrap_or(serde_json::json!({}));
    let schema = serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$defs": defs,
        "$ref": "#/$defs/Args",
    });
    let validator =
        jsonschema::draft202012::new(&schema).map_err(|e| CompatError::SchemaMismatch {
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
