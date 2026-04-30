//! `explain_filter` — pure partition of caller-visible candidates into
//! kept and excluded subsets (brief §5.1). Tier-1-invisible records
//! are filtered upstream and must not appear in the input.

use std::collections::HashMap;
use std::collections::HashSet;

use crate::domain::TargetId;
use crate::policy_trace::{PolicyDetail, PolicyGate, RecordExclusion};

/// One candidate from the store query. Caller-visible by definition;
/// the store filters Tier-1-invisible records before populating this.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// Lineage key for the record.
    pub target_id: TargetId,
    /// How many days old the record is.
    pub age_days: u32,
    /// Relevance score from the ranker; higher wins dedup. NaN scores
    /// always lose to non-NaN scores; ties between two NaN scores
    /// resolve to the first-seen candidate.
    pub relevance_score: f32,
    /// Content hash used for deduplication.
    pub content_hash: String,
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

/// Partition `candidates` into kept and excluded sets. Order:
///
/// 1. Staleness — exclude any candidate older than the threshold.
/// 2. Dedup — globally across the post-staleness set, keep the
///    highest-relevance candidate per `content_hash`; exclude the
///    rest. NaN scores lose to non-NaN; ties between NaNs resolve to
///    the first-seen candidate.
///
/// Relevance pruning is not applied here — callers that want a
/// top-N cut compose this with their own ranker.
#[must_use]
pub fn explain_filter(
    candidates: Vec<Candidate>,
    cfg: &ExplainConfig,
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
