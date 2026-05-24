//! Threshold manifest loader + gate evaluator.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use cairn_test_fixtures::replay::MetricCategory;
use serde::{Deserialize, Serialize};

use super::category::ALL as ALL_CATEGORIES;
use super::score::{CategoryScore, CategoryScores};

/// Gate mode selected on the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateMode {
    /// Record only, never fail.
    None,
    /// Beta gate — uses `beta_min`.
    Beta,
    /// Release-candidate gate — uses `rc_min`.
    Rc,
}

/// Per-category threshold row.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct CategoryThreshold {
    /// Minimum score required to pass `--gate beta`.
    pub beta_min: f64,
    /// Minimum score required to pass `--gate rc`.
    pub rc_min: f64,
    /// Maximum allowed regression from the committed baseline, in percentage
    /// points (e.g. `2.0` means fail if `(baseline - current) > 0.02`).
    pub max_drop_pct: f64,
}

/// Threshold manifest (matches `manifests/coherence.toml`).
#[derive(Debug, Clone, Deserialize)]
pub struct ThresholdManifest {
    /// Schema version. Currently only `1` is supported.
    pub schema_version: u32,
    /// Recall-precision row.
    pub recall_precision: CategoryThreshold,
    /// Stale-avoidance row.
    pub stale_avoidance: CategoryThreshold,
    /// Summary-quality row.
    pub summary_quality: CategoryThreshold,
    /// Search-usefulness row.
    pub search_usefulness: CategoryThreshold,
    /// Forget-completeness row.
    pub forget_completeness: CategoryThreshold,
}

impl ThresholdManifest {
    /// Look up the threshold row for one category.
    #[must_use]
    pub fn for_category(&self, category: MetricCategory) -> CategoryThreshold {
        match category {
            MetricCategory::RecallPrecision => self.recall_precision,
            MetricCategory::StaleAvoidance => self.stale_avoidance,
            MetricCategory::SummaryQuality => self.summary_quality,
            MetricCategory::SearchUsefulness => self.search_usefulness,
            MetricCategory::ForgetCompleteness => self.forget_completeness,
        }
    }
}

/// Prior-run baseline, used for the delta-regression check.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Baseline {
    /// Schema version. Currently only `1` is supported.
    pub schema_version: u32,
    /// RFC-3339 timestamp of capture.
    pub captured_at: String,
    /// Cairn version that produced this baseline.
    pub cairn_version: String,
    /// Git SHA of the commit at capture.
    pub git_sha: String,
    /// Per-category scores, keyed by the wire-string name
    /// (e.g. `"recall_precision"`).
    pub metrics: BTreeMap<String, CategoryScore>,
}

impl Baseline {
    /// Look up the recorded score for one category. Returns `None` if the
    /// baseline does not record this category yet.
    #[must_use]
    pub fn score_for(&self, category: MetricCategory) -> Option<CategoryScore> {
        self.metrics.get(super::category::as_str(category)).copied()
    }
}

/// Outcome of evaluating one metric against the gate.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum MetricOutcome {
    /// Metric is at or above the floor and within the regression delta.
    Pass,
    /// Metric score fell below the configured floor.
    BelowFloor {
        /// The floor that was applied (`beta_min` or `rc_min`).
        floor: f64,
    },
    /// Metric regressed more than `max_drop_pct` from the baseline.
    ExceededDrop {
        /// Baseline score (the prior value).
        previous: f64,
        /// `(previous - current) * 100`, in percentage points.
        drop_pct: f64,
    },
    /// Gate was set to `None` — outcome is not enforced.
    GateNone,
}

impl MetricOutcome {
    /// `true` for `Pass` and `GateNone`.
    #[must_use]
    pub const fn is_pass(self) -> bool {
        matches!(self, Self::Pass | Self::GateNone)
    }
}

/// Per-category result row in the final gate outcome.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct MetricResult {
    /// The computed score for this category.
    pub score: CategoryScore,
    /// Per-metric outcome under the configured gate.
    pub outcome: MetricOutcome,
    /// Signed delta vs. baseline (`current - previous`). `None` when no
    /// baseline was provided.
    pub delta: Option<f64>,
}

