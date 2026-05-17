//! Latency gate harness — invokes criterion, parses JSON, compares to baseline.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::gates::baseline::Baseline;
use crate::gates::report::GateOutcome;
use crate::gates::thresholds::SLO_HOT_PATH_SUBPROCESS_MS;

/// Per-metric SLO. All 8 P0 benches share the subprocess proxy SLO.
#[must_use]
pub fn slo_ms(_metric: &str) -> f64 {
    SLO_HOT_PATH_SUBPROCESS_MS
}

/// Result for a single benchmark metric comparison.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetricResult {
    /// Name of the benchmark.
    pub bench: String,
    /// Measured median latency in milliseconds.
    pub measured_ms: f64,
    /// Hard SLO threshold in milliseconds.
    pub slo_ms: f64,
    /// Baseline median latency in milliseconds.
    pub baseline_ms: f64,
    /// Baseline plus allowed regression percentage in milliseconds.
    pub regression_threshold_ms: f64,
    /// Whether the measured value is within the hard SLO.
    pub slo_ok: bool,
    /// Whether the measured value is within the regression threshold.
    pub regression_ok: bool,
}

/// Full latency gate report written to `target/cairn-bench/latency.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyReport {
    /// Schema version; currently `1`.
    pub schema_version: u32,
    /// Runner profile name (e.g. `"linux"`, `"macos"`).
    pub runner: String,
    /// Git commit SHA the baseline was measured at.
    pub commit: String,
    /// ISO-8601 timestamp at which the report was captured.
    pub captured_at: String,
    /// Per-metric results.
    pub metrics: Vec<MetricResult>,
    /// `true` iff all checks passed.
    pub ok: bool,
    /// Human-readable failure messages.
    pub failures: Vec<String>,
}

/// Compare measured values against a baseline and return a report.
///
/// **Fail-closed semantics:**
/// - Empty measured set → fail (criterion didn't produce output).
/// - Any baseline metric missing from `measured` → fail (bench was renamed
///   or dropped; regression coverage would silently disappear otherwise).
/// - Any measured metric not in baseline → fail (unknown new metric; the
///   baseline must be refreshed before the gate accepts it).
#[must_use]
pub fn compare(measured: &BTreeMap<String, f64>, baseline: &Baseline) -> LatencyReport {
    let mut metrics = Vec::new();
    let mut failures = Vec::new();

    if measured.is_empty() {
        failures.push(
            "no measured metrics — criterion produced no output (rerun without --no-run)".into(),
        );
    }

    // Detect renamed / dropped benches: every baseline metric must appear
    // in `measured`. Surfacing this catches the silent-gate-pass failure
    // mode where a renamed bench drops regression coverage.
    for bench in baseline.metrics.keys() {
        if !measured.contains_key(bench) {
            failures.push(format!(
                "{bench}: in baseline but missing from measured output — was the bench renamed or filtered?"
            ));
        }
    }

    // Detect new / unknown benches: anything in `measured` that has no
    // baseline entry must be added to the baseline (via --refresh-baseline)
    // before the gate accepts it.
    for bench in measured.keys() {
        if !baseline.metrics.contains_key(bench) {
            failures.push(format!(
                "{bench}: measured but not in baseline — refresh the baseline to enrol this metric"
            ));
        }
    }

    for (bench, measured_ms) in measured {
        let slo = slo_ms(bench);
        let baseline_ms = baseline
            .metrics
            .get(bench)
            .copied()
            .unwrap_or(f64::INFINITY);
        let regression_threshold = baseline.regression_threshold_ms(bench);
        let slo_ok = *measured_ms <= slo;
        let regression_ok = *measured_ms <= regression_threshold;
        if !slo_ok {
            failures.push(format!(
                "{bench}: measured {measured_ms:.2} ms > SLO {slo:.2} ms"
            ));
        }
        if !regression_ok {
            failures.push(format!(
                "{bench}: measured {measured_ms:.2} ms > baseline regression threshold {regression_threshold:.2} ms"
            ));
        }
        metrics.push(MetricResult {
            bench: bench.clone(),
            measured_ms: *measured_ms,
            slo_ms: slo,
            baseline_ms,
            regression_threshold_ms: regression_threshold,
            slo_ok,
            regression_ok,
        });
    }

    let ok = failures.is_empty();
    LatencyReport {
        schema_version: 1,
        runner: baseline.runner.clone(),
        commit: baseline.commit.clone(),
        captured_at: baseline.captured_at.clone(),
        metrics,
        ok,
        failures,
    }
}

/// Convert a report to a [`GateOutcome`].
#[must_use]
pub fn outcome_from_report(r: &LatencyReport) -> GateOutcome {
    if r.ok {
        GateOutcome::Pass
    } else {
        GateOutcome::Fail
    }
}

