//! Pure WAL FSM transition validators (brief §5.6).
//!
//! Mirrors the `SQLite` triggers in
//! `cairn-store-sqlite/src/migrations/sql/0002_wal.sql` byte-for-byte.
//! The cross-validation proptest in
//! `crates/cairn-store-sqlite/tests/wal_fsm_cross_validation.rs` runs both
//! sides over the same input space and asserts equivalence.

/// Operation-level state. Mirrors the `wal_ops.state` CHECK constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum OpState {
    /// Inserted, not yet validated. Persisted only on a daemon crash
    /// between insert and `prepare`. Recovery finalizes as `Rejected`.
    Issued,
    /// Validated; locks acquired; about to apply side-effects.
    Prepared,
    /// All side-effects committed durably.
    Committed,
    /// Compensated and abandoned.
    Aborted,
    /// Validation failed; never applied.
    Rejected,
}

impl OpState {
    /// Wire form used by `wal_ops.state` and FSM trigger.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Issued => "ISSUED",
            Self::Prepared => "PREPARED",
            Self::Committed => "COMMITTED",
            Self::Aborted => "ABORTED",
            Self::Rejected => "REJECTED",
        }
    }
}

/// Step-level state. Mirrors the `wal_steps.state` CHECK constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StepState {
    /// Started or queued.
    Pending,
    /// Side-effect committed.
    Done,
    /// Side-effect raised an error; retry until `MAX_STEP_ATTEMPTS`.
    Failed,
    /// Side-effect was reversed by compensation.
    Compensated,
}

impl StepState {
    /// Wire form used by `wal_steps.state`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::Done => "DONE",
            Self::Failed => "FAILED",
            Self::Compensated => "COMPENSATED",
        }
    }
}

/// Returns true if `from -> to` is allowed by the §5.6 op FSM.
///
/// Same set as the `SQLite` `wal_ops_state_transition` trigger:
/// `ISSUED -> {PREPARED, REJECTED}` and `PREPARED -> {COMMITTED, ABORTED}`.
/// Same-state writes (`from == to`) are *not* transitions; the `SQLite` trigger
/// only fires on `NEW.state IS NOT OLD.state`. To match, this function returns
/// `true` for `from == to`.
#[must_use]
pub fn legal_op_transition(from: OpState, to: OpState) -> bool {
    use OpState::{Aborted, Committed, Issued, Prepared, Rejected};
    if from == to {
        return true;
    }
    matches!(
        (from, to),
        (Issued, Prepared | Rejected) | (Prepared, Committed | Aborted)
    )
}

/// Returns true if `from -> to` is allowed by the §5.6 step FSM.
///
/// Same set as the `SQLite` `wal_steps_state_transition` trigger:
/// `PENDING -> {DONE, FAILED}`, `FAILED -> {PENDING, COMPENSATED}`,
/// `DONE -> COMPENSATED`. Same-state writes return `true` (see
/// [`legal_op_transition`] for why).
#[must_use]
pub fn legal_step_transition(from: StepState, to: StepState) -> bool {
    use StepState::{Compensated, Done, Failed, Pending};
    if from == to {
        return true;
    }
    matches!(
        (from, to),
        (Pending, Done | Failed)
            | (Failed, Pending | Compensated)
            | (Done, Compensated)
    )
}

/// Returns true if `state` is a terminal `wal_ops.state` (no further
/// transitions are legal).
#[must_use]
pub fn is_terminal_op(state: OpState) -> bool {
    matches!(state, OpState::Committed | OpState::Aborted | OpState::Rejected)
}

