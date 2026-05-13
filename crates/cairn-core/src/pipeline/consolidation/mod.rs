//! Pure functions for the rolling-summary `ConsolidationWorkflow`
//! (brief §5.3 + §10.0). I/O happens in `cairn-workflows`; this module
//! is deterministic and contract-free.

pub mod draft;
pub mod errors;
pub mod window;

pub use draft::{RollingSummaryDraft, SummaryStatus, compute_rolling_summary};
pub use errors::ConsolidationError;
pub use window::{TurnHeader, WindowSelection, pick_window};
