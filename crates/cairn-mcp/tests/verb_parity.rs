//! CLI-vs-MCP verb parity: `dispatch_stub` must return `is_error=true` and
//! embed the verb name in content text — matching the CLI exit-2 branch.
#![allow(missing_docs)]

use cairn_mcp::generated::TOOLS;
use cairn_mcp::handler::dispatch_stub;
use rmcp::model::{Content, RawContent};

fn content_to_text(content: &[Content]) -> String {
    content
        .iter()
        .filter_map(|c| {
            if let RawContent::Text(t) = &**c {
                Some(t.text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn stub_returns_is_error_true_for_all_verbs() {
    for tool in TOOLS {
        let result = dispatch_stub(tool.name);
        assert_eq!(
            result.is_error,
            Some(true),
            "dispatch_stub for '{}' must set is_error=true",
            tool.name
        );
    }
}

#[test]
fn stub_embeds_verb_name_in_content() {
    for tool in TOOLS {
        let result = dispatch_stub(tool.name);
        assert!(
            !result.content.is_empty(),
            "dispatch_stub for '{}' must include content",
            tool.name
        );

        let content_text = content_to_text(&result.content);
        assert!(
            content_text.contains(tool.name),
            "dispatch_stub for '{}' must embed verb name in content; got: {content_text}",
            tool.name
        );
    }
}

#[test]
fn unknown_verb_triggers_handler_error_branch() {
    // Verify that dispatch_stub is a synchronous function that can be
    // called for any verb (including ones not in TOOLS) and always returns
    // is_error=true, matching the CLI's "not yet implemented" exit-2 branch.
    // The handler.rs call_tool function checks TOOLS membership and calls
    // dispatch_stub only for known verbs, but dispatch_stub itself must
    // work for any string without panicking.
    let result = dispatch_stub("hypothetical_unknown_verb");
    assert_eq!(
        result.is_error,
        Some(true),
        "dispatch_stub must set is_error=true for any verb"
    );
    assert!(
        !result.content.is_empty(),
        "dispatch_stub must include content"
    );
    let content_text = content_to_text(&result.content);
    assert!(
        content_text.contains("not yet implemented"),
        "dispatch_stub content must mention placeholder message; got: {content_text}"
    );
}
