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

/// One element of an explicit-rank input list to [`rrf_fusion_weighted`].
///
/// Unlike [`ScoredCandidate`] (rank inferred from list position), this
/// carries `rank` and `weight` per candidate. Used by the graph leg where
/// the SQL output order is the rank and the connecting-edge confidence
/// is the weight, surviving hydration which may re-order results by id.
#[derive(Debug, Clone, PartialEq)]
pub struct RankedCandidate {
    /// Record id.
    pub record_id: RecordId,
    /// 1-based rank in the source list.
    pub rank: usize,
    /// Confidence weight in `[0.0, 1.0]`. Used to compute
    /// `effective_rank = rank / max(weight, floor)`.
    pub weight: f32,
}

/// One leg of [`rrf_fusion_weighted`].
///
/// `ListPosition` is the legacy shape (rank inferred from index, no
/// weighting). `Explicit` carries per-candidate rank and weight, and the
/// per-leg `floor` clamps weight from below to keep `effective_rank`
/// finite at zero confidence.
#[derive(Debug, Clone)]
pub enum Leg {
    /// Rank inferred from slice index. Equivalent to a single input of
    /// [`rrf_fusion`].
    ListPosition(Vec<ScoredCandidate>),
    /// Rank carried per-candidate; confidence penalty applied with `floor`.
    Explicit(Vec<RankedCandidate>, f32),
}

/// Confidence-weighted Reciprocal Rank Fusion.
///
/// Behaves identically to [`rrf_fusion`] for [`Leg::ListPosition`] legs
/// (proven by the `list_position_matches_legacy_fusion` test). For
/// [`Leg::Explicit`] legs each contribution is `1.0 / (k +
/// rank/max(weight, floor))`, so a low-confidence neighbor at rank 1
/// contributes less than a high-confidence one at the same rank, but
/// always contributes more than a list of equal length and rank with no
/// confidence penalty would.
#[must_use]
pub fn rrf_fusion_weighted(legs: &[Leg], k: usize) -> Vec<RrfCandidate> {
    use std::collections::HashMap;
    let mut acc: HashMap<RecordId, f64> = HashMap::new();
    #[allow(clippy::cast_precision_loss)]
    let kf = k as f64;
    for leg in legs {
        match leg {
            Leg::ListPosition(list) => {
                for (i, c) in list.iter().enumerate() {
                    #[allow(clippy::cast_precision_loss)]
                    let r = (i + 1) as f64;
                    *acc.entry(c.record_id.clone()).or_insert(0.0) += 1.0 / (kf + r);
                }
            }
            Leg::Explicit(list, floor) => {
                let f = f64::from((*floor).max(1e-6));
                for c in list {
                    #[allow(clippy::cast_precision_loss)]
                    let raw_rank = c.rank as f64;
                    let weight = f64::from(c.weight).max(f);
                    let effective = raw_rank / weight;
                    *acc.entry(c.record_id.clone()).or_insert(0.0) += 1.0 / (kf + effective);
                }
            }
        }
    }
    let mut out: Vec<RrfCandidate> = acc
        .into_iter()
        .map(|(record_id, rrf_score)| RrfCandidate {
            record_id,
            rrf_score,
        })
        .collect();
    out.sort_by(|a, b| {
        b.rrf_score
            .partial_cmp(&a.rrf_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.record_id.as_str().cmp(b.record_id.as_str()))
    });
    out
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
    fn weighted_extracted_outranks_inferred_at_same_rank() {
        let extracted = vec![RankedCandidate {
            record_id: rid("0A"),
            rank: 1,
            weight: 1.0,
        }];
        let inferred = vec![RankedCandidate {
            record_id: rid("0B"),
            rank: 1,
            weight: 0.6,
        }];
        let out = rrf_fusion_weighted(
            &[
                Leg::Explicit(extracted, 1e-3),
                Leg::Explicit(inferred, 1e-3),
            ],
            60,
        );
        assert_eq!(out[0].record_id, rid("0A"));
    }

    #[test]
    fn list_position_matches_legacy_fusion() {
        let list = vec![
            ScoredCandidate {
                record_id: rid("0A"),
                score: 1.0,
            },
            ScoredCandidate {
                record_id: rid("0B"),
                score: 0.5,
            },
        ];
        let legacy = rrf_fusion(std::slice::from_ref(&list), 60);
        let weighted = rrf_fusion_weighted(&[Leg::ListPosition(list)], 60);
        assert_eq!(legacy, weighted);
    }

    #[test]
    fn confidence_floor_prevents_div_by_zero() {
        let zero_conf = vec![RankedCandidate {
            record_id: rid("0A"),
            rank: 1,
            weight: 0.0,
        }];
        let out = rrf_fusion_weighted(&[Leg::Explicit(zero_conf, 1e-3)], 60);
        assert!(out[0].rrf_score.is_finite());
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