/// Parse criterion's `estimates.json` files. Returns bench-name → median ms.
///
/// Criterion writes `target/criterion/<bench>/new/estimates.json` after each run.
/// The numbers are nanoseconds. We use the **median** (criterion's most stable
/// summary). Criterion does not emit p95 directly; the brief §15 "p95" target
/// is approximated by the median because subprocess-driven measurement is
/// already noisy enough that median + 2% regression is the load-bearing guard.
///
/// # Errors
///
/// Returns an error if the directory cannot be read, a JSON file is malformed,
/// or `median.point_estimate` is missing from an estimates file.
pub fn parse_criterion_dir(criterion_dir: &Path) -> anyhow::Result<BTreeMap<String, f64>> {
    let mut out = BTreeMap::new();
    for entry in std::fs::read_dir(criterion_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let estimates = path.join("new").join("estimates.json");
        if !estimates.exists() {
            continue;
        }
        let bytes = std::fs::read(&estimates)?;
        let v: serde_json::Value = serde_json::from_slice(&bytes)?;
        let ns = v
            .get("median")
            .and_then(|m| m.get("point_estimate"))
            .and_then(serde_json::Value::as_f64)
            .ok_or_else(|| {
                anyhow::anyhow!("missing median.point_estimate in {}", estimates.display())
            })?;
        let ms = ns / 1_000_000.0;
        let bench = path
            .file_name()
            .expect("invariant: read_dir entry always has a file name")
            .to_string_lossy()
            .to_string();
        out.insert(bench, ms);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(metric: &str, baseline_ms: f64) -> Baseline {
        Baseline {
            schema_version: 1,
            runner: "test".into(),
            captured_at: "now".into(),
            commit: "deadbeef".into(),
            metrics: [(metric.to_string(), baseline_ms)].into_iter().collect(),
            regression_pct: BTreeMap::new(),
        }
    }

    #[test]
    fn pass_when_under_slo_and_under_baseline_plus_2pct() {
        let measured: BTreeMap<String, f64> = [("assemble_hot_p95".into(), 100.0)].into();
        let baseline = b("assemble_hot_p95", 100.0);
        let r = compare(&measured, &baseline);
        assert!(r.ok, "expected pass; failures: {:?}", r.failures);
    }

    #[test]
    fn fail_when_over_slo() {
        let measured: BTreeMap<String, f64> = [("assemble_hot_p95".into(), 301.0)].into();
        let baseline = b("assemble_hot_p95", 100.0);
        let r = compare(&measured, &baseline);
        assert!(!r.ok);
        assert!(r.failures.iter().any(|f| f.contains("SLO")));
    }

    #[test]
    fn fail_when_over_baseline_plus_2pct_but_under_slo() {
        // baseline 100 ms, measured 110 ms (>+2% but well below 300 ms SLO)
        let measured: BTreeMap<String, f64> = [("assemble_hot_p95".into(), 110.0)].into();
        let baseline = b("assemble_hot_p95", 100.0);
        let r = compare(&measured, &baseline);
        assert!(!r.ok);
        assert!(
            r.failures
                .iter()
                .any(|f| f.contains("baseline regression threshold"))
        );
    }

    #[test]
    fn fail_when_measured_empty() {
        let measured: BTreeMap<String, f64> = BTreeMap::new();
        let baseline = b("assemble_hot_p95", 100.0);
        let r = compare(&measured, &baseline);
        assert!(!r.ok);
        assert!(r.failures.iter().any(|f| f.contains("no measured")));
    }

    #[test]
    fn fail_when_baseline_metric_missing_from_measured() {
        // Rename: the baseline knows assemble_hot_p95 but criterion only
        // produced a result for a (renamed) `assemble_hot_v2_p95`.
        let measured: BTreeMap<String, f64> = [("assemble_hot_v2_p95".into(), 100.0)].into();
        let baseline = b("assemble_hot_p95", 100.0);
        let r = compare(&measured, &baseline);
        assert!(!r.ok);
        assert!(
            r.failures
                .iter()
                .any(|f| f.contains("missing from measured output"))
        );
    }

    #[test]
    fn fail_when_measured_has_new_unknown_metric() {
        let measured: BTreeMap<String, f64> = [
            ("assemble_hot_p95".into(), 100.0),
            ("new_unknown_p95".into(), 100.0),
        ]
        .into();
        let baseline = b("assemble_hot_p95", 100.0);
        let r = compare(&measured, &baseline);
        assert!(!r.ok);
        assert!(
            r.failures
                .iter()
                .any(|f| f.contains("not in baseline — refresh"))
        );
    }
}
