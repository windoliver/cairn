//! Integration tests for agent extractor parser plumbing.

use cairn_core::domain::CaptureEventId;
use cairn_core::pipeline::extract::agent::{AgentParseError, parse_agent_response};

#[test]
fn parser_accepts_drafts_discards_and_evidence() {
    let event_id = CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ulid");
    let source = "Use shard alpha for refunds. Ignore the earlier typo.";
    let value = serde_json::json!({
        "drafts": [{
            "kind": "fact",
            "body": "Refund routing uses shard alpha.",
            "confidence": 0.91,
            "span": {"start": 4, "end": 27},
            "evidence": [{"tool": "retrieve", "claim": "source text says shard alpha"}]
        }],
        "discards": [{
            "reason": "earlier typo is explicitly superseded",
            "span": {"start": 29, "end": 53}
        }],
        "evidence": [{"tool": "search", "claim": "matched prior refund routing note"}]
    });

    let parsed = parse_agent_response(&event_id, source, value).expect("valid agent output");
    assert_eq!(parsed.drafts.len(), 1);
    assert_eq!(parsed.discards.len(), 1);
    assert_eq!(parsed.evidence.len(), 1);
}

#[test]
fn parser_rejects_out_of_bounds_spans() {
    let event_id = CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ulid");
    let value = serde_json::json!({
        "drafts": [{
            "kind": "fact",
            "body": "bad span",
            "confidence": 0.8,
            "span": {"start": 0, "end": 99}
        }],
        "discards": [],
        "evidence": []
    });

    let err = parse_agent_response(&event_id, "short", value).expect_err("span must be checked");
    assert!(matches!(err, AgentParseError::SpanOutOfBounds { .. }));
}

#[test]
fn parser_rejects_invalid_confidence() {
    let event_id = CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ulid");
    let value = serde_json::json!({
        "drafts": [{
            "kind": "fact",
            "body": "bad confidence",
            "confidence": 1.2,
            "span": {"start": 0, "end": 3}
        }],
        "discards": [],
        "evidence": []
    });

    let err =
        parse_agent_response(&event_id, "short", value).expect_err("confidence must be checked");
    assert!(matches!(
        err,
        AgentParseError::InvalidField {
            field: "drafts.confidence",
            ..
        }
    ));
}

#[test]
fn parser_accepts_empty_record_id_but_rejects_empty_tool_and_claim() {
    let event_id = CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ulid");
    let valid = serde_json::json!({
        "drafts": [],
        "discards": [],
        "evidence": [{"tool": "retrieve", "record_id": "", "claim": "matched note"}]
    });

    let parsed = parse_agent_response(&event_id, "source", valid).expect("valid evidence");
    assert_eq!(parsed.evidence[0].record_id.as_deref(), Some(""));

    let empty_tool = serde_json::json!({
        "drafts": [],
        "discards": [],
        "evidence": [{"tool": "", "claim": "matched note"}]
    });
    let err =
        parse_agent_response(&event_id, "source", empty_tool).expect_err("tool must be non-empty");
    assert!(matches!(
        err,
        AgentParseError::InvalidField {
            field: "evidence",
            ..
        }
    ));

    let empty_claim = serde_json::json!({
        "drafts": [],
        "discards": [],
        "evidence": [{"tool": "retrieve", "claim": ""}]
    });
    let err = parse_agent_response(&event_id, "source", empty_claim)
        .expect_err("claim must be non-empty");
    assert!(matches!(
        err,
        AgentParseError::InvalidField {
            field: "evidence",
            ..
        }
    ));
}
