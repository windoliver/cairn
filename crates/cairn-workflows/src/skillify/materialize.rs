//! Skillify candidate bundle materialization.

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use cairn_core::pipeline::skillify::{
    SkillArtifact, SkillArtifactBundle, SkillArtifactError, SkillArtifactKind, SkillifyGate,
    SkillifyGateReport, SkillifyGateStatus,
};
use sha2::{Digest, Sha256};

struct ExistingCandidateBundle {
    bundle: SkillArtifactBundle,
    report: SkillifyGateReport,
}

/// Structured skill bundle authored by an LLM before materialization.
#[derive(Debug, Clone)]
pub struct AuthoredSkillBundle {
    /// Skill lane declared by the generated contract.
    pub lane: String,
    /// Filesystem-safe skill slug.
    pub slug: String,
    /// Markdown skill contract.
    pub skill_markdown: String,
    /// Deterministic executable script body.
    pub script: String,
    /// Unit-test artifact JSON.
    pub unit_tests: serde_json::Value,
    /// Integration-test artifact JSON.
    pub integration_tests: serde_json::Value,
    /// LLM eval artifact JSON.
    pub llm_evals: serde_json::Value,
    /// Resolver trigger artifact JSON.
    pub resolver_triggers: serde_json::Value,
    /// Resolver eval artifact JSON.
    pub resolver_eval: serde_json::Value,
    /// End-to-end smoke artifact JSON.
    pub smoke: serde_json::Value,
    /// Filing rules artifact JSON.
    pub filing_rules: serde_json::Value,
}

impl TryFrom<serde_json::Value> for AuthoredSkillBundle {
    type Error = SkillifyMaterializeError;

    fn try_from(value: serde_json::Value) -> Result<Self, Self::Error> {
        let bundle = Self {
            lane: required_string(&value, "lane")?,
            slug: required_string(&value, "slug")?,
            skill_markdown: required_string(&value, "skill_markdown")?,
            script: required_string(&value, "script")?,
            unit_tests: required_value(&value, "unit_tests")?,
            integration_tests: required_value(&value, "integration_tests")?,
            llm_evals: required_value(&value, "llm_evals")?,
            resolver_triggers: required_value(&value, "resolver_triggers")?,
            resolver_eval: required_value(&value, "resolver_eval")?,
            smoke: required_value(&value, "smoke")?,
            filing_rules: required_value(&value, "filing_rules")?,
        };
        validate_path_token("slug", &bundle.slug)?;
        Ok(bundle)
    }
}

