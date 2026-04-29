//! Identity status reporting: reconciliation sweep and point-in-time snapshot
//! (issue #50, D5+, D9).

// Re-exports so `mod.rs` and callers can refer to these types without a
// full path to `cairn_core`.
pub use cairn_core::domain::identity::status::{
    IdentityStatusReport, MismatchCheckOutcome, ReconciliationReport,
};
