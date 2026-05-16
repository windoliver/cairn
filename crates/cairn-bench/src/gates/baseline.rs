//! Shared baseline JSON load/save for the latency gate.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// A snapshot of benchmark metrics used as a regression baseline.
///
/// Serialised as JSON; the file is written by the `gates capture` subcommand
/// and read by `gates latency` to detect regressions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Baseline {
    /// Monotonically increasing schema version; currently `1`.
    pub schema_version: u32,
    /// Runner profile name (e.g. `"linux"`, `"macos"`).
    pub runner: String,
    /// ISO-8601 timestamp at which the baseline was captured.
    pub captured_at: String,
    /// Git commit SHA the baseline was measured at.
    pub commit: String,
    /// Metric name → value in milliseconds.
    pub metrics: BTreeMap<String, f64>,
    /// Per-metric regression-threshold overrides (percentage, e.g. `5.0` = 5%).
    /// If absent for a metric, [`crate::gates::thresholds::DEFAULT_REGRESSION_PCT`] applies.
    #[serde(default)]
    pub regression_pct: BTreeMap<String, f64>,
}

/// Errors returned by [`Baseline::load`] and [`Baseline::save`].
#[derive(Debug, thiserror::Error)]
pub enum BaselineError {
    /// The baseline file does not exist at the given path.
    #[error("baseline file not found: {0}")]
    NotFound(String),
    /// The file exists but its JSON could not be parsed.
    #[error("baseline parse error in {path}: {source}")]
    Parse {
        /// Path to the file that could not be parsed.
        path: String,
        /// Underlying JSON error.
        #[source]
        source: serde_json::Error,
    },
    /// The baseline could not be written to disk.
    #[error("baseline write error in {path}: {source}")]
    Write {
        /// Path to the file that could not be written.
        path: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

impl Baseline {
    /// Load a [`Baseline`] from a JSON file on disk.
    pub fn load(path: &Path) -> Result<Self, BaselineError> {
        let bytes = std::fs::read(path)
            .map_err(|_| BaselineError::NotFound(path.display().to_string()))?;
        serde_json::from_slice(&bytes).map_err(|source| BaselineError::Parse {
            path: path.display().to_string(),
            source,
        })
    }

    /// Serialise this baseline as pretty-printed JSON and write it to `path`.
    pub fn save(&self, path: &Path) -> Result<(), BaselineError> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|source| BaselineError::Parse {
            path: path.display().to_string(),
            source,
        })?;
        std::fs::write(path, bytes).map_err(|source| BaselineError::Write {
            path: path.display().to_string(),
            source,
        })
    }

    /// Return the regression threshold (in milliseconds) for `metric`.
    ///
    /// Uses the per-metric override from [`Self::regression_pct`] if present;
    /// otherwise falls back to [`crate::gates::thresholds::DEFAULT_REGRESSION_PCT`].
    #[must_use]
    pub fn regression_threshold_ms(&self, metric: &str) -> f64 {
        let baseline_ms = self.metrics.get(metric).copied().unwrap_or(f64::INFINITY);
        let pct = self
            .regression_pct
            .get(metric)
            .copied()
            .unwrap_or(crate::gates::thresholds::DEFAULT_REGRESSION_PCT);
        baseline_ms * (1.0 + pct / 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_writes_and_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.json");
        let mut b = Baseline {
            schema_version: 1,
            runner: "ubuntu".into(),
            captured_at: "2026-05-15T00:00:00Z".into(),
            commit: "abc".into(),
            metrics: BTreeMap::new(),
            regression_pct: BTreeMap::new(),
        };
        b.metrics.insert("assemble_hot_p95_ms".into(), 4.2);
        b.save(&path).unwrap();
        let got = Baseline::load(&path).unwrap();
        assert_eq!(b, got);
    }

    #[test]
    fn missing_file_returns_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let err = Baseline::load(&dir.path().join("nope.json")).unwrap_err();
        assert!(matches!(err, BaselineError::NotFound(_)));
    }

    #[test]
    fn default_regression_is_two_pct() {
        let b = Baseline {
            schema_version: 1,
            runner: "x".into(),
            captured_at: "x".into(),
            commit: "x".into(),
            metrics: [("m".to_string(), 100.0)].into_iter().collect(),
            regression_pct: BTreeMap::new(),
        };
        let t = b.regression_threshold_ms("m");
        assert!((t - 102.0).abs() < 1e-9, "expected 102 ms, got {t}");
    }

    #[test]
    fn per_metric_override_wins() {
        let b = Baseline {
            schema_version: 1,
            runner: "x".into(),
            captured_at: "x".into(),
            commit: "x".into(),
            metrics: [("m".to_string(), 100.0)].into_iter().collect(),
            regression_pct: [("m".to_string(), 5.0)].into_iter().collect(),
        };
        let t = b.regression_threshold_ms("m");
        assert!((t - 105.0).abs() < 1e-9, "expected 105 ms, got {t}");
    }
}
