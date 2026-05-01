//! Pure retrieval-ranking primitives: RRF fusion and cosine re-rank.
//!
//! These functions have no I/O; they take pre-fetched candidate lists and
//! return scored output. The store adapters orchestrate the data fetching.

mod rrf;

pub use rrf::{RrfCandidate, ScoredCandidate, rrf_fusion};
