//! Deterministic parser for read-only agent extractor output.

use serde_json::Value;

use crate::domain::CaptureEventId;
use crate::domain::taxonomy::MemoryKind;
use crate::pipeline::extract::draft::{Confidence, KindHint, MemoryDraft};
use crate::pipeline::extract::{DiscardCandidate, DiscardReason, TextSpan};

/// Evidence emitted by the read-only agent extractor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentEvidence {
    /// Tool or retrieval path that produced the evidence.
    pub tool: String,
    /// Optional referenced memory or record identifier.
    pub record_id: Option<String>,
    /// Short natural-language claim supported by the tool output.
    pub claim: String,
}

/// Fully parsed agent extractor response.
#[derive(Debug, Clone)]
pub struct ParsedAgentResponse {
    /// Validated draft memories proposed by the agent extractor.
    pub drafts: Vec<MemoryDraft>,
    /// Validated discard candidates proposed by the agent extractor.
    pub discards: Vec<DiscardCandidate>,
    /// Top-level evidence records emitted by the agent extractor.
    pub evidence: Vec<AgentEvidence>,
}

/// Errors returned when deterministic validation of agent output fails.
#[derive(Debug, thiserror::Error)]
pub enum AgentParseError {
    /// The response root is not a JSON object.
    #[error("agent output is not an object")]
    NotObject,
    /// A declared byte span is invalid for the supplied source text.
    #[error("agent output span {start}..{end} is outside source length {len}")]
    SpanOutOfBounds {
        /// Inclusive start byte offset.
        start: usize,
        /// Exclusive end byte offset.
        end: usize,
        /// Source text length in bytes.
        len: usize,
    },
    /// A required field is missing, malformed, or semantically invalid.
    #[error("agent output field `{field}` is invalid: {reason}")]
    InvalidField {
        /// Stable parser field path.
        field: &'static str,
        /// Human-readable validation failure.
        reason: String,
    },
}

/// Parse and validate one read-only agent extractor JSON response.
pub fn parse_agent_response(
    source_event: &CaptureEventId,
    source_text: &str,
    value: Value,
) -> Result<ParsedAgentResponse, AgentParseError> {
    let object = value.as_object().ok_or(AgentParseError::NotObject)?;

    let drafts_value = required_array(object.get("drafts"), "drafts")?;
    let discards_value = required_array(object.get("discards"), "discards")?;
    let evidence_value = required_array(object.get("evidence"), "evidence")?;

    let drafts = drafts_value
        .iter()
        .map(|draft| parse_draft(source_event, source_text, draft))
        .collect::<Result<Vec<_>, _>>()?;
    let discards = discards_value
        .iter()
        .map(|discard| parse_discard(source_text, discard))
        .collect::<Result<Vec<_>, _>>()?;
    let evidence = evidence_value
        .iter()
        .map(|evidence| parse_evidence(evidence, "evidence"))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ParsedAgentResponse {
        drafts,
        discards,
        evidence,
    })
}

fn required_array<'a>(
    value: Option<&'a Value>,
    field: &'static str,
) -> Result<&'a [Value], AgentParseError> {
    value
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| AgentParseError::InvalidField {
            field,
            reason: "expected array".to_owned(),
        })
}