/// Returns true if `state` is a terminal `wal_steps.state`.
///
/// `Failed` is *not* terminal — it can transition to `Pending` (retry) or
/// `Compensated` (rollback). `Done` and `Compensated` are terminal.
#[must_use]
pub fn is_terminal_step(state: StepState) -> bool {
    matches!(state, StepState::Done | StepState::Compensated)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every op transition the §5.6 trigger allows, paired with the
    /// expected output of `legal_op_transition`.
    const OP_MATRIX: &[(OpState, OpState, bool)] = &[
        // From ISSUED
        (OpState::Issued, OpState::Issued, true),
        (OpState::Issued, OpState::Prepared, true),
        (OpState::Issued, OpState::Committed, false),
        (OpState::Issued, OpState::Aborted, false),
        (OpState::Issued, OpState::Rejected, true),
        // From PREPARED
        (OpState::Prepared, OpState::Issued, false),
        (OpState::Prepared, OpState::Prepared, true),
        (OpState::Prepared, OpState::Committed, true),
        (OpState::Prepared, OpState::Aborted, true),
        (OpState::Prepared, OpState::Rejected, false),
        // From COMMITTED (terminal)
        (OpState::Committed, OpState::Issued, false),
        (OpState::Committed, OpState::Prepared, false),
        (OpState::Committed, OpState::Committed, true),
        (OpState::Committed, OpState::Aborted, false),
        (OpState::Committed, OpState::Rejected, false),
        // From ABORTED (terminal)
        (OpState::Aborted, OpState::Issued, false),
        (OpState::Aborted, OpState::Prepared, false),
        (OpState::Aborted, OpState::Committed, false),
        (OpState::Aborted, OpState::Aborted, true),
        (OpState::Aborted, OpState::Rejected, false),
        // From REJECTED (terminal)
        (OpState::Rejected, OpState::Issued, false),
        (OpState::Rejected, OpState::Prepared, false),
        (OpState::Rejected, OpState::Committed, false),
        (OpState::Rejected, OpState::Aborted, false),
        (OpState::Rejected, OpState::Rejected, true),
    ];

    #[test]
    fn op_transition_matrix_matches_5_6() {
        for &(from, to, expected) in OP_MATRIX {
            assert_eq!(
                legal_op_transition(from, to),
                expected,
                "{from:?} -> {to:?}"
            );
        }
    }

    /// Every step transition the §5.6 trigger allows.
    const STEP_MATRIX: &[(StepState, StepState, bool)] = &[
        // From PENDING
        (StepState::Pending, StepState::Pending, true),
        (StepState::Pending, StepState::Done, true),
        (StepState::Pending, StepState::Failed, true),
        (StepState::Pending, StepState::Compensated, false),
        // From DONE
        (StepState::Done, StepState::Pending, false),
        (StepState::Done, StepState::Done, true),
        (StepState::Done, StepState::Failed, false),
        (StepState::Done, StepState::Compensated, true),
        // From FAILED
        (StepState::Failed, StepState::Pending, true),
        (StepState::Failed, StepState::Done, false),
        (StepState::Failed, StepState::Failed, true),
        (StepState::Failed, StepState::Compensated, true),
        // From COMPENSATED (terminal)
        (StepState::Compensated, StepState::Pending, false),
        (StepState::Compensated, StepState::Done, false),
        (StepState::Compensated, StepState::Failed, false),
        (StepState::Compensated, StepState::Compensated, true),
    ];

    #[test]
    fn step_transition_matrix_matches_5_6() {
        for &(from, to, expected) in STEP_MATRIX {
            assert_eq!(
                legal_step_transition(from, to),
                expected,
                "{from:?} -> {to:?}"
            );
        }
    }

    #[test]
    fn op_terminals() {
        assert!(!is_terminal_op(OpState::Issued));
        assert!(!is_terminal_op(OpState::Prepared));
        assert!(is_terminal_op(OpState::Committed));
        assert!(is_terminal_op(OpState::Aborted));
        assert!(is_terminal_op(OpState::Rejected));
    }

    #[test]
    fn step_terminals() {
        assert!(!is_terminal_step(StepState::Pending));
        assert!(is_terminal_step(StepState::Done));
        // Failed is retryable — NOT terminal.
        assert!(!is_terminal_step(StepState::Failed));
        assert!(is_terminal_step(StepState::Compensated));
    }

    #[test]
    fn op_state_wire_form() {
        assert_eq!(OpState::Issued.as_str(), "ISSUED");
        assert_eq!(OpState::Prepared.as_str(), "PREPARED");
        assert_eq!(OpState::Committed.as_str(), "COMMITTED");
        assert_eq!(OpState::Aborted.as_str(), "ABORTED");
        assert_eq!(OpState::Rejected.as_str(), "REJECTED");
    }

    #[test]
    fn step_state_wire_form() {
        assert_eq!(StepState::Pending.as_str(), "PENDING");
        assert_eq!(StepState::Done.as_str(), "DONE");
        assert_eq!(StepState::Failed.as_str(), "FAILED");
        assert_eq!(StepState::Compensated.as_str(), "COMPENSATED");
    }
}
