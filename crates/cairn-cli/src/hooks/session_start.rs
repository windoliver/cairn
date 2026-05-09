//! `SessionStart` hook handler.

use std::path::Path;

use cairn_core::generated::common::Ulid;
use serde::Serialize;
use serde_json::Value;

use super::artifact::{self, ArtifactKind};
use super::{HookArtifacts, HookError, require_string};

#[derive(Serialize)]
struct HotArtifact {
    operation_id: Ulid,
    session_id: String,
    prefix: String,
    note: &'static str,
}

pub(super) fn run(
    vault_path: &Path,
    operation_id: Ulid,
    payload: &Value,
) -> Result<HookArtifacts, HookError> {
    let session_id = require_string(payload, "session_id")?;
    let artifact = HotArtifact {
        operation_id: operation_id.clone(),
        session_id,
        prefix: String::new(),
        note: "assemble_hot store path is not wired yet; empty prefix is the P0 hook boundary",
    };
    let written =
        artifact::write_json(vault_path, ArtifactKind::Hot, Some(operation_id), &artifact)?;
    let hot_path = written
        .path
        .strip_prefix(vault_path)
        .unwrap_or(&written.path)
        .to_string_lossy()
        .trim_start_matches('/')
        .to_owned();
    Ok(HookArtifacts {
        trace_id: None,
        hot_path: Some(hot_path),
        queued_jobs: Vec::new(),
    })
}
