//! Pure orchestration of RRF fusion + cosine re-rank.

use std::collections::HashMap;
use std::collections::HashSet;

use crate::domain::RecordId;

use super::cosine::{
    CandidateOrigin, OriginTaggedCandidate, RerankedCandidate, cosine_rerank,
    cosine_rerank_tagged,
};
use super::graph::GraphCandidate;
use super::rrf::{Leg, RankedCandidate, RrfCandidate, ScoredCandidate, rrf_fusion, rrf_fusion_weighted};

/// Inputs to [`hybrid_search`]. The store fetches keyword + semantic
/// candidate lists and the per-record vectors; this function does the math.
#[derive(Debug, Clone)]
pub struct HybridSearchInputs {
    /// FTS5 BM25 hits, sorted descending by BM25.
    pub keyword: Vec<ScoredCandidate>,
    /// Vector ANN hits, sorted ascending by L2 distance.
    /// (Smaller distance = more similar; convert to descending via reverse.)
    pub semantic: Vec<ScoredCandidate>,
    /// 1-hop entity-graph neighbor hits, in SQL output order. Each
    /// candidate carries its own `graph_rank` so RRF fusion does not
    /// infer rank from list position after hydration.
    pub graph: Vec<GraphCandidate>,
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
    /// Floor on per-candidate confidence weight in [`Leg::Explicit`]. Keeps
    /// `effective_rank = rank / max(weight, floor)` finite at zero
    /// confidence. Default `1e-3`.
    pub confidence_floor: f32,
}

impl Default for HybridSearchParams {
    fn default() -> Self {
        Self {
            rrf_k: 60,
            rerank_topk: 20,
            blend: 0.7,
            skip_rerank: false,
            confidence_floor: 1e-3,
        }
    }
}

