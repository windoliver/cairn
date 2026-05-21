//! Prompt rendering for the read-only agent extractor.

use std::fmt::Write;

use crate::domain::CaptureEventId;
use crate::pipeline::extract::TextSpan;

use super::AGENT_EXTRACTOR_OUTPUT_SCHEMA;

/// Render the prompt given to the read-only agent extractor runtime.
pub fn render_agent_extract_prompt(
    source_event: &CaptureEventId,
    source_text: &str,
    eligible_spans: &[TextSpan],
) -> String {
    let mut prompt = String::new();
    prompt.push_str("You are Cairn's read-only agent extractor.\n");
    prompt.push_str("Return only JSON matching AGENT_EXTRACTOR_OUTPUT_SCHEMA.\n");
    prompt.push_str(
        "Do not write, delete, promote memories, change policy, or perform side effects.\n",
    );
    prompt.push_str("Use byte offsets into the capture text for every span.\n\n");

    prompt.push_str("AGENT_EXTRACTOR_OUTPUT_SCHEMA:\n");
    prompt.push_str(AGENT_EXTRACTOR_OUTPUT_SCHEMA);
    prompt.push_str("\n\n");

    let _ = writeln!(prompt, "event_id: {}", source_event.as_str());
    prompt.push_str("eligible_spans:\n");
    if eligible_spans.is_empty() {
        prompt.push_str("- none\n");
    } else {
        for span in eligible_spans {
            let _ = writeln!(prompt, "- {}..{}", span.start, span.end);
        }
    }

    prompt.push_str("\ncapture_text:\n");
    prompt.push_str(source_text);

    prompt
}
