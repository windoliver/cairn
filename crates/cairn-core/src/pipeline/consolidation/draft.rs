//! Filled in Task 3.

use super::errors::ConsolidationError;
use super::window::WindowSelection;
use crate::config::ConsolidationConfig;

/// Whether the consolidator produced a summary or deferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryStatus {
    /// Placeholder — Task 3 expands.
    Pending,
}

/// Placeholder — Task 3 expands.
#[derive(Debug, Clone, PartialEq)]
pub struct RollingSummaryDraft {
    /// Placeholder.
    pub status: SummaryStatus,
}

/// Placeholder — Task 3 expands. Always returns `EmptyWindow`.
///
/// # Errors
/// Always returns [`ConsolidationError::EmptyWindow`].
pub fn compute_rolling_summary(
    _window: &WindowSelection,
    _config: &ConsolidationConfig,
) -> Result<RollingSummaryDraft, ConsolidationError> {
    Err(ConsolidationError::EmptyWindow)
}
