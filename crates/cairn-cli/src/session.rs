//! Adapter-side session source normalization.
//!
//! `cairn-core` owns pure precedence and validation. This module reads CLI
//! shaped inputs and filesystem project markers, then hands normalized values
//! to the core/session store layer.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use cairn_core::session::SessionIdCandidates;

/// Environment variable used as the CLI/session fallback.
pub const CAIRN_SESSION_ENV: &str = "CAIRN_SESSION_ID";

const PROJECT_MARKERS: &[&str] = &[
    ".cairn/config.yaml",
    ".git",
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "go.mod",
];

/// Normalized project metadata derived from cwd.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectContext {
    /// Stable project ID. For the local CLI adapter this is the canonical root
    /// path discovered from project markers.
    pub project_id: String,
    /// Canonical caller cwd.
    pub cwd: String,
}

/// Build direct session candidates from CLI/harness inputs and environment.
#[must_use]
pub fn session_candidates_from_env(
    explicit_arg: Option<String>,
    harness: Option<String>,
    env: &BTreeMap<String, String>,
) -> SessionIdCandidates {
    SessionIdCandidates {
        explicit_arg,
        harness,
        environment: env.get(CAIRN_SESSION_ENV).cloned(),
    }
}

/// Discover the project root for a cwd using nearby repository/vault markers.
///
/// If no marker exists, the canonical cwd itself is the project scope.
///
/// # Errors
/// Returns an error if `cwd` cannot be canonicalized.
pub fn discover_project_context(cwd: impl AsRef<Path>) -> Result<ProjectContext> {
    let cwd = cwd
        .as_ref()
        .canonicalize()
        .with_context(|| format!("canonicalizing cwd {}", cwd.as_ref().display()))?;
    let root = project_root_for(&cwd);
    Ok(ProjectContext {
        project_id: root.display().to_string(),
        cwd: cwd.display().to_string(),
    })
}

fn project_root_for(cwd: &Path) -> PathBuf {
    for ancestor in cwd.ancestors() {
        if PROJECT_MARKERS
            .iter()
            .any(|marker| ancestor.join(marker).exists())
        {
            return ancestor.to_path_buf();
        }
    }
    cwd.to_path_buf()
}
