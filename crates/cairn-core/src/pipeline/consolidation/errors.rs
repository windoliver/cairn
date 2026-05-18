//! Errors raised by the pure consolidation pipeline.

/// Failures from [`super::compute_rolling_summary`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConsolidationError {
    /// Caller supplied a window with no turns.
    #[error("consolidation: empty window")]
    EmptyWindow,
    /// Generated body could not fit inside the configured token budget.
    #[error("consolidation: body exceeds token_budget {budget}")]
    BudgetExceeded {
        /// The configured budget that was exceeded.
        budget: u32,
    },
}