fn parse_draft(
    source_event: &CaptureEventId,
    source_text: &str,
    value: &Value,
) -> Result<MemoryDraft, AgentParseError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("drafts", "expected object"))?;

    let kind_raw = non_empty_string(object.get("kind"), "drafts.kind")?;
    let kind =
        MemoryKind::parse(kind_raw).map_err(|err| invalid_owned("drafts.kind", err.to_string()))?;
    let body = non_empty_string(object.get("body"), "drafts.body")?.to_owned();
    let confidence_raw = object
        .get("confidence")
        .and_then(Value::as_f64)
        .ok_or_else(|| invalid("drafts.confidence", "expected number"))?;
    #[allow(clippy::cast_possible_truncation)]
    let confidence_f32 = confidence_raw as f32;
    let confidence = Confidence::try_from(confidence_f32)
        .map_err(|err| invalid_owned("drafts.confidence", err.to_string()))?;
    let source_span = Some(parse_span(object.get("span"), "drafts.span", source_text)?);

    if let Some(evidence) = object.get("evidence") {
        let evidence_items = evidence
            .as_array()
            .ok_or_else(|| invalid("drafts.evidence", "expected array"))?;
        for item in evidence_items {
            parse_evidence(item, "drafts.evidence")?;
        }
    }

    Ok(MemoryDraft {
        kind_hint: KindHint::from(kind),
        body,
        confidence,
        source_event: source_event.clone(),
        source_span,
        trigger_id: None,
    })
}

fn parse_discard(source_text: &str, value: &Value) -> Result<DiscardCandidate, AgentParseError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("discards", "expected object"))?;
    let reason_text = non_empty_string(object.get("reason"), "discards.reason")?;
    let source_span = parse_span(object.get("span"), "discards.span", source_text)?;

    Ok(DiscardCandidate {
        reason: parse_discard_reason(reason_text),
        source_span,
        evidence: reason_text.to_owned(),
    })
}

fn parse_evidence(value: &Value, field: &'static str) -> Result<AgentEvidence, AgentParseError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid(field, "expected object"))?;
    let tool = non_empty_string(object.get("tool"), field)?.to_owned();
    let claim = non_empty_string(object.get("claim"), field)?.to_owned();
    let record_id = match object.get("record_id") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_str()
                .ok_or_else(|| invalid(field, "expected string or null record_id"))?
                .to_owned(),
        ),
    };

    Ok(AgentEvidence {
        tool,
        record_id,
        claim,
    })
}

fn parse_span(
    value: Option<&Value>,
    field: &'static str,
    source_text: &str,
) -> Result<TextSpan, AgentParseError> {
    let object = value
        .and_then(Value::as_object)
        .ok_or_else(|| invalid(field, "expected object"))?;
    let start = object
        .get("start")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid(field, "expected unsigned integer start"))?;
    let end = object
        .get("end")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid(field, "expected unsigned integer end"))?;
    let start = usize::try_from(start)
        .map_err(|_| invalid_owned(field, "start does not fit usize".to_owned()))?;
    let end = usize::try_from(end)
        .map_err(|_| invalid_owned(field, "end does not fit usize".to_owned()))?;

    if start >= end || end > source_text.len() {
        return Err(AgentParseError::SpanOutOfBounds {
            start,
            end,
            len: source_text.len(),
        });
    }

    let start_u32 = u32::try_from(start)
        .map_err(|_| invalid_owned(field, "start does not fit u32".to_owned()))?;
    let end_u32 =
        u32::try_from(end).map_err(|_| invalid_owned(field, "end does not fit u32".to_owned()))?;

    Ok(TextSpan::new(start_u32, end_u32))
}

fn non_empty_string<'a>(
    value: Option<&'a Value>,
    field: &'static str,
) -> Result<&'a str, AgentParseError> {
    let value = value
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(field, "expected string"))?;
    if value.is_empty() {
        return Err(invalid(field, "expected non-empty string"));
    }
    Ok(value)
}

fn parse_discard_reason(value: &str) -> DiscardReason {
    match value {
        "volatile" => DiscardReason::Volatile,
        "tool_lookup" => DiscardReason::ToolLookup,
        "competing_source" => DiscardReason::CompetingSource,
        "low_salience" => DiscardReason::LowSalience,
        "other" => DiscardReason::Other,
        _ => DiscardReason::Other,
    }
}

fn invalid(field: &'static str, reason: &'static str) -> AgentParseError {
    invalid_owned(field, reason.to_owned())
}

fn invalid_owned(field: &'static str, reason: String) -> AgentParseError {
    AgentParseError::InvalidField { field, reason }
}
