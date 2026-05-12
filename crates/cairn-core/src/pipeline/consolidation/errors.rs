//! Filled in Task 3.

/// Failures from [`super::compute_rolling_summary`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConsolidationError {
    /// Caller supplied a window with no turns.
    #[error("consolidation: empty window")]
    EmptyWindow,
}
