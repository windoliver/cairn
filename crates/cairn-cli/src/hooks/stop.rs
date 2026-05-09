//! `Stop` hook handler.

use std::path::Path;

use cairn_core::generated::common::Ulid;
use serde::Serialize;
use serde_json::Value;

use super::artifact::{self, ArtifactKind};
use super::{HookArtifacts, HookError, payload_object, queue, require_string};

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
    payload: &Value,
) -> Result<HookArtifacts, HookError> {
    let session_id = require_string(payload, "session_id")?;
    let trace_id = crate::verbs::envelope::new_operation_id();
    let artifact = TraceArtifact {
        operation_id: operation_id.clone(),
        hook: "Stop",
        session_id: session_id.clone(),
        event: payload_object(payload),
    };
    let written = artifact::write_json(vault_path, ArtifactKind::Trace, Some(trace_id), &artifact)?;
    let job_id =
        queue::enqueue_post_turn(vault_path, operation_id, session_id, written.id.clone())?;
    Ok(HookArtifacts {
        trace_id: Some(written.id),
        hot_path: None,
        queued_jobs: vec![job_id],
    })
}
