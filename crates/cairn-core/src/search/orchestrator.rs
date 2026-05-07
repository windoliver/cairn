//! Pure orchestration of RRF fusion + cosine re-rank.

use std::collections::HashMap;

use crate::domain::RecordId;

use super::cosine::{RerankedCandidate, cosine_rerank};
use super::rrf::{RrfCandidate, ScoredCandidate, rrf_fusion};

/// Inputs to [`hybrid_search`]. The store fetches keyword + semantic
/// candidate lists and the per-record vectors; this function does the math.
#[derive(Debug, Clone)]
pub struct HybridSearchInputs {
    /// FTS5 BM25 hits, sorted descending by BM25.
    pub keyword: Vec<ScoredCandidate>,
    /// Vector ANN hits, sorted ascending by L2 distance.
    /// (Smaller distance = more similar; convert to descending via reverse.)
    pub semantic: Vec<ScoredCandidate>,
    /// Query embedding (for cosine re-rank).
    pub query_vector: Vec<f32>,
    /// Top-K record vectors, fetched after RRF. May be a subset of the
    /// fused candidates; missing entries get `cosine = 0.0` in re-rank.
    pub doc_vectors: HashMap<RecordId, Vec<f32>>,
}

/// Configuration for [`hybrid_search`]. Pulled from `SearchConfig`.
#[derive(Debug, Clone, Copy)]
pub struct HybridSearchParams {
    /// RRF constant. Default 60.
    pub rrf_k: usize,
    /// Top-K from RRF that are second-pass re-ranked. Default 20.
    pub rerank_topk: usize,
    /// Blend coefficient α. `1.0` skips the cosine pass.
    pub blend: f32,
    /// `true` to skip the cosine pass entirely (useful when the semantic
    /// leg failed and we only have RRF). The output `cosine` will be `None`.
    pub skip_rerank: bool,
}

impl Default for HybridSearchParams {
    fn default() -> Self {
        Self {
            rrf_k: 60,
            rerank_topk: 20,
            blend: 0.7,
            skip_rerank: false,
        }
    }
}

/// Run RRF fusion then optional cosine re-rank.
///
/// The store calls this after fetching both legs. Output is sorted
/// descending by `final_score`.
#[must_use]
pub fn hybrid_search(
    inputs: &HybridSearchInputs,
    params: HybridSearchParams,
) -> Vec<RerankedCandidate> {
    let lists = vec![inputs.keyword.clone(), inputs.semantic.clone()];
    let fused: Vec<RrfCandidate> = rrf_fusion(&lists, params.rrf_k);
    if params.skip_rerank || params.blend >= 1.0 {
        // Skip cosine: emit reranked candidates with normalized RRF as final.
        let max_rrf = fused.iter().map(|c| c.rrf_score).fold(0.0_f64, f64::max);
        return fused
            .into_iter()
            .map(|c| {
                // `max_rrf` is produced by `f64::max` over a list that may
                // be empty or all-zero; in those cases the literal `0.0` is
                // returned and the comparison is exact-equivalent.
                let normalized = if max_rrf < f64::EPSILON {
                    0.0
                } else {
                    c.rrf_score / max_rrf
                };
                RerankedCandidate {
                    record_id: c.record_id,
                    rrf_score: c.rrf_score,
                    cosine: None,
                    final_score: normalized,
                }
            })
            .collect();
    }
    let topk = fused
        .iter()
        .take(params.rerank_topk)
        .cloned()
        .collect::<Vec<_>>();
    cosine_rerank(
        &topk,
        &inputs.doc_vectors,
        &inputs.query_vector,
        params.blend,
    )
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
    fn skip_rerank_returns_normalized_rrf() {
        let inputs = HybridSearchInputs {
            keyword: vec![cand("0A", 1.0), cand("0B", 0.5)],
            semantic: vec![],
            query_vector: vec![],
            doc_vectors: HashMap::new(),
        };
        let params = HybridSearchParams {
            skip_rerank: true,
            ..HybridSearchParams::default()
        };
        let out = hybrid_search(&inputs, params);
        assert_eq!(out[0].record_id, rid("0A"));
        assert!(out[0].cosine.is_none());
    }

    #[test]
    fn rerank_uses_cosine_when_blend_low() {
        let mut docs: HashMap<RecordId, Vec<f32>> = HashMap::new();
        docs.insert(rid("0A"), vec![0.0, 1.0]);
        docs.insert(rid("0B"), vec![1.0, 0.0]);
        let inputs = HybridSearchInputs {
            // 0A leads in keyword
            keyword: vec![cand("0A", 1.0), cand("0B", 0.5)],
            // 0B leads in semantic
            semantic: vec![cand("0B", 0.1), cand("0A", 0.5)],
            query_vector: vec![1.0_f32, 0.0],
            doc_vectors: docs,
        };
        let params = HybridSearchParams {
            blend: 0.0, // pure cosine
            ..HybridSearchParams::default()
        };
        let out = hybrid_search(&inputs, params);
        // pure cosine: 0B (cos=1) > 0A (cos=0)
        assert_eq!(out[0].record_id, rid("0B"));
    }

    #[test]
    fn empty_legs_returns_empty() {
        let inputs = HybridSearchInputs {
            keyword: vec![],
            semantic: vec![],
            query_vector: vec![],
            doc_vectors: HashMap::new(),
        };
        let out = hybrid_search(&inputs, HybridSearchParams::default());
        assert!(out.is_empty());
    }
}