/// Candidate materialization failures.
#[derive(Debug, thiserror::Error)]
pub enum SkillifyMaterializeError {
    /// Required string field was absent or not a string.
    #[error("missing string field {field}")]
    MissingStringField {
        /// Field name.
        field: String,
    },
    /// Required JSON field was absent.
    #[error("missing field {field}")]
    MissingField {
        /// Field name.
        field: String,
    },
    /// A path token controlled by authored content or payload was unsafe.
    #[error("invalid {label} `{value}`")]
    InvalidPathToken {
        /// Token label.
        label: &'static str,
        /// Rejected token value.
        value: String,
    },
    /// Candidate directory exists but is not a complete ready bundle.
    #[error("candidate `{candidate_id}` already exists but is incomplete")]
    ExistingCandidateIncomplete {
        /// Candidate id.
        candidate_id: String,
    },
    /// JSON serialization/deserialization failed.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Filesystem operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Core artifact validation failed.
    #[error(transparent)]
    Artifact(#[from] SkillArtifactError),
}

impl SkillifyMaterializeError {
    /// Whether retrying the same job can reasonably fix this error.
    #[must_use]
    pub fn is_permanent(&self) -> bool {
        matches!(
            self,
            Self::MissingStringField { .. }
                | Self::MissingField { .. }
                | Self::InvalidPathToken { .. }
                | Self::ExistingCandidateIncomplete { .. }
                | Self::Json(_)
                | Self::Artifact(_)
        )
    }
}

/// Return true when the candidate already has a complete ready bundle.
///
/// # Errors
/// Returns an error when the candidate id is unsafe or an existing candidate
/// directory is incomplete.
pub fn candidate_ready(
    vault_root: &Path,
    candidate_id: &str,
) -> Result<bool, SkillifyMaterializeError> {
    validate_path_token("candidate id", candidate_id)?;
    let root = vault_root
        .join(".cairn/evolution/skillify")
        .join(candidate_id);
    read_existing_bundle(&root, candidate_id)
        .map(|existing| existing.is_some_and(|existing| existing.report.ready_for_promotion()))
}

/// Return true when the candidate has a complete materialized bundle,
/// regardless of whether promotion gates have passed.
///
/// # Errors
/// Returns an error when the candidate id is unsafe or an existing candidate
/// directory is malformed.
pub fn candidate_materialized(
    vault_root: &Path,
    candidate_id: &str,
) -> Result<bool, SkillifyMaterializeError> {
    validate_path_token("candidate id", candidate_id)?;
    let root = vault_root
        .join(".cairn/evolution/skillify")
        .join(candidate_id);
    read_existing_bundle(&root, candidate_id).map(|existing| existing.is_some())
}

/// Write a lint-visible blocked candidate marker when authoring cannot start.
///
/// # Errors
/// Returns when the candidate id is unsafe or the marker cannot be written.
pub fn materialize_blocked_candidate(
    vault_root: &Path,
    candidate_id: &str,
    message: &str,
) -> Result<(), SkillifyMaterializeError> {
    validate_path_token("candidate id", candidate_id)?;
    if candidate_materialized(vault_root, candidate_id)? {
        return Ok(());
    }

    let root = vault_root
        .join(".cairn/evolution/skillify")
        .join(candidate_id);
    fs::create_dir_all(&root)?;
    let report = blocked_gate_report(candidate_id, message);
    fs::write(
        root.join("gate-report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    Ok(())
}

/// Materialize a candidate bundle under `.cairn/evolution/skillify/{candidate_id}`.
///
/// # Errors
/// Returns an error when artifact data cannot be serialized, paths are unsafe,
/// writes fail, or the generated bundle does not satisfy required coverage.
pub fn materialize_bundle(
    vault_root: &Path,
    candidate_id: &str,
    authored: &AuthoredSkillBundle,
    evidence_refs: &[String],
) -> Result<SkillArtifactBundle, SkillifyMaterializeError> {
    validate_path_token("candidate id", candidate_id)?;

    let parent = vault_root.join(".cairn/evolution/skillify");
    fs::create_dir_all(&parent)?;
    let root = parent.join(candidate_id);
    if let Some(existing) = read_existing_bundle(&root, candidate_id)? {
        return Ok(existing.bundle);
    }
    if blocked_placeholder_exists(&root, candidate_id)? {
        fs::remove_dir_all(&root)?;
    }

    let temp_root = temp_candidate_dir(&parent, candidate_id);
    fs::create_dir(&temp_root)?;
    let result = materialize_new_bundle(&temp_root, candidate_id, authored, evidence_refs);
    let bundle = match result {
        Ok(bundle) => bundle,
        Err(e) => {
            let _ = fs::remove_dir_all(&temp_root);
            return Err(e);
        }
    };

    match fs::rename(&temp_root, &root) {
        Ok(()) => Ok(bundle),
        Err(e) if root.exists() => {
            let _ = fs::remove_dir_all(&temp_root);
            if let Some(existing) = read_existing_bundle(&root, candidate_id)? {
                Ok(existing.bundle)
            } else {
                Err(SkillifyMaterializeError::Io(e))
            }
        }
        Err(e) => {
            let _ = fs::remove_dir_all(&temp_root);
            Err(SkillifyMaterializeError::Io(e))
        }
    }
}

fn materialize_new_bundle(
    root: &Path,
    candidate_id: &str,
    authored: &AuthoredSkillBundle,
    evidence_refs: &[String],
) -> Result<SkillArtifactBundle, SkillifyMaterializeError> {
    let bundle_root = root.join("bundle");
    let files = [
        (
            SkillArtifactKind::SkillContract,
            format!("skills/skill_{}.md", authored.slug),
            authored.skill_markdown.clone(),
        ),
        (
            SkillArtifactKind::DeterministicScript,
            format!("scripts/{}.sh", authored.slug),
            authored.script.clone(),
        ),
        (
            SkillArtifactKind::UnitTests,
            format!("tests/unit/{}.json", authored.slug),
            serde_json::to_string_pretty(&authored.unit_tests)?,
        ),
        (
            SkillArtifactKind::IntegrationTests,
            format!("tests/integration/{}.json", authored.slug),
            serde_json::to_string_pretty(&authored.integration_tests)?,
        ),
        (
            SkillArtifactKind::LlmEvals,
            format!("evals/llm/{}.json", authored.slug),
            serde_json::to_string_pretty(&authored.llm_evals)?,
        ),
        (
            SkillArtifactKind::ResolverTrigger,
            "resolver/triggers.json".to_owned(),
            serde_json::to_string_pretty(&authored.resolver_triggers)?,
        ),
        (
            SkillArtifactKind::ResolverEval,
            "resolver/eval.json".to_owned(),
            serde_json::to_string_pretty(&authored.resolver_eval)?,
        ),
        (
            SkillArtifactKind::CheckResolvableAndDry,
            "audits/check-resolvable.json".to_owned(),
            "{\"status\":\"passed\"}\n".to_owned(),
        ),
        (
            SkillArtifactKind::E2eSmoke,
            format!("smoke/{}.json", authored.slug),
            serde_json::to_string_pretty(&authored.smoke)?,
        ),
        (
            SkillArtifactKind::FilingRules,
            "filing-rules.json".to_owned(),
            serde_json::to_string_pretty(&authored.filing_rules)?,
        ),
    ];

    let mut artifacts = Vec::with_capacity(files.len());
    for (kind, rel, body) in files {
        let path = bundle_root.join(&rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, body.as_bytes())?;
        artifacts.push(SkillArtifact {
            kind,
            path: format!("bundle/{rel}"),
            content_sha256: sha256_prefixed(body.as_bytes()),
            evidence_refs: evidence_refs.to_vec(),
            status: "generated".to_owned(),
        });
    }

    let bundle = SkillArtifactBundle {
        candidate_id: candidate_id.to_owned(),
        version: 1,
        artifacts,
    };
    bundle.validate()?;

    fs::write(
        root.join("manifest.json"),
        serde_json::to_vec_pretty(&bundle)?,
    )?;
    let report = SkillifyGateReport {
        candidate_id: candidate_id.to_owned(),
        gates: required_blocked_gates("gate not executed"),
    };
    fs::write(
        root.join("gate-report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;

    Ok(bundle)
}

fn read_existing_bundle(
    root: &Path,
    candidate_id: &str,
) -> Result<Option<ExistingCandidateBundle>, SkillifyMaterializeError> {
    if !root.exists() {
        return Ok(None);
    }

    let manifest_path = root.join("manifest.json");
    let gate_report_path = root.join("gate-report.json");
    if !manifest_path.exists() || !gate_report_path.exists() {
        if blocked_placeholder_exists(root, candidate_id)? {
            return Ok(None);
        }
        return Err(SkillifyMaterializeError::ExistingCandidateIncomplete {
            candidate_id: candidate_id.to_owned(),
        });
    }

    let bundle: SkillArtifactBundle = serde_json::from_slice(&fs::read(&manifest_path)?)?;
    let report: SkillifyGateReport = serde_json::from_slice(&fs::read(&gate_report_path)?)?;
    if bundle.candidate_id != candidate_id || report.candidate_id != candidate_id {
        return Err(SkillifyMaterializeError::ExistingCandidateIncomplete {
            candidate_id: candidate_id.to_owned(),
        });
    }
    bundle.validate()?;
    for artifact in &bundle.artifacts {
        let artifact_path = root.join(&artifact.path);
        let metadata = match fs::metadata(&artifact_path) {
            Ok(metadata) => metadata,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(SkillifyMaterializeError::ExistingCandidateIncomplete {
                    candidate_id: candidate_id.to_owned(),
                });
            }
            Err(e) => return Err(SkillifyMaterializeError::Io(e)),
        };
        if !metadata.is_file() {
            return Err(SkillifyMaterializeError::ExistingCandidateIncomplete {
                candidate_id: candidate_id.to_owned(),
            });
        }
        let bytes = fs::read(&artifact_path)?;
        if sha256_prefixed(&bytes) != artifact.content_sha256 {
            return Err(SkillifyMaterializeError::ExistingCandidateIncomplete {
                candidate_id: candidate_id.to_owned(),
            });
        }
    }
    Ok(Some(ExistingCandidateBundle { bundle, report }))
}

fn blocked_placeholder_exists(
    root: &Path,
    candidate_id: &str,
) -> Result<bool, SkillifyMaterializeError> {
    let manifest_path = root.join("manifest.json");
    let gate_report_path = root.join("gate-report.json");
    if manifest_path.exists() || !gate_report_path.exists() {
        return Ok(false);
    }
    let report: SkillifyGateReport = serde_json::from_slice(&fs::read(&gate_report_path)?)?;
    Ok(report.candidate_id == candidate_id && !report.ready_for_promotion())
}

fn temp_candidate_dir(parent: &Path, candidate_id: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    parent.join(format!(
        ".{candidate_id}.tmp-{}-{nanos}",
        std::process::id()
    ))
}

fn required_string(
    value: &serde_json::Value,
    key: &str,
) -> Result<String, SkillifyMaterializeError> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| SkillifyMaterializeError::MissingStringField {
            field: key.to_owned(),
        })
}

fn required_value(
    value: &serde_json::Value,
    key: &str,
) -> Result<serde_json::Value, SkillifyMaterializeError> {
    value
        .get(key)
        .cloned()
        .ok_or_else(|| SkillifyMaterializeError::MissingField {
            field: key.to_owned(),
        })
}

pub(super) fn validate_path_token(
    label: &'static str,
    value: &str,
) -> Result<(), SkillifyMaterializeError> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err(SkillifyMaterializeError::InvalidPathToken {
            label,
            value: value.to_owned(),
        });
    }
    Ok(())
}

fn sha256_prefixed(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("sha256:{:x}", h.finalize())
}

fn blocked_gate_report(candidate_id: &str, message: &str) -> SkillifyGateReport {
    SkillifyGateReport {
        candidate_id: candidate_id.to_owned(),
        gates: required_blocked_gates(message),
    }
}

fn required_blocked_gates(message: &str) -> Vec<SkillifyGate> {
    SkillArtifactKind::required()
        .iter()
        .map(|kind| SkillifyGate {
            name: kind.as_str().to_owned(),
            status: SkillifyGateStatus::Blocked,
            message: Some(message.to_owned()),
        })
        .collect()
}
