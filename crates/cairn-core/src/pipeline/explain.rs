//! `explain_filter` — pure partition of caller-visible candidates into
//! kept and excluded subsets (brief §5.1). Tier-1-invisible records
//! are filtered upstream and must not appear in the input.

use std::collections::HashMap;
use std::collections::HashSet;

use crate::domain::TargetId;
use crate::policy_trace::{PolicyDetail, PolicyGate, RecordExclusion};

/// One candidate from the store query, **after** scope and Tier-1
/// visibility filtering.
///
/// `Candidate` is a sealed type: fields are private, the only
/// constructor [`Self::from_scope_filter`] is `pub(crate)`, and
/// [`explain_filter`] is `pub(crate)`. Together this means a candidate
/// can only be produced from inside `cairn-core`, on the verb-runtime
/// path that has already applied scope/visibility predicates. External
/// callers cannot synthesize a `Candidate`, so `explain_filter` cannot
/// leak `target_id` for unfiltered store rows.
///
/// When the verb runtime in #9 / #61 / #62 wires this in, the call site
/// inside `cairn-core::verbs::search` will be the only producer.
#[derive(Debug, Clone)]
pub struct Candidate {
    target_id: TargetId,
    age_days: u32,
    relevance_score: f32,
    content_hash: String,
}

impl Candidate {
    /// Construct a candidate **after** the store has applied scope and
    /// Tier-1 visibility predicates. `pub(crate)` — only callable from
    /// inside `cairn-core`, where the verb runtime can be reviewed for
    /// the precondition (brief §5.1, §14).
    ///
    /// `relevance_score`: NaN values lose to non-NaN; two NaNs resolve
    /// to first-seen.
    ///
    /// Currently only the inline test module calls this constructor;
    /// the verb-runtime caller in `cairn-core::verbs::search` lands
    /// with #9 / #61 / #62.
    #[allow(dead_code)]
    #[must_use]
    pub(crate) fn from_scope_filter(
        target_id: TargetId,
        age_days: u32,
        relevance_score: f32,
        content_hash: String,
    ) -> Self {
        Self {
            target_id,
            age_days,
            relevance_score,
            content_hash,
        }
    }

    /// Borrow the candidate's target id (e.g. for read-only inspection
    /// after partitioning).
    #[must_use]
    pub fn target_id(&self) -> &TargetId {
        &self.target_id
    }

    /// Age in days, as supplied at construction.
    #[must_use]
    pub const fn age_days(&self) -> u32 {
        self.age_days
    }

    /// Relevance score, as supplied at construction.
    #[must_use]
    pub const fn relevance_score(&self) -> f32 {
        self.relevance_score
    }

    /// Content hash used for dedup partitioning.
    #[must_use]
    pub fn content_hash(&self) -> &str {
        &self.content_hash
    }
}

/// Pure partition configuration.
#[derive(Debug, Clone, Copy)]
pub struct ExplainConfig {
    /// Candidates older than this are excluded as `ReadFilterStaleness`.
    pub staleness_threshold_days: u32,
}

/// Reason a candidate was filtered. `as_gate` maps to the producer
/// `PolicyGate` variant — closed because per-record exclusions are
/// limited to Tier-2 read filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReadFilterReason {
    /// Record was filtered out by relevance ranking.
    Relevance,
    /// Record's `age_days` exceeded the threshold.
    Staleness,
    /// Record's `content_hash` already chose a higher-relevance winner.
    Dedup,
}

impl ReadFilterReason {
    /// Map to the matching `PolicyGate` variant.
    #[must_use]
    pub const fn as_gate(self) -> PolicyGate {
        match self {
            Self::Relevance => PolicyGate::ReadFilterRelevance,
            Self::Staleness => PolicyGate::ReadFilterStaleness,
            Self::Dedup => PolicyGate::ReadFilterDedup,
        }
    }
}

