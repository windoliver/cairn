//! `PostToolUse` hook handler.

use std::path::Path;

use cairn_core::generated::common::Ulid;
use serde::Serialize;
use serde_json::Value;

use super::artifact::{self, ArtifactKind};
use super::{HookArtifacts, HookError, payload_object, require_string};

#[derive(Serialize)]
struct TraceArtifact {
    operation_id: Ulid,
    hook: &'static str,
    session_id: String,
    tool_call_id: String,
    tool_name: String,
    status: String,
    event: serde_json::Map<String, Value>,
}

pub(super) fn run(
    vault_path: &Path,
    operation_id: Ulid,
    payload: &Value,
) -> Result<HookArtifacts, HookError> {
    let session_id = require_string(payload, "session_id")?;
    let tool_call_id = require_string(payload, "tool_call_id")?;
    let tool_name = require_string(payload, "tool_name")?;
    let status = require_string(payload, "status")?;
    let trace_id = crate::verbs::envelope::new_operation_id();
    let artifact = TraceArtifact {
        operation_id,
        hook: "PostToolUse",
        session_id,
        tool_call_id,
        tool_name,
        status,
        event: payload_object(payload),
    };
    let written = artifact::write_json(vault_path, ArtifactKind::Trace, Some(trace_id), &artifact)?;
    Ok(HookArtifacts {
        trace_id: Some(written.id),
        hot_path: None,
        queued_jobs: Vec::new(),
    })
}
