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
    // Property schemas (by IDL field name) for bound-checking flag values.
    // For tagged-union verbs we union across all variants — a flag like
    // `--limit` lives under each variant's `properties` so we just take the
    // first definition we see.
    let prop_schemas = verb_def
        .map(collect_arg_property_schemas)
        .unwrap_or_default();
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
    } = scan_tokens(verb, source_line, tokens, &allowed_flags, &prop_schemas)?;

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

/// Token-scan result for one CLI line.
struct TokenScan {
    positional_count: usize,
    used_field_names: BTreeSet<String>,
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
        // `list<enum(a,b,c)>` is the closed list-of-enum form (e.g.,
        // retrieve's `--include`). Split the CLI value on `,` and reject any
        // item not in the allow-set so a stale `--include nonsense` slips
        // exactly the way the real clap parser would.
        let allowed_set: BTreeSet<&str> = allowed.split(',').map(str::trim).collect();
        for item in value.split(',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            if !allowed_set.contains(item) {
                return Err(bad(format!("list item `{item}` is not in {{{allowed}}}")));
            }
        }
    }
    Ok(())
}

/// Parse a `list<enum(a,b,c)>` value-source into the inner `a,b,c`. Returns
/// `None` for any other shape so the caller can fall through to the
/// freeform/unchecked path.
fn list_enum_options(source: &str) -> Option<&str> {
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
    prop_schemas: &BTreeMap<String, serde_json::Value>,
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
            let prop = field_name.and_then(|f| prop_schemas.get(f));
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
                    validate_flag_value(name, value, src, prop, source_line)?;
                }
            } else if arity == 1
                && let Some(src) = value_source
                && let Some((_, value)) = flag_body.split_once('=')
            {
                validate_flag_value(name, value, src, prop, source_line)?;
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
        .count()
}

/// Per-variant required-field decomposition: `base` is the schema-required
/// set after stripping const discriminators (and lifting the discriminator
/// flag); `any_of` is the inclusive-or branches (≥1 satisfied); `one_of` is
/// the exclusive-or branches (exactly 1 satisfied — JSON Schema `oneOf`
/// semantics, e.g., `ingest`'s `body | file | url`).
#[derive(Debug, Default)]
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
