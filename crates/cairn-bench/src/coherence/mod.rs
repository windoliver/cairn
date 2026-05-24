//! Coherence release-gate (issue #137).
//!
//! See `docs/design/2026-05-24-coherence-benchmarks-design.md`.
//! Public API will be wired up in a later task.

pub mod category;
pub mod score;

pub use category::{ALL as ALL_CATEGORIES, DisplayCategory, MetricCategory, as_str};
pub use score::{CategoryScore, CategoryScores, ScoreError, aggregate, classify};
