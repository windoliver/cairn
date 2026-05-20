//! Generic transcript fallback parser. One [`TraceBlock::Text`] per line,
//! synthesizing a stable session id from the line digest when none is
//! present in the source.

use std::fmt::Write as _;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::domain::trace::TraceBlock;
use crate::replay::parser::{ParseError, ParsedTranscriptLine, TranscriptParser};

const NAME: &str = "generic";

/// One-block-per-line passthrough parser.
///
/// `session_id_from` (if set) names a top-level JSON key on each line
/// whose string value is used as the session id, so a multi-line generic
/// transcript collapses into one logical session instead of fragmenting
/// per line.
#[derive(Debug, Default)]
pub struct GenericParser {
    /// Optional top-level JSON key whose string value overrides the
    /// per-line synthetic session id.
    pub session_id_from: Option<String>,
}

impl GenericParser {
    fn resolve_session_id(&self, line: &str) -> String {
        if let Some(key) = self.session_id_from.as_deref()
            && let Ok(v) = serde_json::from_str::<Value>(line)
            && let Some(s) = v.get(key).and_then(Value::as_str)
            && !s.is_empty()
        {
            return s.to_owned();
        }
        Self::synth_session_id(line)
    }

    fn synth_session_id(line: &str) -> String {
        let mut h = Sha256::new();
        h.update(line.as_bytes());
        let digest = h.finalize();
        let mut hex = String::with_capacity(2 + 16);
        hex.push_str("generic-");
        for byte in digest.iter().take(8) {
            let _ = write!(hex, "{byte:02x}");
        }
        hex
    }
}

impl TranscriptParser for GenericParser {
    fn name(&self) -> &'static str {
        NAME
    }

    fn parse_line(&self, line: &str, _line_no: usize) -> Result<ParsedTranscriptLine, ParseError> {
        Ok(ParsedTranscriptLine {
            session_id: self.resolve_session_id(line),
            role: "unknown".to_owned(),
            blocks: vec![TraceBlock::Text {
                text: line.to_owned(),
            }],
            timestamp: None,
        })
    }

    fn session_id_for(&self, line: &str, _line_no: usize) -> Result<String, ParseError> {
        Ok(self.resolve_session_id(line))
    }
}
