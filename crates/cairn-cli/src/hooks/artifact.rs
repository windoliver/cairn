//! Durable JSON artifact persistence for harness hooks.

use std::io::Write;
use std::path::{Path, PathBuf};

use cairn_core::generated::common::Ulid;
use serde::Serialize;

use super::HookError;
use crate::verbs::envelope::new_operation_id;

#[derive(Debug, Clone, Copy)]
pub(super) enum ArtifactKind {
    Hot,
    Queue,
    Trace,
}

impl ArtifactKind {
    const fn dir_name(self) -> &'static str {
        match self {
            Self::Hot => "hot",
            Self::Queue => "queue",
            Self::Trace => "traces",
        }
    }
}

pub(super) struct ArtifactWrite {
    pub id: Ulid,
    pub path: PathBuf,
}

pub(super) fn write_json<T: Serialize>(
    vault_path: &Path,
    kind: ArtifactKind,
    id: Option<Ulid>,
    value: &T,
) -> Result<ArtifactWrite, HookError> {
    let id = id.unwrap_or_else(new_operation_id);
    let dir = vault_path
        .join(".cairn")
        .join("hooks")
        .join(kind.dir_name());
    create_dir_all_synced(&dir)?;

    let final_path = dir.join(format!("{}.json", id.0));
    let tmp_path = dir.join(format!(".{}.tmp", id.0));
    let mut file = std::fs::File::create(&tmp_path).map_err(|err| {
        HookError::internal(
            format!(
                "failed to create hook artifact `{}`: {err}",
                tmp_path.display()
            ),
            "restore write access to the vault path and retry the same hook command",
        )
    })?;
    let bytes = serde_json::to_vec(value).map_err(|err| {
        HookError::internal(
            format!("failed to encode hook artifact: {err}"),
            "retry the hook command; report this operation_id if encoding fails again",
        )
    })?;
    file.write_all(&bytes).map_err(|err| {
        HookError::internal(
            format!(
                "failed to write hook artifact `{}`: {err}",
                tmp_path.display()
            ),
            "restore write access to the vault path and retry the same hook command",
        )
    })?;
    file.write_all(b"\n").map_err(|err| {
        HookError::internal(
            format!(
                "failed to finish hook artifact `{}`: {err}",
                tmp_path.display()
            ),
            "restore write access to the vault path and retry the same hook command",
        )
    })?;
    file.sync_all().map_err(|err| {
        HookError::internal(
            format!(
                "failed to sync hook artifact `{}`: {err}",
                tmp_path.display()
            ),
            "restore durable storage for the vault path and retry the same hook command",
        )
    })?;
    std::fs::rename(&tmp_path, &final_path).map_err(|err| {
        HookError::internal(
            format!(
                "failed to publish hook artifact `{}`: {err}",
                final_path.display()
            ),
            "restore write access to the vault path and retry the same hook command",
        )
    })?;
    sync_directory(&dir)?;
    Ok(ArtifactWrite {
        id,
        path: final_path,
    })
}

fn create_dir_all_synced(dir: &Path) -> Result<(), HookError> {
    let mut missing = Vec::new();
    let mut cursor = Some(dir);
    while let Some(path) = cursor {
        if path.exists() {
            break;
        }
        missing.push(path.to_path_buf());
        cursor = path.parent();
    }

    std::fs::create_dir_all(dir).map_err(|err| {
        HookError::internal(
            format!(
                "failed to create hook artifact directory `{}`: {err}",
                dir.display()
            ),
            "restore write access to the vault path and retry the same hook command",
        )
    })?;

    // Persist newly-created directory entries before relying on artifacts below them.
    for created in missing.iter().rev() {
        if let Some(parent) = created.parent() {
            sync_directory(parent)?;
        }
    }

    Ok(())
}

fn sync_directory(dir: &Path) -> Result<(), HookError> {
    #[cfg(unix)]
    {
        let dir_file = std::fs::File::open(dir).map_err(|err| {
            HookError::internal(
                format!(
                    "failed to open hook artifact directory `{}` for sync: {err}",
                    dir.display()
                ),
                "restore durable storage for the vault path and retry the same hook command",
            )
        })?;
        dir_file.sync_all().map_err(|err| {
            HookError::internal(
                format!(
                    "failed to sync hook artifact directory `{}`: {err}",
                    dir.display()
                ),
                "restore durable storage for the vault path and retry the same hook command",
            )
        })?;
    }

    #[cfg(not(unix))]
    {
        let _ = dir;
    }

    Ok(())
}
