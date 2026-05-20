//! Minimum-path `EvaluationWorkflow` (issue #91, brief §15).
//!
//! See [`handler::EvaluationHandler`] for the entry point and
//! [`golden_check::GoldenCheck`] for the pluggable check contract.

pub mod golden_check;
pub mod handler;
pub mod payload;

pub use golden_check::{
    CheckOutcome, GoldenCheck, OrphanCheck, TombstoneConsistencyCheck, default_checks,
};
pub use handler::{EVALUATION_KIND, EvaluationHandler, EvaluationReport};
pub use payload::EvaluationPayload;
