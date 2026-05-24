//! Aggregate `ReplayReport` outcomes into per-category coherence scores.
//!
//! Inputs: parallel slices of `ReplayAction` (carries `metric_category` +
//! `stale_record_ids`) and `ReplayCheckReport` (carries `actual` Value).
//! Outputs: a `CategoryScores` map keyed by `MetricCategory`.

use std::collections::BTreeMap;

use cairn_test_fixtures::replay::{MetricCategory, ReplayAction, ReplayCheckReport};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::category::ALL as ALL_CATEGORIES;

/// One category's aggregated result over a run.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CategoryScore {
    /// Number of actions tagged into this category whose coherence-pass
    /// condition held.
    pub passed: u32,
    /// Total actions tagged into this category.
    pub total: u32,
    /// `passed / total`, or 1.0 if `total == 0` (vacuous pass per design §5.4).
    pub score: f64,
}

impl CategoryScore {
    /// Vacuous-pass for an empty category.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            passed: 0,
            total: 0,
            score: 1.0,
        }
    }
}

/// All five category scores, keyed in deterministic order.
pub type CategoryScores = BTreeMap<MetricCategory, CategoryScore>;

/// Errors arising from parallel-slice misalignment.
#[derive(Debug, thiserror::Error)]
pub enum ScoreError {
    /// Actions and reports must be the same length and same order.
    #[error("actions ({actions}) and reports ({reports}) have different lengths")]
    LengthMismatch {
        /// Number of actions provided.
        actions: usize,
        /// Number of reports provided.
        reports: usize,
    },
}

/// Aggregate a parallel slice of (action, check report) into per-category
/// scores. Untagged actions are ignored.
///
/// # Errors
/// Returns `ScoreError::LengthMismatch` if the two slices are not the same
/// length.
pub fn aggregate(
    actions: &[ReplayAction],
    reports: &[ReplayCheckReport],
) -> Result<CategoryScores, ScoreError> {
    if actions.len() != reports.len() {
        return Err(ScoreError::LengthMismatch {
            actions: actions.len(),
            reports: reports.len(),
        });
    }

    let mut buckets: BTreeMap<MetricCategory, (u32, u32)> = BTreeMap::new();
    for category in ALL_CATEGORIES {
        buckets.insert(category, (0, 0));
    }

    for (action, report) in actions.iter().zip(reports.iter()) {
        let Some(category) = classify(action) else {
            continue;
        };
        let pass = action_passed(action, report);
        let entry = buckets.entry(category).or_insert((0, 0));
        entry.1 += 1;
        if pass {
            entry.0 += 1;
        }
    }

    Ok(buckets
        .into_iter()
        .map(|(category, (passed, total))| {
            let score = if total == 0 {
                1.0
            } else {
                f64::from(passed) / f64::from(total)
            };
            (
                category,
                CategoryScore {
                    passed,
                    total,
                    score,
                },
            )
        })
        .collect())
}

/// Compute the deterministic category for a given action, applying the
/// stale-avoidance auto-promote rule.
#[must_use]
pub fn classify(action: &ReplayAction) -> Option<MetricCategory> {
    if let ReplayAction::Search(search) = action
        && !search.stale_record_ids.is_empty()
    {
        return Some(MetricCategory::StaleAvoidance);
    }
    explicit_category(action)
}

fn explicit_category(action: &ReplayAction) -> Option<MetricCategory> {
    match action {
        ReplayAction::Search(search) => search.metric_category,
        ReplayAction::AssembleHot { metric_category, .. }
        | ReplayAction::CaptureTrace { metric_category, .. }
        | ReplayAction::Summarize { metric_category, .. }
        | ReplayAction::Lint { metric_category, .. }
        | ReplayAction::RetrieveSession { metric_category, .. }
        | ReplayAction::RetrieveTurn { metric_category, .. }
        | ReplayAction::RecordPresent { metric_category, .. }
        | ReplayAction::ForgetRecord { metric_category, .. } => *metric_category,
    }
}

/// Per-category pass condition.
///
/// `StaleAvoidance` (search-only): the cassette action passed AND the
/// returned `record_ids` are disjoint from `stale_record_ids`.
///
/// Everything else: the cassette action passed (i.e. golden expectation
/// held).
fn action_passed(action: &ReplayAction, report: &ReplayCheckReport) -> bool {
    if let ReplayAction::Search(search) = action
        && !search.stale_record_ids.is_empty()
    {
        return report.passed && disjoint_from_stale(&report.actual, &search.stale_record_ids);
    }
    report.passed
}

