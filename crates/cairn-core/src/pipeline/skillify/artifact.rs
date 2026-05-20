//! Skillify artifact bundle data model and validation.

use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

/// Artifact kinds required before a skillify candidate can advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillArtifactKind {
    /// Skill contract markdown.
    SkillContract,
    /// Deterministic executable script.
    DeterministicScript,
    /// Unit test artifact.
    UnitTests,
    /// Integration test artifact.
    IntegrationTests,
    /// LLM eval artifact.
    LlmEvals,
    /// Resolver trigger artifact.
    ResolverTrigger,
    /// Resolver eval artifact.
    ResolverEval,
    /// Check-resolvable dry-run audit artifact.
    CheckResolvableAndDry,
    /// End-to-end smoke artifact.
    E2eSmoke,
    /// Filing rules artifact.
    FilingRules,
}

impl SkillArtifactKind {
    /// Return this artifact kind's stable snake_case name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SkillContract => "skill_contract",
            Self::DeterministicScript => "deterministic_script",
            Self::UnitTests => "unit_tests",
            Self::IntegrationTests => "integration_tests",
            Self::LlmEvals => "llm_evals",
            Self::ResolverTrigger => "resolver_trigger",
            Self::ResolverEval => "resolver_eval",
            Self::CheckResolvableAndDry => "check_resolvable_and_dry",
            Self::E2eSmoke => "e2e_smoke",
            Self::FilingRules => "filing_rules",
        }
    }

    /// Required artifact kinds in gate evaluation order.
    pub const fn required() -> &'static [Self; 10] {
        &[
            Self::SkillContract,
            Self::DeterministicScript,
            Self::UnitTests,
            Self::IntegrationTests,
            Self::LlmEvals,
            Self::ResolverTrigger,
            Self::ResolverEval,
            Self::CheckResolvableAndDry,
            Self::E2eSmoke,
            Self::FilingRules,
        ]
    }

    /// Default bundle-relative path for this artifact kind.
    pub fn default_relative_path(self, slug: &str) -> String {
        match self {
            Self::SkillContract => format!("bundle/skills/skill_{slug}.md"),
            Self::DeterministicScript => format!("bundle/scripts/{slug}.sh"),
            Self::UnitTests => format!("bundle/tests/unit/{slug}.json"),
            Self::IntegrationTests => format!("bundle/tests/integration/{slug}.json"),
            Self::LlmEvals => format!("bundle/evals/llm/{slug}.json"),
            Self::ResolverTrigger => "bundle/resolver/triggers.json".to_owned(),
            Self::ResolverEval => "bundle/resolver/eval.json".to_owned(),
            Self::CheckResolvableAndDry => "bundle/audits/check-resolvable.json".to_owned(),
            Self::E2eSmoke => format!("bundle/smoke/{slug}.json"),
            Self::FilingRules => "bundle/filing-rules.json".to_owned(),
        }
    }
}

impl std::fmt::Display for SkillArtifactKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Generated artifact in a skillify candidate bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillArtifact {
    /// Artifact kind.
    pub kind: SkillArtifactKind,
    /// Bundle-relative artifact path.
    pub path: String,
    /// SHA-256 digest of artifact content.
    pub content_sha256: String,
    /// Evidence records backing the artifact.
    pub evidence_refs: Vec<String>,
    /// Artifact generation status.
    pub status: String,
}

impl SkillArtifact {
    /// Validate that the artifact path stays within the candidate bundle.
    ///
    /// # Errors
    /// Returns [`SkillArtifactError`] when the path is absolute or escapes via
    /// parent/root/prefix components.
    pub fn validate_path(&self) -> Result<(), SkillArtifactError> {
        let path = Path::new(&self.path);
        if path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(SkillArtifactError::InvalidPath {
                path: self.path.clone(),
            });
        }

        Ok(())
    }
}

/// Full artifact bundle for a skillify candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillArtifactBundle {
    /// Candidate id this bundle belongs to.
    pub candidate_id: String,
    /// Bundle schema version.
    pub version: u32,
    /// Generated artifacts.
    pub artifacts: Vec<SkillArtifact>,
}

impl SkillArtifactBundle {
    /// Validate artifact paths and required artifact coverage.
    ///
    /// # Errors
    /// Returns [`SkillArtifactError`] when a path escapes the bundle or a
    /// required artifact kind is missing.
    pub fn validate(&self) -> Result<(), SkillArtifactError> {
        for artifact in &self.artifacts {
            artifact.validate_path()?;
        }

        for kind in SkillArtifactKind::required() {
            if !self.artifacts.iter().any(|artifact| artifact.kind == *kind) {
                return Err(SkillArtifactError::MissingArtifact { kind: *kind });
            }
        }

        Ok(())
    }
}

/// Artifact bundle validation error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SkillArtifactError {
    /// A required artifact kind was absent.
    #[error("skillify bundle missing artifact {kind}")]
    MissingArtifact {
        /// Missing artifact kind.
        kind: SkillArtifactKind,
    },
    /// Artifact path escaped the candidate bundle.
    #[error("skillify artifact invalid path `{path}`: path must stay inside the candidate bundle")]
    InvalidPath {
        /// Rejected artifact path.
        path: String,
    },
}
