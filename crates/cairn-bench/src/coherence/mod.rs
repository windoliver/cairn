//! Coherence release-gate (issue #137).
//!
//! See `docs/design/2026-05-24-coherence-benchmarks-design.md`.
//! Public API will be wired up in a later task.

pub mod category;
pub mod report;
pub mod score;
pub mod threshold;
pub mod trend;

pub use category::{ALL as ALL_CATEGORIES, DisplayCategory, MetricCategory, as_str};
pub use report::{GateReport, build as build_report, render_human, render_json};
pub use score::{CategoryScore, CategoryScores, ScoreError, aggregate, classify};
pub use threshold::{
    Baseline, CategoryThreshold, GateMode, MetricOutcome, MetricResult, ThresholdError,
    ThresholdManifest, all_pass, evaluate, load_manifest,
};
pub use trend::{TrendEntry, TrendError, append as append_trend, load as load_trend};
