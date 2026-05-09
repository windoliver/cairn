//! Pure `decide_recovery` function — given a WAL op snapshot, decides what
//! the boot-recovery routine should do next (brief §5.6 "Boot-time
//! recovery").
//!
//! No I/O. The store-side wrapper loads the snapshot, calls
//! [`decide_recovery`], and applies the returned [`RecoveryDecision`].

use crate::wal::fsm::{OpState, StepState};
use crate::wal::step_graph::{StepGraph, WalKind, graph_for};

/// Maximum number of attempts a single step may make before recovery
/// transitions the op to ABORTED. Brief §5.6 retry policy: max 3 attempts,
/// 100 ms / 400 ms / 1600 ms backoff.
pub const MAX_STEP_ATTEMPTS: u32 = 3;

/// One row from `wal_steps`, projected for the recovery decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRow {
    /// `wal_steps.step_ord`.
    pub ord: u32,
    /// `wal_steps.state`.
    pub state: StepState,
    /// `wal_steps.attempts`.
    pub attempts: u32,
    /// `wal_steps.last_error`. Carried for telemetry only — `decide_recovery`
    /// does not read it.
    pub last_error: Option<String>,
}

/// Snapshot of one `wal_ops` row plus its `wal_steps` rows.
#[derive(Debug, Clone)]
pub struct OpSnapshot {
    /// `wal_ops.kind` parsed.
    pub kind: WalKind,
    /// `wal_ops.state` parsed.
    pub state: OpState,
    /// `wal_steps` rows for this op. **Must be sorted by `ord` ascending**.
    pub steps: Vec<StepRow>,
}

/// What the boot-recovery routine should do with one snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecoveryDecision {
    /// Op is terminal; nothing to do. Idempotent on re-entry.
    NoOp,
    /// Op is `ISSUED` with no progress; finalize as `REJECTED` because at
    /// P0 the same-txn collapse means a persisted `ISSUED` can only result
    /// from a daemon crash before `PREPARE` — there is no recovery path
    /// that produces side-effects from an `ISSUED` op.
    FinalizeRejected,
    /// Op is `PREPARED`, every step is `DONE`; flip to `COMMITTED`.
    FinalizeCommitted,
    /// Op is `PREPARED`; resume from step `next_ord` (or 0 if no DONE rows
    /// exist).
    Resume {
        /// First step ord that still needs to run.
        next_ord: u32,
    },
    /// A step exhausted its retry budget (`attempts >= MAX_STEP_ATTEMPTS`
    /// and last state is `FAILED`). Abort and run compensations.
    AbortAndCompensate {
        /// Ord of the step that exhausted retries.
        failed_ord: u32,
    },
}

/// Decides what recovery should do for one op.
///
/// # Decision rules (brief §5.6)
///
/// | Input state | Step pattern                                 | Output                         |
/// |-------------|----------------------------------------------|--------------------------------|
/// | terminal    | (any)                                        | `NoOp`                         |
/// | `Issued`    | (any)                                        | `FinalizeRejected`             |
/// | `Prepared`  | a step has `Failed` with `attempts >= MAX`   | `AbortAndCompensate{ord}`      |
/// | `Prepared`  | every graph step has a `Done` row            | `FinalizeCommitted`            |
/// | `Prepared`  | otherwise                                    | `Resume{next_ord}`             |
///
/// `next_ord` = the first ord in `0..total` without a `Done` row, or `0`
/// if no `Done` rows exist. This contiguous-coverage interpretation of
/// brief §5.6 step 4 (`resume at step:(last_done + 1)`) is robust to
/// degenerate inputs (duplicate ords, gaps): a gap means an earlier step
/// must be re-run before later DONE rows are honored. `Failed` rows below
/// `MAX` and `Pending` rows do not advance `next_ord` — they will be
/// retried in place.
///
/// `expires_at` is intentionally NOT consulted: brief §5.6 specifies that
/// TTL applies to new external requests, not to WAL recovery.
#[must_use]
pub fn decide_recovery(snapshot: &OpSnapshot) -> RecoveryDecision {
    use OpState::{Aborted, Committed, Issued, Prepared, Rejected};

    match snapshot.state {
        Committed | Aborted | Rejected => RecoveryDecision::NoOp,
        Issued => RecoveryDecision::FinalizeRejected,
        Prepared => decide_prepared(snapshot),
    }
}

