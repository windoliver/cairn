//! Local cold-session bundle helpers for issue #107.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use cairn_core::domain::MemoryRecord;
use serde::{Deserialize, Serialize};

const COLD_SCHEMA: &str = "cairn.cold_session.v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ColdSessionBundle {
    pub(crate) schema: String,
    pub(crate) session_id: String,
    pub(crate) archived_at_ms: u128,
    pub(crate) records: Vec<MemoryRecord>,
}

impl ColdSessionBundle {
    pub(crate) fn new(
        session_id: String,
        archived_at_ms: u128,
        records: Vec<MemoryRecord>,
    ) -> Self {
        Self {
            schema: COLD_SCHEMA.to_owned(),
            session_id,
            archived_at_ms,
            records,
        }
    }
}

pub(crate) fn bundle_path(vault_root: &Path, session_id: &str) -> Result<PathBuf> {
    validate_session_id(session_id)?;
    Ok(cold_dir(vault_root).join(format!("session_{session_id}.json")))
}

pub(crate) fn load_bundle(
    vault_root: &Path,
    session_id: &str,
) -> Result<Option<ColdSessionBundle>> {
    let path = bundle_path(vault_root, session_id)?;
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let bundle: ColdSessionBundle =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    if bundle.schema != COLD_SCHEMA {
        bail!("unsupported cold-session schema {}", bundle.schema);
    }
    if bundle.session_id != session_id {
        bail!(
            "cold-session bundle {} belongs to session {}",
            path.display(),
            bundle.session_id
        );
    }
    Ok(Some(bundle))
}

pub(crate) fn write_bundle(vault_root: &Path, bundle: &ColdSessionBundle) -> Result<PathBuf> {
    let path = bundle_path(vault_root, &bundle.session_id)?;
    let dir = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("cold bundle path has no parent"))?;
    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(bundle).context("serialize cold-session bundle")?;
    fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| {
        format!(
            "atomically replace cold-session bundle {} from {}",
            path.display(),
            tmp.display()
        )
    })?;
    Ok(path)
}

fn cold_dir(vault_root: &Path) -> PathBuf {
    vault_root.join(".cairn").join("cold")
}

fn validate_session_id(session_id: &str) -> Result<()> {
    if session_id.is_empty() {
        bail!("session id must not be empty");
    }
    if session_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Ok(());
    }
    bail!("session id contains unsupported path characters");
}
