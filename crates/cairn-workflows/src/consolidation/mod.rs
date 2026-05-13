//! Rolling-summary `ConsolidationWorkflow` (issue #90, brief §5.3, §10.0).

pub mod handler;
pub mod payload;

pub use handler::{ConsolidationHandler, CONSOLIDATION_KIND};
pub use payload::ConsolidationPayload;
