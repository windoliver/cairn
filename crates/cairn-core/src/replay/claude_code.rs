//! Claude Code transcript parser (`~/.claude/projects/.../session.jsonl`).

use serde_json::Value;

use crate::domain::trace::TraceBlock;
use crate::replay::parser::{ParseError, ParsedTranscriptLine, TranscriptParser};

const NAME: &str = "claude-code";

/// Parser for Claude Code session JSONL transcripts.
#[derive(Debug, Default)]
pub struct ClaudeCodeParser;

impl ClaudeCodeParser {
    fn parse_value(line: &str, line_no: usize) -> Result<Value, ParseError> {
        serde_json::from_str(line).map_err(|e| ParseError::Malformed {
            parser: NAME,
            line: line_no,
            msg: format!("invalid JSON: {e}"),
        })
    }

    fn extract_session_id(value: &Value, line_no: usize) -> Result<String, ParseError> {
        value
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or(ParseError::Malformed {
                parser: NAME,
                line: line_no,
                msg: "missing sessionId".to_owned(),
            })
    }

    fn require_str<'a>(
        block: &'a Value,
        field: &'static str,
        line_no: usize,
    ) -> Result<&'a str, ParseError> {
        block
            .get(field)
            .and_then(Value::as_str)
            .ok_or(ParseError::Malformed {
                parser: NAME,
                line: line_no,
                msg: format!("content block missing required string field `{field}`"),
            })
    }

    fn map_block(block: &Value, line_no: usize) -> Result<TraceBlock, ParseError> {
        let kind =
            block
                .get("type")
                .and_then(Value::as_str)
                .ok_or_else(|| ParseError::Malformed {
                    parser: NAME,
                    line: line_no,
                    msg: "content block missing type".to_owned(),
                })?;
        match kind {
            "thinking" => {
                let text = Self::require_str(block, "thinking", line_no)?.to_owned();
                let signature = block
                    .get("signature")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                Ok(TraceBlock::Reasoning { text, signature })
            }
            "text" => Ok(TraceBlock::Text {
                text: Self::require_str(block, "text", line_no)?.to_owned(),
            }),
            "tool_use" => {
                let tool = Self::require_str(block, "name", line_no)?.to_owned();
                let id = Self::require_str(block, "id", line_no)?.to_owned();
                let input = block.get("input").cloned().ok_or(ParseError::Malformed {
                    parser: NAME,
                    line: line_no,
                    msg: "tool_use block missing required `input`".to_owned(),
                })?;
                Ok(TraceBlock::ToolUse { tool, input, id })
            }
            "tool_result" => {
                let tool_use_id = Self::require_str(block, "tool_use_id", line_no)?.to_owned();
                let content_value = block.get("content").ok_or(ParseError::Malformed {
                    parser: NAME,
                    line: line_no,
                    msg: "tool_result block missing required `content`".to_owned(),
                })?;
                let content = match content_value {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                let is_error = block
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                Ok(TraceBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                })
            }
            other => Err(ParseError::Malformed {
                parser: NAME,
                line: line_no,
                msg: format!("unknown block type: {other}"),
            }),
        }
    }
}

impl TranscriptParser for ClaudeCodeParser {
    fn name(&self) -> &'static str {
        NAME
    }

    fn parse_line(&self, line: &str, line_no: usize) -> Result<ParsedTranscriptLine, ParseError> {
        let value = Self::parse_value(line, line_no)?;
        let session_id = Self::extract_session_id(&value, line_no)?;
        let message = value.get("message").ok_or(ParseError::Malformed {
            parser: NAME,
            line: line_no,
            msg: "missing message".to_owned(),
        })?;
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .ok_or(ParseError::Malformed {
                parser: NAME,
                line: line_no,
                msg: "message missing required `role`".to_owned(),
            })?
            .to_owned();
        let content = message.get("content").ok_or(ParseError::Malformed {
            parser: NAME,
            line: line_no,
            msg: "message missing required `content`".to_owned(),
        })?;
        let blocks = match content {
            Value::Array(arr) => arr
                .iter()
                .map(|b| Self::map_block(b, line_no))
                .collect::<Result<Vec<_>, _>>()?,
            Value::String(s) => vec![TraceBlock::Text { text: s.clone() }],
            other => vec![TraceBlock::Text {
                text: other.to_string(),
            }],
        };
        let timestamp = value
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_owned);
        Ok(ParsedTranscriptLine {
            session_id,
            role,
            blocks,
            timestamp,
        })
    }

    fn session_id_for(&self, line: &str, line_no: usize) -> Result<String, ParseError> {
        let value = Self::parse_value(line, line_no)?;
        Self::extract_session_id(&value, line_no)
    }
}
