//! IR metrics: P@K, R@K, MRR, nDCG@K.
//!
//! Standard formulas; mirror the implementations in
//! `crates/cairn-store-sqlite/examples/gbrain_compare.rs` so numbers stay
//! comparable across the two harnesses.

// k and the corpus size are tiny relative to f64's mantissa (2^53), so
// the usize-to-f64 casts in the metric loops are lossless in practice.
// The single-letter variable pairs (`a`/`b`, `g`) are conventional names
// from the IR formulas — renaming hurts readability.
#![allow(clippy::cast_precision_loss, clippy::similar_names)]

use std::collections::{BTreeMap, BTreeSet};

/// Per-query IR metrics for a single adapter run.
#[derive(Debug, Clone, Default)]
pub struct PerQueryMetrics {
    /// Precision at 5.
    pub p_at_5: f64,
    /// Recall at 5.
    pub r_at_5: f64,
    /// Mean reciprocal rank (single-query: reciprocal rank).
    pub mrr: f64,
    /// Normalized discounted cumulative gain at 5.
    pub ndcg_at_5: f64,
}

/// Compute the standard P@5 / R@5 / MRR / nDCG@5 bundle for one query.
#[must_use]
pub fn compute(
    hits: &[String],
    rel: &BTreeSet<String>,
    grades: &BTreeMap<String, u32>,
) -> PerQueryMetrics {
    PerQueryMetrics {
        p_at_5: precision_at_k(hits, rel, 5),
        r_at_5: recall_at_k(hits, rel, 5),
        mrr: mrr(hits, rel),
        ndcg_at_5: ndcg_at_k(hits, grades, rel, 5),
    }
}

/// Precision at K: fraction of the top-K hits that are in `rel`.
///
/// Returns 0.0 if `k == 0` or `rel` is empty (avoids divide-by-zero
/// and lets the caller treat empty-relevance queries uniformly).
#[must_use]
pub fn precision_at_k(hits: &[String], rel: &BTreeSet<String>, k: usize) -> f64 {
    if k == 0 || rel.is_empty() {
        return 0.0;
    }
    let take = hits.iter().take(k).filter(|s| rel.contains(*s)).count();
    take as f64 / k as f64
}

/// Recall at K: fraction of relevant items captured in the top-K hits.
///
/// Returns 0.0 if `rel` is empty.
#[must_use]
pub fn recall_at_k(hits: &[String], rel: &BTreeSet<String>, k: usize) -> f64 {
    if rel.is_empty() {
        return 0.0;
    }
    let take = hits.iter().take(k).filter(|s| rel.contains(*s)).count();
    take as f64 / rel.len() as f64
}

/// Reciprocal rank: 1 / (rank of the first relevant hit), or 0.0 if
/// none of `hits` are in `rel`.
#[must_use]
pub fn mrr(hits: &[String], rel: &BTreeSet<String>) -> f64 {
    for (i, h) in hits.iter().enumerate() {
        if rel.contains(h) {
            return 1.0 / (i as f64 + 1.0);
        }
    }
    0.0
}

/// Normalized discounted cumulative gain at K.
///
/// Missing grades default to 1 for any slug present in `rel`,
/// matching the upstream `gbrain` analyzer.
#[must_use]
pub fn ndcg_at_k(
    hits: &[String],
    grades: &BTreeMap<String, u32>,
    rel: &BTreeSet<String>,
    k: usize,
) -> f64 {
    let grade_of = |slug: &str| -> u32 {
        grades
            .get(slug)
            .copied()
            .unwrap_or_else(|| u32::from(rel.contains(slug)))
    };
    let mut dcg = 0.0;
    for (i, h) in hits.iter().take(k).enumerate() {
        let g = f64::from(grade_of(h));
        dcg += g / ((i as f64 + 2.0).log2());
    }
    let mut graded: Vec<u32> = rel.iter().map(|s| grade_of(s)).collect();
    graded.sort_unstable_by(|a, b| b.cmp(a));
    let mut idcg = 0.0;
    for (i, g) in graded.into_iter().take(k).enumerate() {
        idcg += f64::from(g) / ((i as f64 + 2.0).log2());
    }
    if idcg.abs() < f64::EPSILON {
        0.0
    } else {
        dcg / idcg
    }
}

#[cfg(test)]
mod tests {
    use super::{BTreeMap, BTreeSet, mrr, ndcg_at_k, precision_at_k};

    fn rel(slugs: &[&str]) -> BTreeSet<String> {
        slugs.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn p_at_5_top_1() {
        let hits = vec!["a".into(), "b".into(), "c".into()];
        let r = rel(&["a", "z"]);
        assert!((precision_at_k(&hits, &r, 5) - 0.2).abs() < 1e-12);
    }

    #[test]
    fn mrr_first_position() {
        let hits = vec!["x".into(), "a".into()];
        let r = rel(&["a"]);
        assert!((mrr(&hits, &r) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn ndcg_with_uniform_grades() {
        let hits = vec!["a".into(), "b".into()];
        let r = rel(&["a", "b"]);
        let g: BTreeMap<String, u32> = BTreeMap::new();
        let n = ndcg_at_k(&hits, &g, &r, 5);
        // Both relevant in top-2 with default grade 1 → idcg == dcg → 1.0
        assert!((n - 1.0).abs() < 1e-12);
    }
}
