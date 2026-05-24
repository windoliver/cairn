//! `SkillPack` manifest and validation.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// One skill entry in a `SkillPack`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillPackEntry {
    /// Candidate id.
    pub candidate_id: String,
    /// Skill lane.
    pub lane: String,
    /// Filesystem-safe slug.
    pub slug: String,
    /// Bundle schema version.
    pub bundle_version: u32,
    /// SHA-256 digest of this skill's bundle.
    pub artifact_sha256: String,
}

/// `SkillPack` manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillPackManifest {
    /// Deterministic pack id.
    pub pack_id: String,
    /// Human-readable pack name.
    pub name: String,
    /// Semver pack version.
    pub version: String,
    /// Minimum Cairn version required, e.g. `>=0.1.0`.
    pub cairn_compat: String,
    /// Pack description.
    pub description: String,
    /// Skills in this pack.
    pub skills: Vec<SkillPackEntry>,
    /// Aggregated dependencies.
    pub requires: Vec<String>,
    /// Aggregated capabilities.
    pub provides: Vec<String>,
    /// SHA-256 digest of the packed archive.
    pub content_sha256: String,
}

/// `SkillPack` validation error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SkillPackError {
    /// Pack skill not found in archive.
    #[error("pack skill `{candidate_id}` not found in archive")]
    MissingSkill {
        /// Candidate id.
        candidate_id: String,
    },
    /// Duplicate lane in pack.
    #[error("duplicate lane `{lane}` in pack")]
    DuplicateLane {
        /// Duplicated lane.
        lane: String,
    },
    /// Cairn version incompatibility.
    #[error("pack requires Cairn {required} but running {running}")]
    IncompatibleCairn {
        /// Required version string.
        required: String,
        /// Running version string.
        running: String,
    },
    /// Unsatisfied dependency.
    #[error("dependency `{dep}` not provided by any skill in pack")]
    DependencyMissing {
        /// Missing dependency.
        dep: String,
    },
    /// Content integrity check failed.
    #[error("content integrity check failed: expected {expected}, got {actual}")]
    IntegrityFailure {
        /// Expected digest.
        expected: String,
        /// Actual digest.
        actual: String,
    },
    /// Invalid pack name.
    #[error("invalid pack name: {reason}")]
    InvalidName {
        /// Rejection reason.
        reason: String,
    },
}

impl SkillPackManifest {
    /// Derive a deterministic pack id from name, version, and candidate ids.
    #[must_use]
    pub fn derive_pack_id(name: &str, version: &str, candidate_ids: &[&str]) -> String {
        let mut sorted = candidate_ids.to_vec();
        sorted.sort_unstable();
        let mut hasher = Sha256::new();
        hasher.update(name.as_bytes());
        hasher.update(b"\0");
        hasher.update(version.as_bytes());
        hasher.update(b"\0");
        for id in sorted {
            hasher.update(id.as_bytes());
            hasher.update(b"\0");
        }
        format!("skp_{:x}", hasher.finalize())
    }

    /// Validate the manifest against the running Cairn version.
    ///
    /// # Errors
    /// Returns [`SkillPackError`] on validation failure.
    pub fn validate(&self, cairn_version: &str) -> Result<(), SkillPackError> {
        self.validate_name()?;
        self.validate_entry_path_tokens()?;
        self.validate_no_duplicate_lanes()?;
        self.validate_cairn_compat(cairn_version)?;
        self.validate_dependencies()?;
        Ok(())
    }

    /// Reject manifest entries whose `candidate_id` or `slug` contain path
    /// separators, parent components, or non-token characters. Without this
    /// check, an archive can craft `candidate_id = "../evil"` and escape the
    /// `.cairn/evolution/skillify/` install root.
    fn validate_entry_path_tokens(&self) -> Result<(), SkillPackError> {
        for entry in &self.skills {
            if !is_safe_path_token(&entry.candidate_id) {
                return Err(SkillPackError::InvalidName {
                    reason: format!(
                        "candidate_id `{}` is not a safe path token \
                         (alphanumeric, hyphens, underscores only; no separators or dot components)",
                        entry.candidate_id
                    ),
                });
            }
            if !is_safe_path_token(&entry.slug) {
                return Err(SkillPackError::InvalidName {
                    reason: format!("slug `{}` is not a safe path token", entry.slug),
                });
            }
        }
        Ok(())
    }

    fn validate_name(&self) -> Result<(), SkillPackError> {
        if self.name.is_empty() {
            return Err(SkillPackError::InvalidName {
                reason: "name must not be empty".to_owned(),
            });
        }
        if !self
            .name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(SkillPackError::InvalidName {
                reason: format!(
                    "name `{}` contains invalid characters (only alphanumeric, hyphens, underscores)",
                    self.name
                ),
            });
        }
        Ok(())
    }

    fn validate_no_duplicate_lanes(&self) -> Result<(), SkillPackError> {
        let mut seen = BTreeSet::new();
        for entry in &self.skills {
            if !seen.insert(&entry.lane) {
                return Err(SkillPackError::DuplicateLane {
                    lane: entry.lane.clone(),
                });
            }
        }
        Ok(())
    }

    fn validate_cairn_compat(&self, cairn_version: &str) -> Result<(), SkillPackError> {
        let required = self
            .cairn_compat
            .strip_prefix(">=")
            .unwrap_or(&self.cairn_compat);
        if !version_gte(cairn_version, required) {
            return Err(SkillPackError::IncompatibleCairn {
                required: self.cairn_compat.clone(),
                running: cairn_version.to_owned(),
            });
        }
        Ok(())
    }

    fn validate_dependencies(&self) -> Result<(), SkillPackError> {
        let provided: BTreeSet<&str> = self.provides.iter().map(String::as_str).collect();
        for dep in &self.requires {
            if !provided.contains(dep.as_str()) {
                return Err(SkillPackError::DependencyMissing { dep: dep.clone() });
            }
        }
        Ok(())
    }
}

/// Path-token validator matching the rules used by [`SkillSpecDraft`] and
/// candidate id derivation. A token is safe iff it is non-empty, not `.` or
/// `..`, contains no path separators, and consists only of ASCII
/// alphanumerics, hyphens, and underscores.
fn is_safe_path_token(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains('/')
        && !value.contains('\\')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.split('.');
    Some((
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
    ))
}

fn version_gte(running: &str, required: &str) -> bool {
    match (parse_version(running), parse_version(required)) {
        (Some(r), Some(q)) => r >= q,
        _ => false,
    }
}
