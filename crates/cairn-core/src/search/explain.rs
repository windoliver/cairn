//! Per-candidate score explanation surfaced when `--explain` is set.
//!
//! Populated by the store layer when `*SearchArgs.with_explain` is true.
//! The dispatcher passes the explain block through to the response envelope
//! gated by the `cairn.mcp.v1.policy_trace` capability.

use crate::domain::RecordId;

/// Component scores for a single search candidate.
///
/// Field semantics:
/// - `bm25_rank`: 1-based position in the FTS5 BM25 leg, or `None` if the
///   candidate did not appear in that leg.
/// - `semantic_rank`: 1-based position in the ANN leg, or `None`.
/// - `rrf_score`: RRF fusion score; `0.0` for keyword/semantic-only modes
///   that did not run RRF.
/// - `cosine`: cosine similarity to the query vector when the candidate was
///   in the rerank top-K; `None` otherwise.
/// - `final_score`: blended `final_score` from the orchestrator (hybrid),
///   normalized RRF (hybrid skip-rerank), or the leg's native score.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoreExplain {
    /// Identifier of the explained candidate.
    pub record_id: RecordId,
    /// 1-based BM25 leg rank, or `None` if absent from that leg.
    pub bm25_rank: Option<usize>,
    /// 1-based semantic leg rank, or `None` if absent from that leg.
    pub semantic_rank: Option<usize>,
    /// RRF fusion score. `0.0` for non-hybrid modes.
    pub rrf_score: f64,
    /// Cosine similarity to the query vector when re-ranked, else `None`.
    pub cosine: Option<f64>,
    /// Blended final score the candidate was sorted by.
    pub final_score: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rid(s: &str) -> RecordId {
        RecordId::parse(format!("01HQZX9F5N00000000000000{s}")).expect("valid record id")
    }

    #[test]
    fn explain_holds_all_components() {
        let e = ScoreExplain {
            record_id: rid("0A"),
            bm25_rank: Some(1),
            semantic_rank: Some(3),
            rrf_score: 0.05,
            cosine: Some(0.87),
            final_score: 0.91,
        };
        assert_eq!(e.bm25_rank, Some(1));
        assert_eq!(e.semantic_rank, Some(3));
        assert!((e.rrf_score - 0.05).abs() < 1e-9);
        assert_eq!(e.cosine, Some(0.87));
    }
}
