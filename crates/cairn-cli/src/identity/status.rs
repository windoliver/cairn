//! Identity status reporting: reconciliation sweep and point-in-time snapshot
//! (issue #50, D5+).

// Re-export so `mod.rs` can refer to `status::ReconciliationReport` without a
// full path to `cairn_core`.
pub use cairn_core::domain::identity::status::ReconciliationReport;