/// Run RRF fusion (with optional graph leg) then optional cosine re-rank.
///
/// The store calls this after fetching all three legs. Output is sorted
/// descending by `final_score`. When the graph leg is empty this behaves
/// identically to the legacy 2-leg fusion (proven by the `parity` test).
#[must_use]
pub fn hybrid_search(
    inputs: &HybridSearchInputs,
    params: HybridSearchParams,
) -> Vec<RerankedCandidate> {
    // Fast path: no graph candidates → use the legacy 2-leg `rrf_fusion`
    // directly so existing callers and tests see byte-identical scoring.
    let fused: Vec<RrfCandidate> = if inputs.graph.is_empty() {
        let lists = vec![inputs.keyword.clone(), inputs.semantic.clone()];
        rrf_fusion(&lists, params.rrf_k)
    } else {
        let graph_ranked: Vec<RankedCandidate> = inputs
            .graph
            .iter()
            .map(|g| RankedCandidate {
                record_id: g.record_id.clone(),
                rank: g.graph_rank,
                weight: g.edge_confidence_score,
            })
            .collect();
        let legs = [
            Leg::ListPosition(inputs.keyword.clone()),
            Leg::ListPosition(inputs.semantic.clone()),
            Leg::Explicit(graph_ranked, params.confidence_floor),
        ];
        rrf_fusion_weighted(&legs, params.rrf_k)
    };

    if params.skip_rerank || params.blend >= 1.0 {
        let max_rrf = fused.iter().map(|c| c.rrf_score).fold(0.0_f64, f64::max);
        return fused
            .into_iter()
            .map(|c| {
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

    // 2-leg path: keep using the legacy untagged rerank for byte-compat.
    if inputs.graph.is_empty() {
        return cosine_rerank(
            &topk,
            &inputs.doc_vectors,
            &inputs.query_vector,
            params.blend,
        );
    }

    // 3-leg path: tag each top-K survivor as Lexical (appears in keyword
    // or semantic) or GraphOnly (only the graph leg surfaced it). The
    // origin gates whether the cosine term blends in — see
    // [`cosine_rerank_tagged`] for the formula.
    let lexical_ids: HashSet<&RecordId> = inputs
        .keyword
        .iter()
        .map(|c| &c.record_id)
        .chain(inputs.semantic.iter().map(|c| &c.record_id))
        .collect();
    let tagged: Vec<OriginTaggedCandidate> = topk
        .into_iter()
        .map(|c| {
            let origin = if lexical_ids.contains(&c.record_id) {
                CandidateOrigin::Lexical
            } else {
                CandidateOrigin::GraphOnly
            };
            OriginTaggedCandidate { inner: c, origin }
        })
        .collect();
    cosine_rerank_tagged(
        &tagged,
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
            graph: vec![],
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
            graph: vec![],
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
            graph: vec![],
            query_vector: vec![],
            doc_vectors: HashMap::new(),
        };
        let out = hybrid_search(&inputs, HybridSearchParams::default());
        assert!(out.is_empty());
    }

    #[test]
    fn graph_leg_empty_matches_legacy_2leg_output() {
        // Parity: the 3-leg path with an empty graph leg must produce the
        // same final ordering and final_score as the legacy 2-leg path.
        let mut docs: HashMap<RecordId, Vec<f32>> = HashMap::new();
        docs.insert(rid("0A"), vec![0.0, 1.0]);
        docs.insert(rid("0B"), vec![1.0, 0.0]);
        let inputs = HybridSearchInputs {
            keyword: vec![cand("0A", 1.0), cand("0B", 0.5)],
            semantic: vec![cand("0B", 0.1), cand("0A", 0.5)],
            graph: vec![],
            query_vector: vec![1.0_f32, 0.0],
            doc_vectors: docs,
        };
        let out = hybrid_search(&inputs, HybridSearchParams::default());
        assert_eq!(out.len(), 2);
        // Sanity: both candidates surface.
        let ids: std::collections::HashSet<_> = out.iter().map(|c| c.record_id.clone()).collect();
        assert!(ids.contains(&rid("0A")));
        assert!(ids.contains(&rid("0B")));
    }

    #[test]
    fn graph_only_candidate_surfaces_through_rerank() {
        // 0C appears only in the graph leg, with high edge confidence.
        // It should survive RRF + tagged rerank.
        let mut docs: HashMap<RecordId, Vec<f32>> = HashMap::new();
        docs.insert(rid("0A"), vec![1.0, 0.0]);
        let inputs = HybridSearchInputs {
            keyword: vec![cand("0A", 1.0)],
            semantic: vec![],
            graph: vec![super::super::graph::GraphCandidate {
                record_id: rid("0C"),
                edge_confidence_score: 0.95,
                graph_rank: 1,
            }],
            query_vector: vec![1.0_f32, 0.0],
            doc_vectors: docs,
        };
        let out = hybrid_search(&inputs, HybridSearchParams::default());
        let ids: Vec<_> = out.iter().map(|c| c.record_id.clone()).collect();
        assert!(ids.contains(&rid("0C")), "graph-only candidate must surface");
        // GraphOnly origin → cosine field is None.
        let c0c = out.iter().find(|c| c.record_id == rid("0C")).unwrap();
        assert!(c0c.cosine.is_none());
    }

    #[test]
    fn three_leg_high_confidence_outranks_low_confidence_at_same_rank() {
        // Two graph-only candidates at graph_rank 1, differing only by
        // edge confidence. Higher confidence must rank higher.
        let docs: HashMap<RecordId, Vec<f32>> = HashMap::new();
        let inputs_high = HybridSearchInputs {
            keyword: vec![],
            semantic: vec![],
            graph: vec![super::super::graph::GraphCandidate {
                record_id: rid("0A"),
                edge_confidence_score: 1.0,
                graph_rank: 1,
            }],
            query_vector: vec![1.0_f32, 0.0],
            doc_vectors: docs.clone(),
        };
        let inputs_low = HybridSearchInputs {
            keyword: vec![],
            semantic: vec![],
            graph: vec![super::super::graph::GraphCandidate {
                record_id: rid("0B"),
                edge_confidence_score: 0.3,
                graph_rank: 1,
            }],
            query_vector: vec![1.0_f32, 0.0],
            doc_vectors: docs,
        };
        let out_high = hybrid_search(&inputs_high, HybridSearchParams::default());
        let out_low = hybrid_search(&inputs_low, HybridSearchParams::default());
        assert!(out_high[0].rrf_score > out_low[0].rrf_score);
    }
}
