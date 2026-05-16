//! Brief §15-derived constants for the latency gate.
//!
//! See `docs/design/design-brief.md` §15 and §19.a for the source SLOs.

/// Brief §15: "fails build if any metric drops > 2%".
pub const DEFAULT_REGRESSION_PCT: f64 = 2.0;

/// Brief §15: p95 turn latency with hot-assembly + write < 50 ms.
pub const SLO_HOT_PATH_MS: f64 = 50.0;

/// Brief §15: p99 turn latency < 100 ms.
pub const SLO_HOT_PATH_P99_MS: f64 = 100.0;

/// Brief §15: forget-me reader-invisible latency (1M recs) < 1 s p95.
pub const SLO_FORGET_PHASE_A_MS: f64 = 1_000.0;

/// Brief §15: forget-me physical purge (Phase B) < 30 s p95.
pub const SLO_FORGET_PHASE_B_MS: f64 = 30_000.0;

/// Brief §15: cold-rehydration (≤ 10 MB session) < 3 s p95.
pub const SLO_COLD_REHYDRATE_MS: f64 = 3_000.0;
