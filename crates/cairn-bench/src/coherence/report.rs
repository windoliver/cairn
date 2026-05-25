//! Human + JSON rendering for coherence gate results.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use cairn_test_fixtures::replay::MetricCategory;
use serde::Serialize;
use serde_json::json;

use super::category::{ALL as ALL_CATEGORIES, as_str};
use super::score::CategoryScore;
use super::threshold::{GateMode, MetricOutcome, MetricResult};

/// Compact one-shot summary of a gate run, suitable for serialising to
/// JSON or appending into the trend file.
#[derive(Debug, Clone, Serialize)]
pub struct GateReport {
    /// Schema version stamped on the report.
    pub schema_version: u32,
    /// Gate mode label (`"beta"`, `"rc"`, or `"none"`).
    pub gate: &'static str,
    /// Overall outcome (`"pass"` or `"fail"`).
    pub outcome: &'static str,
    /// Wire-string names of failing categories (empty on pass).
    pub failures: Vec<&'static str>,
    /// Per-category scores, keyed by wire-string name.
    pub metrics: BTreeMap<String, CategoryScore>,
    /// Per-category deltas vs. baseline (`None` if no baseline was provided).
    pub deltas: BTreeMap<String, Option<f64>>,
    /// Mean of the five category scores. For trend display only.
    pub overall: f64,
    /// Number of cassettes run.
    pub cassettes: u32,
    /// Number of actions (across all cassettes) scored.
    pub actions: u32,
}

/// Build a `GateReport` from per-metric results.
#[must_use]
pub fn build(
    mode: GateMode,
    results: &BTreeMap<MetricCategory, MetricResult>,
    cassettes: u32,
    actions: u32,
) -> GateReport {
    let mut metrics: BTreeMap<String, CategoryScore> = BTreeMap::new();
    let mut deltas: BTreeMap<String, Option<f64>> = BTreeMap::new();
    let mut failures: Vec<&'static str> = Vec::new();
    let mut sum = 0.0_f64;
    for category in ALL_CATEGORIES {
        let result = results.get(&category).copied().unwrap_or(MetricResult {
            score: CategoryScore::empty(),
            outcome: MetricOutcome::Pass,
            delta: None,
        });
        let label = as_str(category);
        metrics.insert(label.to_owned(), result.score);
        deltas.insert(label.to_owned(), result.delta);
        if !result.outcome.is_pass() {
            failures.push(label);
        }
        sum += result.score.score;
    }
    let outcome = if failures.is_empty() { "pass" } else { "fail" };
    GateReport {
        schema_version: 1,
        gate: gate_label(mode),
        outcome,
        failures,
        metrics,
        deltas,
        overall: sum / 5.0,
        cassettes,
        actions,
    }
}

const fn gate_label(mode: GateMode) -> &'static str {
    match mode {
        GateMode::None => "none",
        GateMode::Beta => "beta",
        GateMode::Rc => "rc",
    }
}

/// Render a `GateReport` as a fixed-width human-readable table.
#[must_use]
pub fn render_human(
    report: &GateReport,
    results: &BTreeMap<MetricCategory, MetricResult>,
) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "coherence gate={}  cassettes={}  actions={}",
        report.gate, report.cassettes, report.actions,
    );
    for category in ALL_CATEGORIES {
        let label = as_str(category);
        let result = results.get(&category).copied().unwrap_or(MetricResult {
            score: CategoryScore::empty(),
            outcome: MetricOutcome::Pass,
            delta: None,
        });
        let verdict = if result.outcome.is_pass() {
            "pass"
        } else {
            "fail"
        };
        let delta = result
            .delta
            .map_or_else(|| "  n/a ".to_owned(), |d| format!("{d:+.3}"));
        let _ = writeln!(
            out,
            "  {label:<22} {:.3}  {verdict}  ({}/{})   Δ={}",
            result.score.score, result.score.passed, result.score.total, delta,
        );
    }
    let verdict = if report.outcome == "pass" {
        "PASS"
    } else {
        "FAIL"
    };
    let _ = writeln!(
        out,
        "  overall                {:.3}  {verdict}",
        report.overall
    );
    out
}

/// Render a `GateReport` as a JSON Value for `--json` output.
#[must_use]
pub fn render_json(report: &GateReport, trend_path: &str, run_id: &str) -> serde_json::Value {
    json!({
        "schema_version": report.schema_version,
        "gate": report.gate,
        "outcome": report.outcome,
        "failures": report.failures,
        "metrics": report.metrics,
        "deltas": report.deltas,
        "overall": report.overall,
        "cassettes": report.cassettes,
        "actions": report.actions,
        "trend_path": trend_path,
        "run_id": run_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_results() -> BTreeMap<MetricCategory, MetricResult> {
        let mut m = BTreeMap::new();
        for (category, passed, total, delta) in [
            (MetricCategory::RecallPrecision, 9, 9, Some(0.005)),
            (MetricCategory::StaleAvoidance, 3, 3, Some(0.0)),
            (MetricCategory::SummaryQuality, 3, 3, Some(0.02)),
            (MetricCategory::SearchUsefulness, 3, 3, Some(-0.005)),
            (MetricCategory::ForgetCompleteness, 3, 3, Some(0.0)),
        ] {
            m.insert(
                category,
                MetricResult {
                    score: CategoryScore {
                        passed,
                        total,
                        score: f64::from(passed) / f64::from(total),
                    },
                    outcome: MetricOutcome::Pass,
                    delta,
                },
            );
        }
        m
    }

    #[test]
    fn build_marks_pass_when_no_failures() {
        let results = fixture_results();
        let report = build(GateMode::Beta, &results, 3, 21);
        assert_eq!(report.outcome, "pass");
        assert!(report.failures.is_empty());
        assert!((report.overall - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn build_marks_fail_when_any_category_fails() {
        let mut results = fixture_results();
        results.insert(
            MetricCategory::SearchUsefulness,
            MetricResult {
                score: CategoryScore {
                    passed: 1,
                    total: 3,
                    score: 1.0 / 3.0,
                },
                outcome: MetricOutcome::BelowFloor { floor: 0.85 },
                delta: Some(-0.5),
            },
        );
        let report = build(GateMode::Beta, &results, 3, 21);
        assert_eq!(report.outcome, "fail");
        assert_eq!(report.failures, vec!["search_usefulness"]);
    }

    #[test]
    fn human_output_snapshot() {
        let results = fixture_results();
        let report = build(GateMode::Beta, &results, 3, 21);
        let rendered = render_human(&report, &results);
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn json_output_snapshot() {
        let results = fixture_results();
        let report = build(GateMode::Beta, &results, 3, 21);
        let rendered = render_json(
            &report,
            "crates/cairn-bench/baselines/coherence-trend.jsonl",
            "01J000000000000000000000RUN",
        );
        insta::assert_json_snapshot!(rendered);
    }
}