fn decide_prepared(snapshot: &OpSnapshot) -> RecoveryDecision {
    // Abort takes priority over all forward progress: any step that
    // exhausted retries finalizes the op.
    if let Some(failed_ord) = snapshot
        .steps
        .iter()
        .find(|s| s.state == StepState::Failed && s.attempts >= MAX_STEP_ATTEMPTS)
        .map(|s| s.ord)
    {
        return RecoveryDecision::AbortAndCompensate { failed_ord };
    }

    let graph: &StepGraph = graph_for(snapshot.kind);
    // graph.len() <= 7 for every graph in this scaffold; cast is safe.
    #[allow(clippy::cast_possible_truncation)] // see graph_for invariant
    let total = graph.len() as u32;

    // Collect DONE ords into a BTreeSet — naturally dedupes duplicate rows
    // and lets us scan the contiguous 0..total range for the first ord
    // that does not have a DONE row.
    let done: std::collections::BTreeSet<u32> = snapshot
        .steps
        .iter()
        .filter(|s| s.state == StepState::Done)
        .map(|s| s.ord)
        .collect();

    // First ord in [0, total) without a DONE row, or None if every ord is
    // covered. `find` short-circuits on the first miss.
    match (0..total).find(|ord| !done.contains(ord)) {
        None => RecoveryDecision::FinalizeCommitted,
        Some(next_ord) => RecoveryDecision::Resume { next_ord },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(state: OpState, steps: Vec<StepRow>) -> OpSnapshot {
        OpSnapshot {
            kind: WalKind::Upsert,
            state,
            steps,
        }
    }

    fn step(ord: u32, state: StepState, attempts: u32) -> StepRow {
        StepRow {
            ord,
            state,
            attempts,
            last_error: None,
        }
    }

    #[test]
    fn terminal_committed_is_noop() {
        let s = snap(OpState::Committed, vec![]);
        assert_eq!(decide_recovery(&s), RecoveryDecision::NoOp);
    }

    #[test]
    fn terminal_aborted_is_noop() {
        let s = snap(OpState::Aborted, vec![]);
        assert_eq!(decide_recovery(&s), RecoveryDecision::NoOp);
    }

    #[test]
    fn terminal_rejected_is_noop() {
        let s = snap(OpState::Rejected, vec![]);
        assert_eq!(decide_recovery(&s), RecoveryDecision::NoOp);
    }

    #[test]
    fn issued_finalizes_rejected() {
        let s = snap(OpState::Issued, vec![]);
        assert_eq!(decide_recovery(&s), RecoveryDecision::FinalizeRejected);
    }

    #[test]
    fn prepared_no_steps_resumes_from_zero() {
        let s = snap(OpState::Prepared, vec![]);
        assert_eq!(
            decide_recovery(&s),
            RecoveryDecision::Resume { next_ord: 0 }
        );
    }

    #[test]
    fn prepared_partial_resumes_after_last_done() {
        // 0,1 DONE, 2 absent — resume from 2.
        let s = snap(
            OpState::Prepared,
            vec![step(0, StepState::Done, 1), step(1, StepState::Done, 1)],
        );
        assert_eq!(
            decide_recovery(&s),
            RecoveryDecision::Resume { next_ord: 2 }
        );
    }

    #[test]
    fn prepared_all_done_finalizes_committed() {
        // upsert has 6 steps (ords 0..=5).
        let s = snap(
            OpState::Prepared,
            (0..6).map(|o| step(o, StepState::Done, 1)).collect(),
        );
        assert_eq!(decide_recovery(&s), RecoveryDecision::FinalizeCommitted);
    }

    #[test]
    fn prepared_failed_under_max_resumes() {
        // step 1 FAILED with 1 attempt — under MAX, treat as in-flight.
        // Last DONE is 0, so next_ord = 1.
        let s = snap(
            OpState::Prepared,
            vec![step(0, StepState::Done, 1), step(1, StepState::Failed, 1)],
        );
        assert_eq!(
            decide_recovery(&s),
            RecoveryDecision::Resume { next_ord: 1 }
        );
    }

    #[test]
    fn prepared_failed_at_max_aborts() {
        let s = snap(
            OpState::Prepared,
            vec![
                step(0, StepState::Done, 1),
                step(1, StepState::Failed, MAX_STEP_ATTEMPTS),
            ],
        );
        assert_eq!(
            decide_recovery(&s),
            RecoveryDecision::AbortAndCompensate { failed_ord: 1 }
        );
    }

    #[test]
    fn prepared_failed_above_max_aborts() {
        let s = snap(
            OpState::Prepared,
            vec![step(0, StepState::Failed, MAX_STEP_ATTEMPTS + 1)],
        );
        assert_eq!(
            decide_recovery(&s),
            RecoveryDecision::AbortAndCompensate { failed_ord: 0 }
        );
    }

    #[test]
    fn prepared_with_gap_in_done_steps_resumes_at_first_missing_ord() {
        // Steps 0, 1, 3 DONE; step 2 is absent. Brief §5.6 says recovery
        // resumes at "step:(last_done + 1)" — interpreted as the first ord
        // without a DONE row, NOT max(done) + 1. The runner then runs step 2
        // (and 3 again is skipped as already DONE) before continuing.
        let s = snap(
            OpState::Prepared,
            vec![
                step(0, StepState::Done, 1),
                step(1, StepState::Done, 1),
                step(3, StepState::Done, 1),
            ],
        );
        assert_eq!(
            decide_recovery(&s),
            RecoveryDecision::Resume { next_ord: 2 }
        );
    }

    #[test]
    fn prepared_with_duplicate_done_rows_does_not_finalize_falsely() {
        // 6 DONE rows that all reference ord 0 — a buggy upstream projection
        // could in principle produce this. Naive count-based logic would say
        // "6 == graph.len() so FinalizeCommitted"; the BTreeSet dedup catches
        // it and reports first-missing = 1.
        let s = snap(
            OpState::Prepared,
            (0..6).map(|_| step(0, StepState::Done, 1)).collect(),
        );
        assert_eq!(
            decide_recovery(&s),
            RecoveryDecision::Resume { next_ord: 1 }
        );
    }

    #[test]
    fn pending_under_max_resumes_in_place() {
        let s = snap(
            OpState::Prepared,
            vec![step(0, StepState::Done, 1), step(1, StepState::Pending, 1)],
        );
        assert_eq!(
            decide_recovery(&s),
            RecoveryDecision::Resume { next_ord: 1 }
        );
    }
}
