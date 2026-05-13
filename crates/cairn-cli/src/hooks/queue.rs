//! Queue artifact persistence for post-turn hook work.

use std::path::Path;

use cairn_core::generated::common::Ulid;
use serde::Serialize;

use super::HookError;
use super::artifact::{self, ArtifactKind};

#[derive(Serialize)]
struct QueueArtifact {
    operation_id: Ulid,
    job_id: Ulid,
    session_id: String,
    trace_id: Ulid,
    kind: &'static str,
    status: &'static str,
}

pub(super) fn enqueue_post_turn(
    vault_path: &Path,
    operation_id: Ulid,
    session_id: String,
    trace_id: Ulid,
) -> Result<Ulid, HookError> {
    let job_id = crate::verbs::envelope::new_operation_id();
    let artifact = QueueArtifact {
        operation_id,
        job_id: job_id.clone(),
        session_id,
        trace_id,
        kind: "post_turn",
        status: "pending",
    };
    artifact::write_json(vault_path, ArtifactKind::Queue, Some(job_id), &artifact)
        .map(|written| written.id)
        .map_err(|err| {
            err.with_retry_guidance(
                "retry cairn hook Stop for the same session after restoring queue write access",
            )
        })
}
