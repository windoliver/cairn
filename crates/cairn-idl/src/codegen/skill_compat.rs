//! Compatibility checks for the generated Cairn skill (issue #70).
//!
//! Extracts CLI and JSON examples from `skills/cairn/SKILL.md` and validates
//! them against the IDL: every `cairn <verb>` invocation must reference a real
//! verb (or protocol prelude) and supply only known flags; every JSON block
//! must parse against the input schema of its declared verb.
//!
//! These checks run alongside drift detection so the skill cannot reference a
//! retired verb, an invented kind, or a flag that no longer exists.

use std::collections::BTreeMap;

use crate::codegen::ir::{CliCommand, CliFlag, CliShape, Document};

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
#[must_use]
pub fn extract_code_blocks(markdown: &str) -> Vec<CodeBlock> {
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
        } else {
            out.extend(extract_inline_cairn_spans(line, line_no));
        }
    }

    out
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
        let mut tokens = line.split_whitespace();
        let _ = tokens.next(); // "cairn"
        let Some(verb) = tokens.next() else {
            return Err(CompatError::Malformed {
                kind: "cli",
                detail: "missing verb after `cairn`".to_string(),
                line: block.line,
            });
        };

        // Build (long-name → flag) map and positional capacity. Preludes carry
        // no flags or positionals; verb variants merge the union of all
        // variant flag tables and accept a positional iff any variant does.
        let (allowed_flags, positional_capacity) = if PRELUDES.contains(&verb) {
            (BTreeMap::new(), 0usize)
        } else if let Some(cmds) = cli_commands_for(doc, verb) {
            let map: BTreeMap<&str, &CliFlag> = cmds
                .iter()
                .flat_map(|c| c.flags.iter().map(|f| (f.long.as_str(), f)))
                .collect();
            let cap = usize::from(cmds.iter().any(|c| c.positional.is_some()));
            (map, cap)
        } else {
            return Err(CompatError::UnknownVerb {
                verb: verb.to_string(),
                line: block.line,
            });
        };

        let mut positional_count = 0usize;
        let mut iter = tokens.peekable();
        while let Some(tok) = iter.next() {
            if let Some(flag_body) = tok.strip_prefix("--") {
                // `--name` or `--name=value`.
                let (name, has_inline_value) = flag_body
                    .split_once('=')
                    .map_or((flag_body, false), |(n, _)| (n, true));
                if name.is_empty() {
                    continue;
                }
                let arity = if UNIVERSAL_FLAGS.contains(&name) {
                    0
                } else if let Some(flag) = allowed_flags.get(name) {
                    usize::from(flag.value_source != "bool")
                } else {
                    return Err(CompatError::UnknownFlag {
                        verb: verb.to_string(),
                        flag: name.to_string(),
                        line: block.line,
                    });
                };
                // Consume the next token as the flag's value when the flag
                // takes one and didn't carry it inline. Without this the
                // value would be miscounted as a stray positional.
                if arity == 1
                    && !has_inline_value
                    && iter.peek().is_some_and(|n| !n.starts_with('-'))
                {
                    let _ = iter.next();
                }
            } else if tok.starts_with('-') {
                // Short flags (`-h`, `-j`) — skip without arity tracking.
            } else {
                positional_count += 1;
            }
        }

        if positional_count > positional_capacity {
            return Err(CompatError::Malformed {
                kind: "cli",
                detail: format!(
                    "verb `{verb}` accepts {positional_capacity} positional arg(s), got {positional_count}"
                ),
                line: block.line,
            });
        }
    }
    Ok(())
}

/// Walk `markdown`, returning each code block paired with the verb id from the
/// most recent `cairn <verb>` H2 heading (or `None` when the block sits
/// outside any verb section). Used by the codegen drift gate to validate JSON
/// payload examples against the right schema.
#[must_use]
pub fn extract_verb_scoped_blocks(markdown: &str) -> Vec<(Option<String>, CodeBlock)> {
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
        } else {
            for span in extract_inline_cairn_spans(line, line_no) {
                out.push((current_verb.clone(), span));
            }
        }
    }
    out
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
