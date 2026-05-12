//! Harness transcript replay parsers (issue #311).
//!
//! Pure, I/O-free parsers that map a single JSONL line into a normalized
//! [`ParsedTranscriptLine`] of [`crate::domain::trace::TraceBlock`] values.
//! Selection is either explicit (`--harness`) or inferred from the first
//! non-empty line via [`detect_parser`].

mod claude_code;
mod generic;
mod parser;

use serde_json::Value;

pub use self::claude_code::ClaudeCodeParser;
pub use self::generic::GenericParser;
pub use self::parser::{ParseError, ParsedTranscriptLine, TranscriptParser};

/// Resolve which parser to use for a transcript file.
///
/// `forced` wins when present. Otherwise the first non-empty line is sniffed
/// for the Claude Code shape (`message.role` string field); anything else
/// falls back to the generic parser.
///
/// # Errors
///
/// Returns [`ParseError::UnknownHarness`] when `forced` names a parser the
/// registry does not know.
pub fn detect_parser(
    first_line: &str,
    forced: Option<&str>,
) -> Result<Box<dyn TranscriptParser>, ParseError> {
    detect_parser_with(first_line, forced, None)
}

/// Same as [`detect_parser`] but threads a `session_id_from` selector
/// through to the [`GenericParser`] so multi-line generic transcripts
/// can collapse into one logical session.
///
/// # Errors
///
/// Returns [`ParseError::UnknownHarness`] or [`ParseError::Malformed`]
/// per the same contract as [`detect_parser`].
pub fn detect_parser_with(
    first_line: &str,
    forced: Option<&str>,
    session_id_from: Option<&str>,
) -> Result<Box<dyn TranscriptParser>, ParseError> {
    let make_generic = || GenericParser {
        session_id_from: session_id_from.map(str::to_owned),
    };
    match forced {
        Some("claude-code") => Ok(Box::new(ClaudeCodeParser)),
        Some("generic") => Ok(Box::new(make_generic())),
        Some(other) => Err(ParseError::UnknownHarness(other.to_owned())),
        None => {
            // A line that looks JSON-shaped (`{...}`) must parse and follow
            // a known transcript schema. Falling back to the generic text
            // parser on a syntax error would silently swallow corrupt
            // structured transcripts.
            let trimmed = first_line.trim_start();
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                let value = serde_json::from_str::<Value>(first_line).map_err(|e| {
                    ParseError::Malformed {
                        parser: "auto-detect",
                        line: 1,
                        msg: format!("first line is JSON-shaped but invalid: {e}"),
                    }
                })?;
                if value
                    .get("message")
                    .and_then(Value::as_object)
                    .and_then(|m| m.get("role"))
                    .and_then(Value::as_str)
                    .is_some()
                {
                    return Ok(Box::new(ClaudeCodeParser));
                }
                // Structured JSONL that does not match a known harness
                // shape falls through to the generic parser. The malformed
                // path is reserved for *invalid* JSON; valid-but-unknown
                // shapes are an explicit `GenericParser` use case.
                Ok(Box::new(make_generic()))
            } else {
                Ok(Box::new(make_generic()))
            }
        }
    }
}
