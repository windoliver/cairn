//! Re-exports `MetricCategory` from `cairn-test-fixtures` and adds a
//! `Display` impl used by the coherence report renderer.
//!
//! The enum itself lives in the replay schema so cassette authors can tag
//! actions without depending on `cairn-bench`. Keeping the canonical
//! definition there means the schema (`fixtures/v0/replay/*.json`) and the
//! scorer share one source of truth.

use std::fmt;

pub use cairn_test_fixtures::replay::MetricCategory;

/// All five categories in display order. Used by the report renderer to
/// iterate deterministically and by the smoke test to assert coverage.
pub const ALL: [MetricCategory; 5] = [
    MetricCategory::RecallPrecision,
    MetricCategory::StaleAvoidance,
    MetricCategory::SummaryQuality,
    MetricCategory::SearchUsefulness,
    MetricCategory::ForgetCompleteness,
];

/// Render a category as its `snake_case` wire string. Matches the
/// `serde(rename_all = "snake_case")` shape in the replay schema.
#[must_use]
pub fn as_str(category: MetricCategory) -> &'static str {
    match category {
        MetricCategory::RecallPrecision => "recall_precision",
        MetricCategory::StaleAvoidance => "stale_avoidance",
        MetricCategory::SummaryQuality => "summary_quality",
        MetricCategory::SearchUsefulness => "search_usefulness",
        MetricCategory::ForgetCompleteness => "forget_completeness",
    }
}

/// Newtype wrapper so callers can `write!("{}", DisplayCategory(c))`
/// without depending on the third-party `Display` trait being implemented
/// on the upstream enum.
pub struct DisplayCategory(pub MetricCategory);

impl fmt::Display for DisplayCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(as_str(self.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_categories_listed_in_canonical_order() {
        let names: Vec<&'static str> = ALL.iter().copied().map(as_str).collect();
        assert_eq!(
            names,
            vec![
                "recall_precision",
                "stale_avoidance",
                "summary_quality",
                "search_usefulness",
                "forget_completeness",
            ]
        );
    }

    #[test]
    fn display_matches_as_str() {
        for category in ALL {
            assert_eq!(
                format!("{}", DisplayCategory(category)),
                as_str(category)
            );
        }
    }
}
