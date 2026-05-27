//! Workflow domain types — progress events and workflow identifiers for
//! long-running admin operations (backfill, consolidate, promote, expire,
//! evaluate).
//!
//! Brief §5 (pipeline) / §10 (workflows).

pub mod progress;

pub use progress::{ProgressEvent, ProgressKind, WorkflowId};
