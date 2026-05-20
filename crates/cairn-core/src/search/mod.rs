//! Pure retrieval-ranking primitives: RRF fusion and cosine re-rank.
//!
//! These functions have no I/O; they take pre-fetched candidate lists and
//! return scored output. The store adapters orchestrate the data fetching.

mod cosine;
mod degraded;
mod explain;
mod graph;
mod orchestrator;
mod rrf;
mod trim;

pub use cosine::{
    CandidateOrigin, OriginTaggedCandidate, RerankedCandidate, cosine_rerank, cosine_rerank_tagged,
    cosine_similarity,
};
pub use degraded::{DegradationReason, DegradedLeg, GraphSource};
pub use explain::ScoreExplain;
pub use graph::GraphCandidate;
pub use orchestrator::{HybridSearchInputs, HybridSearchParams, hybrid_search};
pub use rrf::{
    Leg, RankedCandidate, RrfCandidate, ScoredCandidate, rrf_fusion, rrf_fusion_weighted,
};
pub use trim::token_budget_trim;