/// Partition scope-filtered `candidates` into kept and excluded sets.
/// `pub(crate)` — only callable from inside `cairn-core` because each
/// input must be a [`Candidate`], which only `cairn-core` can construct.
/// External callers can read the partition outputs once verb runtime
/// (#9 / #61 / #62) wires the partition into `search`'s envelope.
///
/// Currently only the inline test module calls this; the verb-runtime
/// caller in `cairn-core::verbs::search` lands with #9 / #61 / #62.
///
/// Order:
///
/// 1. Staleness — exclude any candidate older than the threshold.
/// 2. Dedup — globally across the post-staleness set, keep the
///    highest-relevance candidate per `content_hash`; exclude the
///    rest. NaN scores lose to non-NaN; ties between NaNs resolve to
///    the first-seen candidate.
///
/// Relevance pruning is not applied here — callers that want a
/// top-N cut compose this with their own ranker.
#[allow(dead_code)]
#[must_use]
pub(crate) fn explain_filter(
    candidates: Vec<Candidate>,
    cfg: ExplainConfig,
) -> (Vec<Candidate>, Vec<RecordExclusion>) {
    let mut excluded: Vec<RecordExclusion> = Vec::new();

    // 1. Staleness pass — preserve original order.
    let mut after_stale: Vec<Candidate> = Vec::with_capacity(candidates.len());
    for c in candidates {
        if c.age_days > cfg.staleness_threshold_days {
            excluded.push(RecordExclusion::new(
                c.target_id.clone(),
                PolicyGate::ReadFilterStaleness,
                PolicyDetail::None,
            ));
        } else {
            after_stale.push(c);
        }
    }

    // 2. Dedup by content_hash — keep highest-relevance per hash,
    //    preserve the original order of the kept candidates. NaN
    //    scores always lose to non-NaN scores; two NaNs resolve to
    //    first-seen.
    let mut best_index_by_hash: HashMap<String, usize> = HashMap::new();
    for (idx, c) in after_stale.iter().enumerate() {
        match best_index_by_hash.get(&c.content_hash) {
            None => {
                best_index_by_hash.insert(c.content_hash.clone(), idx);
            }
            Some(&prev_idx) => {
                let prev_score = after_stale[prev_idx].relevance_score;
                let cur_score = c.relevance_score;
                // Non-NaN beats NaN. Two NaNs keep the first-seen.
                // Two non-NaNs use ordinary `>`.
                let cur_wins =
                    !cur_score.is_nan() && (prev_score.is_nan() || cur_score > prev_score);
                if cur_wins {
                    best_index_by_hash.insert(c.content_hash.clone(), idx);
                }
            }
        }
    }
    let kept_indices: HashSet<usize> = best_index_by_hash.values().copied().collect();
    let mut kept: Vec<Candidate> = Vec::with_capacity(kept_indices.len());
    for (idx, c) in after_stale.into_iter().enumerate() {
        if kept_indices.contains(&idx) {
            kept.push(c);
        } else {
            excluded.push(RecordExclusion::new(
                c.target_id,
                PolicyGate::ReadFilterDedup,
                PolicyDetail::None,
            ));
        }
    }

    (kept, excluded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy_trace::{PolicyDetail, PolicyGate};

    // Build a valid 26-char ULID-shaped TargetId. Crockford base32
    // (no I L O U), leading char 0..=7. Trailing char(s) replaced by
    // `suffix` for uniqueness.
    fn id(suffix: char) -> TargetId {
        let mut s = String::from("01HQZX9F5N0000000000000000");
        s.pop();
        s.push(suffix);
        TargetId::parse(s).expect("valid ULID")
    }

    #[test]
    fn empty_candidates_yields_empty_kept_and_excluded() {
        let cfg = ExplainConfig {
            staleness_threshold_days: 30,
        };
        let (kept, excluded) = explain_filter(Vec::<Candidate>::new(), cfg);
        assert!(kept.is_empty());
        assert!(excluded.is_empty());
    }

    #[test]
    fn stale_candidate_is_excluded_with_staleness_gate() {
        let cfg = ExplainConfig {
            staleness_threshold_days: 30,
        };
        let candidates = vec![Candidate::from_scope_filter(
            id('A'),
            90,
            0.8,
            "h1".to_owned(),
        )];
        let (kept, excluded) = explain_filter(candidates, cfg);
        assert!(kept.is_empty());
        assert_eq!(excluded.len(), 1);
        assert_eq!(excluded[0].gate(), PolicyGate::ReadFilterStaleness);
        assert_eq!(excluded[0].detail(), &PolicyDetail::None);
    }

    #[test]
    fn duplicate_content_hash_excluded_by_dedup() {
        let cfg = ExplainConfig {
            staleness_threshold_days: 30,
        };
        let candidates = vec![
            Candidate::from_scope_filter(id('A'), 1, 0.9, "h".to_owned()),
            Candidate::from_scope_filter(id('B'), 1, 0.8, "h".to_owned()),
        ];
        let (kept, excluded) = explain_filter(candidates, cfg);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].target_id(), &id('A'));
        assert_eq!(excluded.len(), 1);
        assert_eq!(excluded[0].target_id(), &id('B'));
        assert_eq!(excluded[0].gate(), PolicyGate::ReadFilterDedup);
    }

    #[test]
    fn stale_takes_precedence_over_dedup() {
        let cfg = ExplainConfig {
            staleness_threshold_days: 30,
        };
        let candidates = vec![
            Candidate::from_scope_filter(id('A'), 90, 0.9, "h".to_owned()),
            Candidate::from_scope_filter(id('B'), 1, 0.5, "h".to_owned()),
        ];
        let (kept, excluded) = explain_filter(candidates, cfg);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].target_id(), &id('B'));
        assert_eq!(excluded.len(), 1);
        assert_eq!(excluded[0].gate(), PolicyGate::ReadFilterStaleness);
    }

    #[test]
    fn nan_score_loses_to_non_nan_regardless_of_arrival_order() {
        let cfg = ExplainConfig {
            staleness_threshold_days: 30,
        };
        // NaN seen first; non-NaN second — non-NaN must win the dedup.
        let candidates = vec![
            Candidate::from_scope_filter(id('A'), 1, f32::NAN, "h".to_owned()),
            Candidate::from_scope_filter(id('B'), 1, 0.1, "h".to_owned()),
        ];
        let (kept, excluded) = explain_filter(candidates, cfg);
        assert_eq!(kept.len(), 1, "exactly one survives dedup");
        assert_eq!(kept[0].target_id(), &id('B'), "non-NaN wins over NaN");
        assert_eq!(excluded.len(), 1);
        assert_eq!(excluded[0].target_id(), &id('A'));
        assert_eq!(excluded[0].gate(), PolicyGate::ReadFilterDedup);
    }

    #[test]
    fn nan_score_loses_to_non_nan_when_seen_second() {
        let cfg = ExplainConfig {
            staleness_threshold_days: 30,
        };
        let candidates = vec![
            Candidate::from_scope_filter(id('A'), 1, 0.1, "h".to_owned()),
            Candidate::from_scope_filter(id('B'), 1, f32::NAN, "h".to_owned()),
        ];
        let (kept, excluded) = explain_filter(candidates, cfg);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].target_id(), &id('A'), "non-NaN keeps its win");
        assert_eq!(excluded.len(), 1);
        assert_eq!(excluded[0].target_id(), &id('B'));
    }

    #[test]
    fn two_nan_scores_resolve_to_first_seen() {
        let cfg = ExplainConfig {
            staleness_threshold_days: 30,
        };
        let candidates = vec![
            Candidate::from_scope_filter(id('A'), 1, f32::NAN, "h".to_owned()),
            Candidate::from_scope_filter(id('B'), 1, f32::NAN, "h".to_owned()),
        ];
        let (kept, _excluded) = explain_filter(candidates, cfg);
        assert_eq!(kept[0].target_id(), &id('A'), "first NaN wins tie-break");
    }

    #[test]
    fn read_filter_reason_round_trips() {
        let cases = [
            (ReadFilterReason::Staleness, PolicyGate::ReadFilterStaleness),
            (ReadFilterReason::Dedup, PolicyGate::ReadFilterDedup),
            (ReadFilterReason::Relevance, PolicyGate::ReadFilterRelevance),
        ];
        for (reason, expected_gate) in cases {
            assert_eq!(reason.as_gate(), expected_gate);
        }
    }
}
