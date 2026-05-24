//! WAL state machine, step graphs, and recovery decisions (brief §5.6).
//!
//! This module is **pure** — no I/O, no workspace dependencies, no
//! `unsafe`. It defines the `OpState` / `StepState` finite-state machines
//! that mirror the `SQLite` triggers in
//! `cairn-store-sqlite/src/migrations/sql/0002_wal.sql`, the static step
//! graphs for P0 mutation kinds (`upsert`, `forget_record`, `expire`), and
//! the [`recover::decide_recovery`] function the boot-recovery routine
//! consults.
//!
//! Step bodies (the actual mutation side-effects) are not in this module —
//! they live in `cairn-store-sqlite::wal::runner` and are owned by sibling
//! issues #57 and #58. This module is only the FSM and decision layer.

pub mod fsm;
pub mod idempotency;
pub mod recover;
pub mod step_graph;

// Re-exports populated as Tasks 2–5 land.
pub use fsm::{
    OpState, StepState, is_terminal_op, is_terminal_step, legal_op_transition,
    legal_step_transition,
};
pub use idempotency::{OperationId, OperationIdError, StepKey, StepLog, StepLogError};
pub use recover::{MAX_STEP_ATTEMPTS, OpSnapshot, RecoveryDecision, StepRow, decide_recovery};
pub use step_graph::{
    EVOLVE_STEPS, EXPIRE_STEPS, FORGET_RECORD_STEPS, StepDef, StepGraph, UPSERT_STEPS, WalKind,
    graph_for,
};
