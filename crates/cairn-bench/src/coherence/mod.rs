//! Coherence release-gate (issue #137).
//!
//! See `docs/design/2026-05-24-coherence-benchmarks-design.md` for the
//! contract. Public API: `run_coherence_gate` (added in a later task).

pub mod category;

pub use category::{ALL as ALL_CATEGORIES, DisplayCategory, MetricCategory, as_str};
