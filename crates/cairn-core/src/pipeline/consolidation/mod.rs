//! Pure functions for the rolling-summary `ConsolidationWorkflow`
//! (brief §5.3 + §10.0). I/O happens in `cairn-workflows`; this module
//! is deterministic and contract-free.

pub mod draft;
pub mod errors;
pub mod window;

pub use draft::{compute_rolling_summary, RollingSummaryDraft, SummaryStatus};
pub use errors::ConsolidationError;
pub use window::{pick_window, TurnHeader, WindowSelection};
