//! Reciprocal Rank Fusion (RRF).

use crate::domain::RecordId;

/// One element of an input rank list to RRF.
///
/// Each input list is pre-sorted descending by its source's score
/// (BM25, cosine similarity, etc.). RRF only uses the rank position;
/// it does not rescore via the original score.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredCandidate {
    /// Record id.
    pub record_id: RecordId,
    /// Source score, kept for diagnostics. RRF does not use this.
    pub score: f64,
}

/// Output of [`rrf_fusion`].
#[derive(Debug, Clone, PartialEq)]
pub struct RrfCandidate {
    /// Record id.
    pub record_id: RecordId,
    /// Sum of `1.0 / (k + rank)` across input lists where this id appeared.
    pub rrf_score: f64,
}

/// Reciprocal Rank Fusion over `inputs`.
///
/// Each input list must be pre-sorted descending by its own score.
/// The constant `k` softens the curve; canonical IR literature default
/// is `60`. Returns candidates sorted descending by `rrf_score`.
///
/// Empty input lists are tolerated and contribute nothing.
#[must_use]
pub fn rrf_fusion(inputs: &[Vec<ScoredCandidate>], k: usize) -> Vec<RrfCandidate> {
    use std::collections::HashMap;
    let mut acc: HashMap<RecordId, f64> = HashMap::new();
    // `k` is a small softening constant (canonical default 60); cast to f64.
    #[allow(clippy::cast_precision_loss)] // k is small (≤ usize::MAX); precision irrelevant
    let k = k as f64;
    for list in inputs {
        for (rank, candidate) in list.iter().enumerate() {
            // rank starts at 1 in the formula
            #[allow(clippy::cast_precision_loss)]
            // rank bounded by list length; precision irrelevant
            let r = (rank + 1) as f64;
            let contribution = 1.0 / (k + r);
            *acc.entry(candidate.record_id.clone()).or_insert(0.0) += contribution;
        }
    }
    let mut out: Vec<RrfCandidate> = acc
        .into_iter()
        .map(|(record_id, rrf_score)| RrfCandidate {
            record_id,
            rrf_score,
        })
        .collect();
    // Sort descending by score; tie-break on record_id for determinism.
    out.sort_by(|a, b| {
        b.rrf_score
            .partial_cmp(&a.rrf_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.record_id.as_str().cmp(b.record_id.as_str()))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rid(s: &str) -> RecordId {
        // Prefix is 24 chars; `s` is a 2-char suffix → 26-char ULID.
        RecordId::parse(format!("01HQZX9F5N00000000000000{s}")).expect("valid record id")
    }

    fn cand(s: &str, score: f64) -> ScoredCandidate {
        ScoredCandidate {
            record_id: rid(s),
            score,
        }
    }

    #[test]
    fn empty_inputs_returns_empty() {
        let out = rrf_fusion(&[], 60);
        assert!(out.is_empty());
    }

    #[test]
    fn single_list_preserves_order() {
        let list = vec![cand("0A", 10.0), cand("0B", 5.0), cand("0C", 1.0)];
        let out = rrf_fusion(&[list], 60);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].record_id, rid("0A"));
        assert_eq!(out[1].record_id, rid("0B"));
        assert_eq!(out[2].record_id, rid("0C"));
        // 1/(60+1), 1/(60+2), 1/(60+3)
        assert!((out[0].rrf_score - 1.0 / 61.0).abs() < 1e-12);
        assert!((out[1].rrf_score - 1.0 / 62.0).abs() < 1e-12);
        assert!((out[2].rrf_score - 1.0 / 63.0).abs() < 1e-12);
    }

    #[test]
    fn two_lists_doc_in_both_outranks_doc_in_one() {
        // 0A is rank 2 in both → 2/62 ≈ 0.0322
        // 0B is rank 1 in list 1 only → 1/61 ≈ 0.0164
        let list1 = vec![cand("0B", 10.0), cand("0A", 5.0)];
        let list2 = vec![cand("0C", 8.0), cand("0A", 3.0)];
        let out = rrf_fusion(&[list1, list2], 60);
        assert_eq!(out[0].record_id, rid("0A"));
    }

    #[test]
    fn rank_only_score_ignored() {
        // The original scores differ wildly but rank position is the same.
        // Output ranking must be identical.
        let a = vec![cand("0A", 1000.0), cand("0B", 999.0)];
        let b = vec![cand("0A", 0.001), cand("0B", 0.0001)];
        let out_a = rrf_fusion(&[a], 60);
        let out_b = rrf_fusion(&[b], 60);
        assert_eq!(
            out_a
                .iter()
                .map(|c| c.record_id.as_str().to_owned())
                .collect::<Vec<_>>(),
            out_b
                .iter()
                .map(|c| c.record_id.as_str().to_owned())
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn deterministic_tie_breaking() {
        // 0A and 0B both rank 1 in their respective lists → identical RRF score.
        // Tie-broken by record_id ascending.
        let list1 = vec![cand("0B", 5.0)];
        let list2 = vec![cand("0A", 5.0)];
        let out = rrf_fusion(&[list1, list2], 60);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].record_id, rid("0A"));
        assert_eq!(out[1].record_id, rid("0B"));
    }
}

#[cfg(test)]
mod prop_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn rrf_score_descending(
            // Sizes capped at 16 so that `rank` and `list_idx` each fit in
            // one hex char — keeps the generated ULID suffix exactly 2 chars.
            sizes in prop::collection::vec(1usize..16, 1..5),
        ) {
            let mut lists: Vec<Vec<ScoredCandidate>> = Vec::new();
            for (list_idx, size) in sizes.iter().enumerate() {
                let mut list = Vec::with_capacity(*size);
                for rank in 0..*size {
                    let suffix = format!("{list_idx:X}{rank:X}");
                    list.push(ScoredCandidate {
                        record_id: RecordId::parse(format!(
                            "01HQZX9F5N00000000000000{suffix}"
                        ))
                        .unwrap(),
                        // rank is bounded by `size` (≤16) — well within f64 mantissa range.
                        #[allow(clippy::cast_precision_loss)]
                        score: (1000 - rank) as f64,
                    });
                }
                lists.push(list);
            }
            let out = rrf_fusion(&lists, 60);
            for w in out.windows(2) {
                prop_assert!(w[0].rrf_score >= w[1].rrf_score);
            }
        }
    }
}