fn disjoint_from_stale(actual: &Value, stale_ids: &[String]) -> bool {
    let Some(arr) = actual.get("record_ids").and_then(Value::as_array) else {
        return false;
    };
    let returned: Vec<&str> = arr.iter().filter_map(Value::as_str).collect();
    !stale_ids.iter().any(|id| returned.contains(&id.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_test_fixtures::replay::{ReplayExpectation, ReplaySearchAction, ReplaySearchMode};
    use serde_json::json;

    fn report(scenario: &str, story: &str, verb: &str, passed: bool) -> ReplayCheckReport {
        ReplayCheckReport {
            scenario_id: scenario.to_owned(),
            story: story.to_owned(),
            verb: verb.to_owned(),
            query: None,
            expected: Value::Null,
            actual: Value::Null,
            passed,
            message: None,
        }
    }

    fn summarize_action(category: Option<MetricCategory>) -> ReplayAction {
        ReplayAction::Summarize {
            story: "S".to_owned(),
            session_id: "s".to_owned(),
            expected_record_ids: vec![],
            metric_category: category,
        }
    }

    #[test]
    fn empty_slices_produce_five_vacuous_passes() {
        let scores = aggregate(&[], &[]).expect("aggregate");
        assert_eq!(scores.len(), 5);
        for category in ALL_CATEGORIES {
            let s = scores[&category];
            assert_eq!(s.total, 0);
            assert!((s.score - 1.0).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn untagged_actions_excluded_from_scoring() {
        let actions = vec![summarize_action(None)];
        let reports = vec![report("c", "S", "summarize", true)];
        let scores = aggregate(&actions, &reports).expect("aggregate");
        assert_eq!(scores[&MetricCategory::SummaryQuality].total, 0);
    }

    #[test]
    fn partial_pass_scores_correctly() {
        let actions = vec![
            summarize_action(Some(MetricCategory::SummaryQuality)),
            summarize_action(Some(MetricCategory::SummaryQuality)),
            summarize_action(Some(MetricCategory::SummaryQuality)),
        ];
        let reports = vec![
            report("c", "S", "summarize", true),
            report("c", "S", "summarize", true),
            report("c", "S", "summarize", false),
        ];
        let scores = aggregate(&actions, &reports).expect("aggregate");
        let s = scores[&MetricCategory::SummaryQuality];
        assert_eq!(s.passed, 2);
        assert_eq!(s.total, 3);
        assert!((s.score - (2.0 / 3.0)).abs() < 1e-9);
    }

    #[test]
    fn length_mismatch_returns_error() {
        let err = aggregate(&[summarize_action(None)], &[]).unwrap_err();
        assert!(matches!(err, ScoreError::LengthMismatch { .. }));
    }

    #[test]
    fn stale_avoidance_auto_classifies_search() {
        let action = ReplayAction::Search(ReplaySearchAction {
            story: "stale".to_owned(),
            mode: ReplaySearchMode::Keyword,
            query: "q".to_owned(),
            limit: 5,
            expected: ReplayExpectation::Hits { record_ids: vec![] },
            metric_category: Some(MetricCategory::SearchUsefulness),
            stale_record_ids: vec!["stale-id".to_owned()],
        });
        assert_eq!(classify(&action), Some(MetricCategory::StaleAvoidance));
    }

    #[test]
    fn stale_avoidance_pass_requires_disjoint_and_golden() {
        let action = ReplayAction::Search(ReplaySearchAction {
            story: "stale".to_owned(),
            mode: ReplaySearchMode::Keyword,
            query: "q".to_owned(),
            limit: 5,
            expected: ReplayExpectation::Hits { record_ids: vec![] },
            metric_category: None,
            stale_record_ids: vec!["stale-id".to_owned()],
        });
        let mut r = report("c", "stale", "search", true);
        r.actual = json!({ "record_ids": ["clean-id"] });
        assert!(action_passed(&action, &r));
        r.actual = json!({ "record_ids": ["clean-id", "stale-id"] });
        assert!(!action_passed(&action, &r));
        r.passed = false;
        r.actual = json!({ "record_ids": ["clean-id"] });
        assert!(!action_passed(&action, &r));
    }
}