/// Errors arising from loading or evaluating the gate.
#[derive(Debug, thiserror::Error)]
pub enum ThresholdError {
    /// Filesystem read of the manifest failed.
    #[error("read {path}: {source}")]
    Io {
        /// Path that failed.
        path: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// TOML parse of the manifest failed.
    #[error("parse {path}: {source}")]
    Toml {
        /// Path that failed.
        path: String,
        /// Underlying parse error.
        #[source]
        source: toml::de::Error,
    },
    /// Manifest declared a `schema_version` this build does not understand.
    #[error("unsupported manifest schema_version {version}")]
    UnsupportedManifestVersion {
        /// The unsupported version number.
        version: u32,
    },
}

/// Load a `coherence.toml` manifest from disk.
///
/// # Errors
/// - `Io` if the file cannot be read.
/// - `Toml` if the file is malformed.
/// - `UnsupportedManifestVersion` if `schema_version` is not 1.
pub fn load_manifest(path: &Path) -> Result<ThresholdManifest, ThresholdError> {
    let raw = fs::read_to_string(path).map_err(|source| ThresholdError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let manifest: ThresholdManifest =
        toml::from_str(&raw).map_err(|source| ThresholdError::Toml {
            path: path.display().to_string(),
            source,
        })?;
    if manifest.schema_version != 1 {
        return Err(ThresholdError::UnsupportedManifestVersion {
            version: manifest.schema_version,
        });
    }
    Ok(manifest)
}

/// Evaluate the gate for all five categories.
///
/// Returns a map of per-category outcomes. `baseline` is `None` on the
/// first run; in that case only the floor check applies (no delta).
#[must_use]
pub fn evaluate(
    mode: GateMode,
    scores: &CategoryScores,
    manifest: &ThresholdManifest,
    baseline: Option<&Baseline>,
) -> BTreeMap<MetricCategory, MetricResult> {
    let mut out = BTreeMap::new();
    for category in ALL_CATEGORIES {
        let score = scores
            .get(&category)
            .copied()
            .unwrap_or_else(CategoryScore::empty);
        let threshold = manifest.for_category(category);
        let previous = baseline
            .and_then(|b| b.score_for(category))
            .map(|s| s.score);
        let delta = previous.map(|prev| score.score - prev);
        let outcome = evaluate_one(mode, score.score, threshold, previous);
        out.insert(
            category,
            MetricResult {
                score,
                outcome,
                delta,
            },
        );
    }
    out
}

fn evaluate_one(
    mode: GateMode,
    score: f64,
    threshold: CategoryThreshold,
    previous: Option<f64>,
) -> MetricOutcome {
    let floor = match mode {
        GateMode::None => return MetricOutcome::GateNone,
        GateMode::Beta => threshold.beta_min,
        GateMode::Rc => threshold.rc_min,
    };
    if score < floor {
        return MetricOutcome::BelowFloor { floor };
    }
    if let Some(prev) = previous {
        let drop_pct = (prev - score) * 100.0;
        if drop_pct > threshold.max_drop_pct {
            return MetricOutcome::ExceededDrop {
                previous: prev,
                drop_pct,
            };
        }
    }
    MetricOutcome::Pass
}

/// True if every metric passed (or the gate was `None`).
#[must_use]
pub fn all_pass(results: &BTreeMap<MetricCategory, MetricResult>) -> bool {
    results.values().all(|r| r.outcome.is_pass())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn threshold(beta: f64, rc: f64, drop_pct: f64) -> CategoryThreshold {
        CategoryThreshold {
            beta_min: beta,
            rc_min: rc,
            max_drop_pct: drop_pct,
        }
    }

    #[test]
    fn gate_pass_at_floor() {
        let outcome = evaluate_one(GateMode::Beta, 0.90, threshold(0.90, 0.95, 2.0), None);
        assert_eq!(outcome, MetricOutcome::Pass);
    }

    #[test]
    fn gate_fail_below_floor() {
        let outcome = evaluate_one(GateMode::Beta, 0.89, threshold(0.90, 0.95, 2.0), None);
        assert_eq!(outcome, MetricOutcome::BelowFloor { floor: 0.90 });
    }

    #[test]
    fn gate_fail_on_drop_exceeded() {
        let outcome = evaluate_one(GateMode::Beta, 0.91, threshold(0.90, 0.95, 2.0), Some(0.95));
        match outcome {
            MetricOutcome::ExceededDrop { previous, drop_pct } => {
                assert!((previous - 0.95).abs() < f64::EPSILON);
                assert!((drop_pct - 4.0).abs() < 1e-9);
            }
            other => panic!("expected ExceededDrop, got {other:?}"),
        }
    }

    #[test]
    fn gate_skips_delta_without_baseline() {
        let outcome = evaluate_one(GateMode::Beta, 0.91, threshold(0.90, 0.95, 2.0), None);
        assert_eq!(outcome, MetricOutcome::Pass);
    }

    #[test]
    fn gate_none_never_fails() {
        let outcome = evaluate_one(GateMode::None, 0.0, threshold(0.90, 0.95, 2.0), Some(1.0));
        assert_eq!(outcome, MetricOutcome::GateNone);
    }

    #[test]
    fn forget_completeness_intolerant_under_both_gates() {
        let t = threshold(1.0, 1.0, 0.0);
        assert!(matches!(
            evaluate_one(GateMode::Beta, 0.999, t, None),
            MetricOutcome::BelowFloor { .. }
        ));
        assert!(matches!(
            evaluate_one(GateMode::Rc, 0.999, t, None),
            MetricOutcome::BelowFloor { .. }
        ));
    }
}
