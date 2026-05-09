//! `UserPromptSubmit` hook handler.

use std::path::Path;

use cairn_core::generated::common::Ulid;
use serde::Serialize;
use serde_json::Value;

use super::artifact::{self, ArtifactKind};
use super::{HookArtifacts, HookError, HookRoutingHints, payload_object, require_string};

#[derive(Serialize)]
struct TraceArtifact {
    operation_id: Ulid,
    hook: &'static str,
    session_id: String,
    event: serde_json::Map<String, Value>,
}

pub(super) fn run(
    vault_path: &Path,
    operation_id: Ulid,
    payload: Value,
) -> Result<HookArtifacts, HookError> {
    let session_id = require_string(&payload, "session_id")?;
    let _prompt = require_string(&payload, "prompt")?;
    let trace_id = crate::verbs::envelope::new_operation_id();
    let artifact = TraceArtifact {
        operation_id,
        hook: "UserPromptSubmit",
        session_id,
        event: payload_object(&payload),
    };
    let written = artifact::write_json(vault_path, ArtifactKind::Trace, Some(trace_id), &artifact)?;
    Ok(HookArtifacts {
        trace_id: Some(written.id),
        hot_path: None,
        queued_jobs: Vec::new(),
    })
}

pub(super) fn routing_hints(payload: &Value) -> HookRoutingHints {
    let prompt = payload
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    HookRoutingHints {
        capture_prompt: true,
        memory_write_suggested: contains_any(
            &prompt,
            &["remember", "save", "note that", "keep track", "prefer"],
        ),
        forget_suggested: contains_any(&prompt, &["forget", "delete", "remove"]),
        search_suggested: contains_any(
            &prompt,
            &[
                "what do you know",
                "search",
                "find",
                "recall",
                "remember when",
            ],
        ),
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}
