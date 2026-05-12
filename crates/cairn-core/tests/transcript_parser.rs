//! Transcript parser registry coverage (issue #311 Task 4).

use cairn_core::domain::trace::TraceBlock;
use cairn_core::replay::{ParseError, detect_parser};

#[test]
fn claude_code_parser_preserves_mixed_blocks() {
    let line = serde_json::json!({
        "sessionId": "sess-123",
        "message": {
            "role": "assistant",
            "content": [
                { "type": "thinking", "thinking": "consider tools", "signature": "sig_abc" },
                { "type": "text", "text": "I will search." },
                { "type": "tool_use", "name": "rg", "input": { "pattern": "foo" }, "id": "tool-1" },
                { "type": "tool_result", "tool_use_id": "tool-1", "content": "match found", "is_error": false }
            ]
        }
    })
    .to_string();

    let parser = detect_parser(&line, None).expect("detect parser");
    let parsed = parser.parse_line(&line, 1).expect("parse line");

    assert_eq!(parser.name(), "claude-code");
    assert_eq!(parsed.session_id, "sess-123");
    assert_eq!(parsed.role, "assistant");
    assert_eq!(parsed.blocks.len(), 4);
    assert!(matches!(
        &parsed.blocks[0],
        TraceBlock::Reasoning { signature: Some(sig), .. } if sig == "sig_abc"
    ));
    assert!(matches!(&parsed.blocks[1], TraceBlock::Text { .. }));
    assert!(
        matches!(&parsed.blocks[2], TraceBlock::ToolUse { tool, id, .. } if tool == "rg" && id == "tool-1")
    );
    assert!(matches!(
        &parsed.blocks[3],
        TraceBlock::ToolResult { tool_use_id, content, is_error: false } if tool_use_id == "tool-1" && content == "match found"
    ));
}

#[test]
fn generic_parser_emits_single_text_block() {
    let line = "plain transcript line";
    let parser = detect_parser(line, None).expect("detect parser");
    let parsed = parser.parse_line(line, 1).expect("parse line");

    assert_eq!(parser.name(), "generic");
    assert_eq!(parsed.role, "unknown");
    assert!(parsed.session_id.starts_with("generic-"));
    assert_eq!(
        parsed.blocks,
        vec![TraceBlock::Text {
            text: line.to_owned(),
        }]
    );
}

#[test]
fn malformed_line_carries_parser_name_and_row() {
    let parser = detect_parser("{", Some("claude-code")).expect("forced parser");
    let err = parser
        .parse_line("{", 3)
        .expect_err("malformed JSON must fail");
    let display = err.to_string();
    assert!(display.contains("claude-code"), "{display}");
    assert!(display.contains("line 3"), "{display}");
    assert!(matches!(
        err,
        ParseError::Malformed {
            parser: "claude-code",
            line: 3,
            ..
        }
    ));
}

#[test]
fn unknown_harness_is_rejected() {
    match detect_parser("anything", Some("not-a-thing")) {
        Err(ParseError::UnknownHarness(s)) => assert_eq!(s, "not-a-thing"),
        Err(other) => panic!("unexpected error: {other:?}"),
        Ok(_) => panic!("expected UnknownHarness"),
    }
}

#[test]
fn detect_picks_claude_code_for_shaped_line_and_generic_otherwise() {
    let shaped = serde_json::json!({
        "sessionId": "sess",
        "message": { "role": "user", "content": "hi" }
    })
    .to_string();
    assert_eq!(
        detect_parser(&shaped, None).expect("detect").name(),
        "claude-code"
    );
    assert_eq!(
        detect_parser("just text", None).expect("detect").name(),
        "generic"
    );
}
