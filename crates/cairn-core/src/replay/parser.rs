//! Transcript parser contract (issue #311).
//!
//! Parsers consume one JSONL line of a harness transcript and emit a
//! [`ParsedTranscriptLine`] of structured [`TraceBlock`] values.

use thiserror::Error;

use crate::domain::trace::TraceBlock;

/// Normalized parser output for a single transcript line.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedTranscriptLine {
    /// Harness session id this line belongs to.
    pub session_id: String,
    /// Speaker role (`user`, `assistant`, or harness-specific).
    pub role: String,
    /// Structured content blocks decoded from the line.
    pub blocks: Vec<TraceBlock>,
    /// RFC3339 timestamp from the source transcript when present.
    pub timestamp: Option<String>,
}

/// Failure modes returned by a [`TranscriptParser`].
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParseError {
    /// Line could not be parsed by the selected parser.
    #[error("{parser}: line {line}: {msg}")]
    Malformed {
        /// Parser name (e.g. `claude-code`).
        parser: &'static str,
        /// 1-indexed line number within the source file.
        line: usize,
        /// Human-readable failure cause.
        msg: String,
    },
    /// `--harness` flag named a parser the registry does not know.
    #[error("unknown harness parser: {0}")]
    UnknownHarness(String),
}

/// A parser maps one JSONL line into structured trace blocks.
pub trait TranscriptParser: Send + Sync {
    /// Parser identifier (used in errors and diagnostics).
    fn name(&self) -> &'static str;

    /// Parse one transcript line.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError::Malformed`] when the line cannot be decoded.
    fn parse_line(&self, line: &str, line_no: usize) -> Result<ParsedTranscriptLine, ParseError>;

    /// Resolve the session id carried by the line without parsing blocks.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError::Malformed`] when the line cannot be inspected.
    fn session_id_for(&self, line: &str, line_no: usize) -> Result<String, ParseError>;
}
