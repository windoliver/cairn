//! `EvaluationWorkflow` configuration (issue #91, brief §15).
//!
//! P0 minimum surface: which golden checks run and whether the
//! workflow is allowed to upsert its `report` record + emit
//! `MetricEvent::EvaluationCompleted` lines.

use serde::{Deserialize, Serialize};

/// Typed configuration for the minimum-path `EvaluationWorkflow`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationConfig {
    /// Master switch. When `false` the workflow refuses to enqueue
    /// and `status` does not advertise
    /// `cairn.workflows.v1.evaluation`.
    #[serde(default = "defaults::enabled")]
    pub enabled: bool,

    /// IDs of the golden checks to run. Empty means "all registered
    /// checks". Brief §15 lists orphan-detection and conflict-DAG
    /// scans; the minimum-path PR ships two starter checks
    /// (`orphan`, `tombstone_consistency`).
    #[serde(default)]
    pub checks: Vec<String>,

    /// When `true` the handler upserts a deterministic `report`
    /// MemoryRecord summarising findings. Disabling this leaves the
    /// metrics-only path active — useful for CI dry-runs.
    #[serde(default = "defaults::write_report_record")]
    pub write_report_record: bool,
}

impl Default for EvaluationConfig {
    fn default() -> Self {
        Self {
            enabled: defaults::enabled(),
            checks: Vec::new(),
            write_report_record: defaults::write_report_record(),
        }
    }
}

mod defaults {
    pub const fn enabled() -> bool {
        // Off by P0 default — release gating opts in explicitly.
        false
    }
    pub const fn write_report_record() -> bool {
        true
    }
}

/// Validation errors raised by [`EvaluationConfig::validate`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EvaluationConfigError {
    /// `checks` contained an empty string.
    #[error("evaluation.checks[{index}] is an empty string")]
    EmptyCheckId {
        /// Index of the offending entry.
        index: usize,
    },
    /// `checks` contained a duplicate.
    #[error("evaluation.checks contains duplicate {id:?}")]
    DuplicateCheckId {
        /// The duplicated check id.
        id: String,
    },
}

impl EvaluationConfig {
    /// Validate semantic invariants the serde layer cannot express.
    ///
    /// # Errors
    /// Returns the first violated invariant.
    pub fn validate(&self) -> Result<(), EvaluationConfigError> {
        for (index, id) in self.checks.iter().enumerate() {
            if id.is_empty() {
                return Err(EvaluationConfigError::EmptyCheckId { index });
            }
        }
        let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for id in &self.checks {
            if !seen.insert(id.as_str()) {
                return Err(EvaluationConfigError::DuplicateCheckId { id: id.clone() });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_brief_p0() {
        let cfg = EvaluationConfig::default();
        assert!(!cfg.enabled);
        assert!(cfg.checks.is_empty());
        assert!(cfg.write_report_record);
    }

    #[test]
    fn rejects_empty_check_id() {
        let cfg = EvaluationConfig {
            checks: vec!["orphan".into(), String::new()],
            ..EvaluationConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(EvaluationConfigError::EmptyCheckId { index: 1 })
        ));
    }

    #[test]
    fn rejects_duplicate_check_id() {
        let cfg = EvaluationConfig {
            checks: vec!["orphan".into(), "orphan".into()],
            ..EvaluationConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(EvaluationConfigError::DuplicateCheckId { .. })
        ));
    }

    #[test]
    fn empty_checks_list_validates() {
        let cfg = EvaluationConfig::default();
        assert!(cfg.validate().is_ok());
    }
}
