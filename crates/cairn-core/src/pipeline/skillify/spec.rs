//! Skillify spec draft — STAGE 1 extraction output.

use serde::{Deserialize, Serialize};

/// Error from [`SkillSpecDraft::validate`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SkillSpecError {
    /// A required field was empty.
    #[error("skill spec field `{field}` must not be empty")]
    EmptyField {
        /// Field name.
        field: &'static str,
    },
    /// Slug contained unsafe characters.
    #[error("skill spec slug `{slug}` is not a safe path token")]
    InvalidSlug {
        /// Rejected slug.
        slug: String,
    },
}

/// Extracted skill specification from a conversation trace (STAGE 1 output).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillSpecDraft {
    /// Skill lane, e.g. `deploy.hotfix`.
    pub lane: String,
    /// Filesystem-safe slug.
    pub slug: String,
    /// Decision tree extracted from the trace.
    pub decision_tree: serde_json::Value,
    /// Natural-language triggers.
    pub triggers: Vec<String>,
    /// Criteria that made the trajectory successful.
    pub success_criteria: Vec<String>,
    /// Source record ids.
    pub source_refs: Vec<String>,
    /// Required capabilities.
    pub requires: Vec<String>,
    /// Capabilities this skill provides.
    pub provides: Vec<String>,
}

impl SkillSpecDraft {
    /// Validate required fields and slug safety.
    ///
    /// # Errors
    /// Returns [`SkillSpecError`] when a required field is empty or the slug
    /// contains unsafe characters.
    pub fn validate(&self) -> Result<(), SkillSpecError> {
        validate_not_empty("lane", &self.lane)?;
        validate_slug(&self.slug)?;
        validate_vec_not_empty("triggers", &self.triggers)?;
        validate_vec_not_empty("source_refs", &self.source_refs)?;
        Ok(())
    }
}

fn validate_not_empty(field: &'static str, value: &str) -> Result<(), SkillSpecError> {
    if value.trim().is_empty() {
        Err(SkillSpecError::EmptyField { field })
    } else {
        Ok(())
    }
}

fn validate_vec_not_empty(field: &'static str, value: &[String]) -> Result<(), SkillSpecError> {
    if value.is_empty() {
        Err(SkillSpecError::EmptyField { field })
    } else {
        Ok(())
    }
}

fn validate_slug(slug: &str) -> Result<(), SkillSpecError> {
    if slug.is_empty()
        || slug == "."
        || slug == ".."
        || slug.contains('/')
        || slug.contains('\\')
        || !slug
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err(SkillSpecError::InvalidSlug {
            slug: slug.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_draft() -> SkillSpecDraft {
        SkillSpecDraft {
            lane: "deploy.hotfix".to_owned(),
            slug: "deploy-hotfix".to_owned(),
            decision_tree: serde_json::json!({"root": "check_env"}),
            triggers: vec!["deploy hotfix".to_owned()],
            success_criteria: vec!["script exits 0".to_owned()],
            source_refs: vec!["01HQZX9F5N0000000000000001".to_owned()],
            requires: vec![],
            provides: vec!["deploy.hotfix".to_owned()],
        }
    }

    #[test]
    fn valid_draft_passes() {
        assert!(valid_draft().validate().is_ok());
    }

    #[test]
    fn empty_lane_rejected() {
        let mut draft = valid_draft();
        draft.lane = String::new();
        let err = draft.validate().unwrap_err();
        assert!(matches!(err, SkillSpecError::EmptyField { .. }));
    }

    #[test]
    fn unsafe_slug_rejected() {
        let mut draft = valid_draft();
        draft.slug = "../escape".to_owned();
        let err = draft.validate().unwrap_err();
        assert!(matches!(err, SkillSpecError::InvalidSlug { .. }));
    }

    #[test]
    fn empty_triggers_rejected() {
        let mut draft = valid_draft();
        draft.triggers.clear();
        let err = draft.validate().unwrap_err();
        assert!(matches!(err, SkillSpecError::EmptyField { .. }));
    }

    #[test]
    fn empty_source_refs_rejected() {
        let mut draft = valid_draft();
        draft.source_refs.clear();
        let err = draft.validate().unwrap_err();
        assert!(matches!(err, SkillSpecError::EmptyField { .. }));
    }

    #[test]
    fn serde_round_trip() {
        let draft = valid_draft();
        let json = serde_json::to_string(&draft).unwrap();
        let parsed: SkillSpecDraft = serde_json::from_str(&json).unwrap();
        assert_eq!(draft, parsed);
    }
}
