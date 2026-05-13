//! Rolling-summary `ConsolidationWorkflow` (issue #90, brief §5.3, §10.0).

pub mod forget_cleanup;
pub mod handler;
pub mod payload;
pub mod trigger;

pub use forget_cleanup::{
    ConsolidationForgetCleanupHandler, FORGET_CLEANUP_KIND, ForgetCleanupPayload,
};
pub use handler::{CONSOLIDATION_KIND, ConsolidationHandler};
pub use payload::ConsolidationPayload;
pub use trigger::{EnqueueDecision, enqueue_if_due};
