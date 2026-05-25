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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
    /// Manifest parsed but violated a content invariant (non-finite
    /// threshold, out-of-range floor or drop, `beta_min > rc_min`, etc.).
    #[error("manifest {path}: {reason}")]
    Invalid {
        /// Path that failed.
        path: String,
        /// Human-readable invariant that was violated.
        reason: String,
    },
}

/// Load a `coherence.toml` manifest from disk.
///
/// # Errors
/// - `Io` if the file cannot be read.
/// - `Toml` if the file is malformed.
/// - `UnsupportedManifestVersion` if `schema_version` is not 1.
/// - `Invalid` if any threshold value violates a runtime invariant
///   (non-finite, out of range, or `beta_min > rc_min`). These checks
///   complement the JSON schema, which can't catch NaN/infinity from
///   TOML and isn't enforced at runtime.
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
    validate_manifest(&manifest).map_err(|reason| ThresholdError::Invalid {
        path: path.display().to_string(),
        reason,
    })?;
    Ok(manifest)
}

/// Reject thresholds that would silently weaken the gate.
///
/// TOML supports `nan`/`inf` literals; if those land in `beta_min` or
/// `rc_min`, the floor check (`score < floor`) is always false because
/// every NaN comparison is false. Same hazard for negative floors or
/// huge `max_drop_pct`. Each violation maps to `ThresholdError::Invalid`
/// (exit 78).
fn validate_manifest(manifest: &ThresholdManifest) -> Result<(), String> {
    let rows: [(&str, CategoryThreshold); 5] = [
        ("recall_precision", manifest.recall_precision),
        ("stale_avoidance", manifest.stale_avoidance),
        ("summary_quality", manifest.summary_quality),
        ("search_usefulness", manifest.search_usefulness),
        ("forget_completeness", manifest.forget_completeness),
    ];
    for (name, t) in rows {
        check_floor(name, "beta_min", t.beta_min)?;
        check_floor(name, "rc_min", t.rc_min)?;
        check_drop(name, t.max_drop_pct)?;
        if t.beta_min > t.rc_min {
            return Err(format!(
                "{name}: beta_min ({}) > rc_min ({}) (rc gate must be at least as strict as beta)",
                t.beta_min, t.rc_min
            ));
        }
    }
    Ok(())
}

fn check_floor(metric: &str, field: &str, value: f64) -> Result<(), String> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(format!(
            "{metric}: {field} {value} out of [0, 1] or non-finite"
        ));
    }
    Ok(())
}

fn check_drop(metric: &str, value: f64) -> Result<(), String> {
    if !value.is_finite() || !(0.0..=100.0).contains(&value) {
        return Err(format!(
            "{metric}: max_drop_pct {value} out of [0, 100] or non-finite"
        ));
    }
    Ok(())
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
        // Tolerate float rounding in the multiplication. The spec is
        // "fail when the drop *exceeds* the budget"; exact budget hits
        // pass. Without the epsilon, prev=1.0 / score=0.98 produces
        // 2.0000000000000018 and trips a 2.0% budget.
        if drop_pct - threshold.max_drop_pct > DROP_EPSILON {
            return MetricOutcome::ExceededDrop {
                previous: prev,
                drop_pct,
            };
        }
    }
    MetricOutcome::Pass
}

/// Tolerance for the percentage-point drop comparison. Picked an order
/// of magnitude above the worst f64 multiplication error for a 100-scale
/// computation on values in [0, 1].
const DROP_EPSILON: f64 = 1e-9;

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
    fn gate_passes_at_exact_drop_budget() {
        // prev=1.0, score=0.98 -> drop is 2.0% on paper but 2.0000…018
        // in f64. The epsilon tolerance keeps an exact-budget drop on
        // the pass side; only drops *exceeding* the budget should fail.
        let outcome = evaluate_one(GateMode::Beta, 0.98, threshold(0.0, 0.0, 2.0), Some(1.0));
        assert_eq!(
            outcome,
            MetricOutcome::Pass,
            "exact 2.0% drop should pass, not be tripped by float rounding"
        );
    }

    #[test]
    fn gate_fails_just_past_drop_budget() {
        // A drop a hair past the budget should still fail.
        let outcome = evaluate_one(GateMode::Beta, 0.97, threshold(0.0, 0.0, 2.0), Some(1.0));
        assert!(matches!(outcome, MetricOutcome::ExceededDrop { .. }));
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

    fn good_manifest() -> ThresholdManifest {
        ThresholdManifest {
            schema_version: 1,
            recall_precision: threshold(0.9, 0.95, 2.0),
            stale_avoidance: threshold(0.95, 0.98, 2.0),
            summary_quality: threshold(0.85, 0.9, 2.0),
            search_usefulness: threshold(0.85, 0.9, 2.0),
            forget_completeness: threshold(1.0, 1.0, 0.0),
        }
    }

    #[test]
    fn validate_manifest_accepts_canonical_shape() {
        assert!(validate_manifest(&good_manifest()).is_ok());
    }

    #[test]
    fn validate_manifest_rejects_nan_floor() {
        let mut m = good_manifest();
        m.recall_precision.beta_min = f64::NAN;
        let err = validate_manifest(&m).unwrap_err();
        assert!(err.contains("beta_min"), "{err}");
    }

    #[test]
    fn validate_manifest_rejects_inf_drop() {
        let mut m = good_manifest();
        m.stale_avoidance.max_drop_pct = f64::INFINITY;
        let err = validate_manifest(&m).unwrap_err();
        assert!(err.contains("max_drop_pct"), "{err}");
    }

    #[test]
    fn validate_manifest_rejects_negative_floor() {
        let mut m = good_manifest();
        m.summary_quality.rc_min = -0.1;
        let err = validate_manifest(&m).unwrap_err();
        assert!(err.contains("rc_min"), "{err}");
    }

    #[test]
    fn validate_manifest_rejects_floor_above_one() {
        let mut m = good_manifest();
        m.summary_quality.beta_min = 1.5;
        let err = validate_manifest(&m).unwrap_err();
        assert!(err.contains("beta_min"), "{err}");
    }

    #[test]
    fn validate_manifest_rejects_drop_above_100() {
        let mut m = good_manifest();
        m.search_usefulness.max_drop_pct = 101.0;
        let err = validate_manifest(&m).unwrap_err();
        assert!(err.contains("max_drop_pct"), "{err}");
    }

    #[test]
    fn validate_manifest_rejects_beta_greater_than_rc() {
        let mut m = good_manifest();
        m.recall_precision = threshold(0.95, 0.90, 2.0);
        let err = validate_manifest(&m).unwrap_err();
        assert!(err.contains("beta_min") && err.contains("rc_min"), "{err}");
    }
}
