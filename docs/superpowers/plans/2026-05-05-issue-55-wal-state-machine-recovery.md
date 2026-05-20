# Issue #55 — WAL State Machine, Step Markers & Boot Recovery: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the scaffold for the §5.6 WAL state machine, per-step `wal_steps` markers, and boot-recovery — pure FSM and decision logic in `cairn-core`, `StepRunner` and `recover_pending` in `cairn-store-sqlite`. No side-effect step bodies (those are #57/#58).

**Architecture:** `cairn-core/src/wal/` holds zero-I/O FSM types, static step graphs, and a pure `decide_recovery` function. `cairn-store-sqlite/src/wal/{runner,recovery}.rs` persists `wal_steps` rows and drives recovery against a real SQLite DB. The scaffold ships in *decision-only* mode: ops in terminal-leaning states (`Committed`/`Aborted`/`Rejected` and PREPARED-with-all-DONE) are finalized without bodies; `Resume`/`AbortAndCompensate` cases log a warn until #57/#58 wire bodies in.

**Tech Stack:** Rust 1.95.0, Cargo workspace, `tokio_rusqlite` async SQLite, `thiserror` (lib errors), `proptest` + `rstest` (tests), `tracing` (observability).

**Spec:** `docs/superpowers/specs/2026-05-05-issue-55-wal-state-machine-recovery-design.md`

---

## File Map

**Created:**
- `crates/cairn-core/src/wal/mod.rs` — re-exports
- `crates/cairn-core/src/wal/fsm.rs` — `OpState`, `StepState`, transition validators
- `crates/cairn-core/src/wal/idempotency.rs` — `OperationId`, `StepKey`, `StepLog` trait
- `crates/cairn-core/src/wal/step_graph.rs` — `WalKind`, `StepDef`, `StepGraph`, static graphs
- `crates/cairn-core/src/wal/recover.rs` — `OpSnapshot`, `StepRow`, `RecoveryDecision`, `decide_recovery`
- `crates/cairn-store-sqlite/src/wal/runner.rs` — `StepRunner`, `StepBody`, retry policy
- `crates/cairn-store-sqlite/src/wal/recovery.rs` — `recover_pending`, `RecoveryConfig`, `RecoveryReport`, `StepBodyRegistry`
- `crates/cairn-store-sqlite/tests/wal_recovery.rs` — 8 integration scenarios
- `crates/cairn-store-sqlite/tests/wal_fsm_cross_validation.rs` — pure-fn vs SQLite-trigger equivalence

**Modified:**
- `crates/cairn-core/src/lib.rs` — add `pub mod wal;`
- `crates/cairn-store-sqlite/src/wal/mod.rs` — add `pub mod runner; pub mod recovery;` and re-exports
- `crates/cairn-store-sqlite/src/open.rs` — invoke `recover_pending` after migrations (decision-only default)
- `crates/cairn-store-sqlite/src/lib.rs` — re-export `RecoveryConfig`, `RecoveryReport`

---

## Task 1: Scaffold `cairn-core/src/wal/` module

**Files:**
- Create: `crates/cairn-core/src/wal/mod.rs`
- Modify: `crates/cairn-core/src/lib.rs`

- [ ] **Step 1.1: Create the wal module skeleton**

Create `crates/cairn-core/src/wal/mod.rs`:

```rust
//! WAL state machine, step graphs, and recovery decisions (brief §5.6).
//!
//! This module is **pure** — no I/O, no workspace dependencies, no
//! `unsafe`. It defines the `OpState` / `StepState` finite-state machines
//! that mirror the SQLite triggers in
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

pub use fsm::{OpState, StepState, is_terminal_op, is_terminal_step, legal_op_transition, legal_step_transition};
pub use idempotency::{OperationId, OperationIdError, StepKey, StepLog, StepLogError};
pub use recover::{MAX_STEP_ATTEMPTS, OpSnapshot, RecoveryDecision, StepRow, decide_recovery};
pub use step_graph::{EXPIRE_STEPS, FORGET_RECORD_STEPS, StepDef, StepGraph, UPSERT_STEPS, WalKind, graph_for};
```

- [ ] **Step 1.2: Create empty submodule files**

Create the four files with just a doc comment so `mod.rs` compiles. We'll fill them in tasks 2–5.

`crates/cairn-core/src/wal/fsm.rs`:
```rust
//! Pure WAL FSM transition validators (brief §5.6).
```

`crates/cairn-core/src/wal/idempotency.rs`:
```rust
//! WAL idempotency primitives — operation IDs, step keys, and the
//! `StepLog` contract the SQLite adapter implements.
```

`crates/cairn-core/src/wal/step_graph.rs`:
```rust
//! Static step-graph definitions for P0 WAL operation kinds (brief §5.6).
```

`crates/cairn-core/src/wal/recover.rs`:
```rust
//! Pure `decide_recovery` function — given a WAL op snapshot, decides what
//! the boot-recovery routine should do next (brief §5.6 "Boot-time
//! recovery").
```

- [ ] **Step 1.3: Register the wal module in `lib.rs`**

Modify `crates/cairn-core/src/lib.rs` — add `pub mod wal;` to the module list (alphabetical placement after `verifier`):

```rust
//! Cairn core — contract traits, domain types, and error enums.
//!
//! P0 scaffold. Verb behaviour, domain types, and error enums land in
//! follow-up issues (#4, #34, #35). Core depends on no adapter crate.
//!
//! The `generated` submodule is produced by `cairn-codegen` from the IDL and
//! must not be hand-edited — see `docs/dev/codegen.md`.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod config;
pub mod contract;
pub mod domain;
pub mod error;
pub mod generated;
pub mod pipeline;
pub mod policy_trace;
pub mod search;
pub mod verbs;
pub mod verifier;
pub mod wal;
```

The five export names referenced in `wal/mod.rs` (`OpState`, `StepState`, etc.) don't exist yet, so this WILL fail to compile. That's expected — Tasks 2–5 add the items.

- [ ] **Step 1.4: Comment out the re-exports temporarily**

So `cargo check` passes for the scaffold step. Replace the `pub use` block in `wal/mod.rs` with:

```rust
// Re-exports populated as Tasks 2–5 land.
// pub use fsm::{...};
// pub use idempotency::{...};
// pub use recover::{...};
// pub use step_graph::{...};
```

- [ ] **Step 1.5: Verify the crate compiles**

```bash
cargo check -p cairn-core --locked
```

Expected: `Finished` with no errors. (Warnings about unused empty modules are OK.)

- [ ] **Step 1.6: Verify the core boundary**

```bash
./scripts/check-core-boundary.sh
```

Expected: exits 0 (no new workspace deps were added).

- [ ] **Step 1.7: Commit**

```bash
git add crates/cairn-core/src/wal/ crates/cairn-core/src/lib.rs
git commit -m "feat(core): scaffold wal/ module (issue #55, brief §5.6)"
```

---

## Task 2: FSM transition validators (`cairn-core/src/wal/fsm.rs`)

**Files:**
- Modify: `crates/cairn-core/src/wal/fsm.rs`

- [ ] **Step 2.1: Write the failing tests**

Replace the contents of `crates/cairn-core/src/wal/fsm.rs` with:

```rust
//! Pure WAL FSM transition validators (brief §5.6).
//!
//! Mirrors the SQLite triggers in
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
/// Same set as the SQLite `wal_ops_state_transition` trigger:
/// `ISSUED -> {PREPARED, REJECTED}` and `PREPARED -> {COMMITTED, ABORTED}`.
/// Same-state writes (`from == to`) are *not* transitions; the SQLite trigger
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
/// Same set as the SQLite `wal_steps_state_transition` trigger:
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
```

- [ ] **Step 2.2: Run tests, verify they pass**

```bash
cargo nextest run -p cairn-core wal::fsm --locked
```

Expected: 6 tests pass (`op_transition_matrix_matches_5_6`, `step_transition_matrix_matches_5_6`, `op_terminals`, `step_terminals`, `op_state_wire_form`, `step_state_wire_form`).

- [ ] **Step 2.3: Activate the re-export in `wal/mod.rs`**

Edit `crates/cairn-core/src/wal/mod.rs` — uncomment the `fsm` export:

```rust
pub use fsm::{OpState, StepState, is_terminal_op, is_terminal_step, legal_op_transition, legal_step_transition};
```

- [ ] **Step 2.4: Verify the crate compiles**

```bash
cargo check -p cairn-core --locked
```

Expected: clean compile.

- [ ] **Step 2.5: Commit**

```bash
git add crates/cairn-core/src/wal/fsm.rs crates/cairn-core/src/wal/mod.rs
git commit -m "feat(core): WAL FSM enums + transition validators (issue #55)"
```

---

## Task 3: Idempotency primitives (`cairn-core/src/wal/idempotency.rs`)

**Files:**
- Modify: `crates/cairn-core/src/wal/idempotency.rs`
- Modify: `crates/cairn-core/src/wal/mod.rs`

- [ ] **Step 3.1: Implement `OperationId` newtype, `StepKey`, and `StepLog` trait**

Replace `crates/cairn-core/src/wal/idempotency.rs` with:

```rust
//! WAL idempotency primitives — operation IDs, step keys, and the
//! `StepLog` contract the SQLite adapter implements.

use std::fmt;

use thiserror::Error;

use crate::wal::fsm::StepState;

/// Errors from [`OperationId::parse`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OperationIdError {
    /// Empty input.
    #[error("operation id must not be empty")]
    Empty,
    /// Input exceeded `OperationId::MAX_LEN` characters.
    #[error("operation id exceeds maximum length ({MAX_LEN})", MAX_LEN = OperationId::MAX_LEN)]
    TooLong,
}

/// Idempotency key for a WAL op. ULID-shaped strings produced by the issuer
/// (e.g. `op-01HQZ...`); we don't enforce ULID format here because the
/// `lint_repair` helper produces UUID-flavoured ids and graph helpers use
/// their own scheme. The only invariants are non-empty and length-bounded.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct OperationId(String);

impl OperationId {
    /// Maximum length in characters. Generous; matches `wal_ops.operation_id
    /// TEXT NOT NULL` with no DB-side cap.
    pub const MAX_LEN: usize = 128;

    /// Parse a string as an `OperationId`.
    ///
    /// # Errors
    /// - [`OperationIdError::Empty`] if `raw` is empty.
    /// - [`OperationIdError::TooLong`] if `raw` exceeds [`Self::MAX_LEN`].
    pub fn parse(raw: impl Into<String>) -> Result<Self, OperationIdError> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(OperationIdError::Empty);
        }
        if raw.chars().count() > Self::MAX_LEN {
            return Err(OperationIdError::TooLong);
        }
        Ok(Self(raw))
    }

    /// Borrow the wire string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume into the underlying `String`.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Debug for OperationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("OperationId").field(&self.0).finish()
    }
}

impl fmt::Display for OperationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Composite key identifying one row in `wal_steps`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StepKey {
    /// FK into `wal_ops.operation_id`.
    pub operation_id: OperationId,
    /// 0-based step ordinal (matches `wal_steps.step_ord`).
    pub step_ord: u32,
}

impl StepKey {
    /// Construct a new `StepKey`.
    #[must_use]
    pub fn new(operation_id: OperationId, step_ord: u32) -> Self {
        Self { operation_id, step_ord }
    }
}

/// Errors from [`StepLog`] implementors.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StepLogError {
    /// Underlying storage failed.
    #[error("step log storage error: {0}")]
    Storage(String),
    /// A transition rejected by the §5.6 step FSM (or the SQLite trigger
    /// that enforces it).
    #[error("illegal step transition for {key:?}")]
    IllegalTransition {
        /// The key whose transition was rejected.
        key: StepKey,
    },
}

/// Contract the SQLite adapter implements; pure-test impls live in
/// `cairn-test-fixtures` (added in a follow-up).
///
/// The trait deliberately exposes only the four methods the runner needs.
/// Reading the full step set (for recovery) is on the recovery path, not
/// the runner path, and uses a separate adapter API.
pub trait StepLog {
    /// Returns the current state of the step, or `None` if no row exists.
    ///
    /// # Errors
    /// Storage failure.
    fn step_state(&self, key: &StepKey) -> Result<Option<StepState>, StepLogError>;

    /// Mark the step as `PENDING` and increment its `attempts` counter.
    /// Inserts the row if absent.
    ///
    /// # Errors
    /// Storage failure or illegal transition.
    fn record_attempt(&mut self, key: &StepKey, attempt: u32) -> Result<(), StepLogError>;

    /// Mark the step as `DONE`.
    ///
    /// # Errors
    /// Storage failure or illegal transition.
    fn record_done(&mut self, key: &StepKey) -> Result<(), StepLogError>;

    /// Mark the step as `FAILED` with the given error message.
    ///
    /// # Errors
    /// Storage failure or illegal transition.
    fn record_failed(&mut self, key: &StepKey, err: &str) -> Result<(), StepLogError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rejects_empty() {
        assert!(matches!(
            OperationId::parse(""),
            Err(OperationIdError::Empty)
        ));
    }

    #[test]
    fn parse_rejects_too_long() {
        let too_long: String = "x".repeat(OperationId::MAX_LEN + 1);
        assert!(matches!(
            OperationId::parse(too_long),
            Err(OperationIdError::TooLong)
        ));
    }

    #[test]
    fn parse_accepts_typical_op_id() {
        let id = OperationId::parse("op-01HQZ12345").expect("typical id parses");
        assert_eq!(id.as_str(), "op-01HQZ12345");
    }

    #[test]
    fn parse_accepts_max_len() {
        let max: String = "x".repeat(OperationId::MAX_LEN);
        assert!(OperationId::parse(max).is_ok());
    }

    #[test]
    fn step_key_eq_round_trip() {
        let a = StepKey::new(OperationId::parse("op-1").expect("ok"), 3);
        let b = StepKey::new(OperationId::parse("op-1").expect("ok"), 3);
        assert_eq!(a, b);
    }
}
```

- [ ] **Step 3.2: Run tests**

```bash
cargo nextest run -p cairn-core wal::idempotency --locked
```

Expected: 5 tests pass.

- [ ] **Step 3.3: Activate the idempotency re-export**

Edit `crates/cairn-core/src/wal/mod.rs` — uncomment the idempotency export:

```rust
pub use idempotency::{OperationId, OperationIdError, StepKey, StepLog, StepLogError};
```

- [ ] **Step 3.4: Verify**

```bash
cargo check -p cairn-core --locked
```

Expected: clean compile.

- [ ] **Step 3.5: Commit**

```bash
git add crates/cairn-core/src/wal/idempotency.rs crates/cairn-core/src/wal/mod.rs
git commit -m "feat(core): WAL idempotency primitives — OperationId, StepKey, StepLog (issue #55)"
```

---

## Task 4: Static step graphs (`cairn-core/src/wal/step_graph.rs`)

**Files:**
- Modify: `crates/cairn-core/src/wal/step_graph.rs`
- Modify: `crates/cairn-core/src/wal/mod.rs`

- [ ] **Step 4.1: Implement `WalKind`, `StepDef`, `StepGraph`, and the static graphs**

Replace `crates/cairn-core/src/wal/step_graph.rs` with:

```rust
//! Static step-graph definitions for P0 WAL operation kinds (brief §5.6).
//!
//! Step names mirror the brief §5.6 fan-out tables. They are written into
//! `wal_steps.step_kind` and are wire-stable: renaming a step requires a
//! schema migration to map old names forward.

/// P0 mutation kinds whose step graphs this module defines. Marked
/// `#[non_exhaustive]` so `Promote`, `ForgetSession`, `Evolve`, and the
/// graph kinds can be added later without breaking matchers.
///
/// The existing `lint_repair` kind (in
/// `cairn-store-sqlite/src/wal/lint_repair.rs`) is intentionally not part
/// of this enum yet — its migration onto the scaffold is a follow-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WalKind {
    /// `upsert` — create or update a record (brief §5.6 fan-out table row 1).
    Upsert,
    /// `forget_record` — record-level tombstone + purge (brief §5.6 row 2).
    ForgetRecord,
    /// `expire` — soft-expire (brief §5.6 row 5).
    Expire,
}

impl WalKind {
    /// Wire form used by `wal_ops.kind`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Upsert => "upsert",
            Self::ForgetRecord => "forget_record",
            Self::Expire => "expire",
        }
    }
}

/// One step in a fan-out graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepDef {
    /// 0-based ordinal; matches `wal_steps.step_ord`.
    pub ord: u32,
    /// Stable name; matches `wal_steps.step_kind`.
    pub name: &'static str,
    /// `true` ⇒ step body may be invoked more than once with the same args
    /// without producing duplicate effects (brief §5.6 `[idem]` marker).
    /// `false` ⇒ step body must run exactly once. Recovery never re-invokes
    /// a non-idempotent step that was already marked `Done`.
    pub idempotent: bool,
}

/// A complete step graph for one [`WalKind`].
#[derive(Debug, Clone, Copy)]
pub struct StepGraph {
    /// The kind this graph applies to.
    pub kind: WalKind,
    /// Steps in execution order. Indexed by `ord` matches the slice index.
    pub steps: &'static [StepDef],
}

impl StepGraph {
    /// Returns the step at `ord`, or `None` if out of range.
    #[must_use]
    pub fn step(&self, ord: u32) -> Option<&StepDef> {
        self.steps.get(ord as usize)
    }

    /// Number of steps.
    #[must_use]
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// `true` if there are no steps.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

/// Step graph for `upsert` — brief §5.6 fan-out table row 1.
///
/// Step 7 (`consent_log_materializer`) is async and not part of the WAL
/// step graph per the brief; it lives in a separate background tail.
pub const UPSERT_STEPS: &[StepDef] = &[
    StepDef { ord: 0, name: "snapshot.stage",     idempotent: false },
    StepDef { ord: 1, name: "primary.upsert_cow", idempotent: true  },
    StepDef { ord: 2, name: "vector.upsert",      idempotent: true  },
    StepDef { ord: 3, name: "fts.upsert",         idempotent: true  },
    StepDef { ord: 4, name: "edges.upsert",       idempotent: true  },
    StepDef { ord: 5, name: "primary.activate",   idempotent: true  },
];

const UPSERT_GRAPH: StepGraph = StepGraph {
    kind: WalKind::Upsert,
    steps: UPSERT_STEPS,
};

/// Step graph for `forget_record` — brief §5.6 fan-out table row 2.
pub const FORGET_RECORD_STEPS: &[StepDef] = &[
    StepDef { ord: 0, name: "primary.mark_tombstone", idempotent: true  },
    StepDef { ord: 1, name: "vector.drain",           idempotent: true  },
    StepDef { ord: 2, name: "fts.drain",              idempotent: true  },
    StepDef { ord: 3, name: "edges.drain",            idempotent: true  },
    StepDef { ord: 4, name: "primary.purge",          idempotent: false },
    StepDef { ord: 5, name: "wal.purge_pre_images",   idempotent: true  },
    StepDef { ord: 6, name: "snapshot.purge",         idempotent: true  },
];

const FORGET_RECORD_GRAPH: StepGraph = StepGraph {
    kind: WalKind::ForgetRecord,
    steps: FORGET_RECORD_STEPS,
};

/// Step graph for `expire` — brief §5.6 fan-out table row 5.
pub const EXPIRE_STEPS: &[StepDef] = &[
    StepDef { ord: 0, name: "snapshot.stage",        idempotent: false },
    StepDef { ord: 1, name: "primary.mark_expired",  idempotent: true  },
    StepDef { ord: 2, name: "vector.drain",          idempotent: true  },
    StepDef { ord: 3, name: "fts.drain",             idempotent: true  },
    StepDef { ord: 4, name: "edges.drain",           idempotent: true  },
];

const EXPIRE_GRAPH: StepGraph = StepGraph {
    kind: WalKind::Expire,
    steps: EXPIRE_STEPS,
};

/// Resolves a [`WalKind`] to its static step graph.
#[must_use]
pub fn graph_for(kind: WalKind) -> &'static StepGraph {
    match kind {
        WalKind::Upsert => &UPSERT_GRAPH,
        WalKind::ForgetRecord => &FORGET_RECORD_GRAPH,
        WalKind::Expire => &EXPIRE_GRAPH,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_ords_match_slice_indices() {
        for graph in [&UPSERT_GRAPH, &FORGET_RECORD_GRAPH, &EXPIRE_GRAPH] {
            for (idx, step) in graph.steps.iter().enumerate() {
                assert_eq!(
                    step.ord as usize, idx,
                    "{:?} step {idx} has ord {}", graph.kind, step.ord
                );
            }
        }
    }

    #[test]
    fn step_names_unique_within_graph() {
        for graph in [&UPSERT_GRAPH, &FORGET_RECORD_GRAPH, &EXPIRE_GRAPH] {
            let mut seen: Vec<&str> = Vec::with_capacity(graph.steps.len());
            for step in graph.steps {
                assert!(
                    !seen.contains(&step.name),
                    "{:?} has duplicate step name {}", graph.kind, step.name
                );
                seen.push(step.name);
            }
        }
    }

    #[test]
    fn graph_for_returns_kind_self() {
        assert_eq!(graph_for(WalKind::Upsert).kind, WalKind::Upsert);
        assert_eq!(graph_for(WalKind::ForgetRecord).kind, WalKind::ForgetRecord);
        assert_eq!(graph_for(WalKind::Expire).kind, WalKind::Expire);
    }

    #[test]
    fn graph_step_lookup() {
        let g = graph_for(WalKind::Upsert);
        assert_eq!(g.step(0).map(|s| s.name), Some("snapshot.stage"));
        assert_eq!(g.step(5).map(|s| s.name), Some("primary.activate"));
        assert_eq!(g.step(99), None);
    }

    #[test]
    fn graph_lengths() {
        assert_eq!(graph_for(WalKind::Upsert).len(), 6);
        assert_eq!(graph_for(WalKind::ForgetRecord).len(), 7);
        assert_eq!(graph_for(WalKind::Expire).len(), 5);
    }

    #[test]
    fn wire_form_matches_schema_check() {
        // wal_ops.kind CHECK in 0002_wal.sql widened by 0041_wal_kind_widening.sql
        // includes 'upsert', 'forget_record', 'expire'.
        assert_eq!(WalKind::Upsert.as_str(), "upsert");
        assert_eq!(WalKind::ForgetRecord.as_str(), "forget_record");
        assert_eq!(WalKind::Expire.as_str(), "expire");
    }
}
```

- [ ] **Step 4.2: Run tests**

```bash
cargo nextest run -p cairn-core wal::step_graph --locked
```

Expected: 6 tests pass.

- [ ] **Step 4.3: Activate the step_graph re-export**

Edit `crates/cairn-core/src/wal/mod.rs` — uncomment:

```rust
pub use step_graph::{EXPIRE_STEPS, FORGET_RECORD_STEPS, StepDef, StepGraph, UPSERT_STEPS, WalKind, graph_for};
```

- [ ] **Step 4.4: Verify**

```bash
cargo check -p cairn-core --locked
```

Expected: clean compile.

- [ ] **Step 4.5: Commit**

```bash
git add crates/cairn-core/src/wal/step_graph.rs crates/cairn-core/src/wal/mod.rs
git commit -m "feat(core): WAL step graphs for upsert/forget_record/expire (issue #55)"
```

---

## Task 5: Recovery decision (`cairn-core/src/wal/recover.rs`)

**Files:**
- Modify: `crates/cairn-core/src/wal/recover.rs`
- Modify: `crates/cairn-core/src/wal/mod.rs`

- [ ] **Step 5.1: Implement snapshot types and `decide_recovery`**

Replace `crates/cairn-core/src/wal/recover.rs` with:

```rust
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
/// `next_ord` = `1 + max(ord where state == Done)`, or `0` if no `Done`
/// rows exist. `Failed` rows below `MAX` and `Pending` rows do not advance
/// `next_ord` — they will be retried in place.
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
    // First: any step exhausted retries?
    if let Some(failed_ord) = snapshot
        .steps
        .iter()
        .find(|s| s.state == StepState::Failed && s.attempts >= MAX_STEP_ATTEMPTS)
        .map(|s| s.ord)
    {
        return RecoveryDecision::AbortAndCompensate { failed_ord };
    }

    let graph: &StepGraph = graph_for(snapshot.kind);
    let total = graph.len() as u32;

    // All steps DONE?
    let done_count = snapshot
        .steps
        .iter()
        .filter(|s| s.state == StepState::Done)
        .count() as u32;
    if done_count == total {
        return RecoveryDecision::FinalizeCommitted;
    }

    // Otherwise resume from 1 + max(done_ord), or 0 if no DONE rows.
    let next_ord = snapshot
        .steps
        .iter()
        .filter(|s| s.state == StepState::Done)
        .map(|s| s.ord)
        .max()
        .map_or(0, |max_done| max_done + 1);

    RecoveryDecision::Resume { next_ord }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(state: OpState, steps: Vec<StepRow>) -> OpSnapshot {
        OpSnapshot { kind: WalKind::Upsert, state, steps }
    }

    fn step(ord: u32, state: StepState, attempts: u32) -> StepRow {
        StepRow { ord, state, attempts, last_error: None }
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
            vec![
                step(0, StepState::Done, 1),
                step(1, StepState::Done, 1),
            ],
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
            vec![
                step(0, StepState::Done, 1),
                step(1, StepState::Failed, 1),
            ],
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
    fn prepared_with_gap_in_done_steps_resumes_from_after_max_done() {
        // Steps 0, 1, 3 DONE; step 2 is absent (must mean it never ran).
        // We resume from 1 + max(done_ord) = 4. Step 2 will be re-run by
        // the runner (idempotent re-entry on the absent row inserts PENDING).
        // NOTE: this is the brief's intent — recovery never replays already-DONE
        // steps. The runner ensures step 2 is also marked DONE before
        // continuing because it iterates from start_ord onward; this test
        // documents what `decide_recovery` itself returns.
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
            RecoveryDecision::Resume { next_ord: 4 }
        );
    }

    #[test]
    fn pending_under_max_resumes_in_place() {
        let s = snap(
            OpState::Prepared,
            vec![
                step(0, StepState::Done, 1),
                step(1, StepState::Pending, 1),
            ],
        );
        assert_eq!(
            decide_recovery(&s),
            RecoveryDecision::Resume { next_ord: 1 }
        );
    }
}
```

- [ ] **Step 5.2: Run tests**

```bash
cargo nextest run -p cairn-core wal::recover --locked
```

Expected: 11 tests pass.

- [ ] **Step 5.3: Activate the recover re-export**

Edit `crates/cairn-core/src/wal/mod.rs` — uncomment:

```rust
pub use recover::{MAX_STEP_ATTEMPTS, OpSnapshot, RecoveryDecision, StepRow, decide_recovery};
```

- [ ] **Step 5.4: Verify**

```bash
cargo check -p cairn-core --locked
cargo clippy -p cairn-core --all-targets --locked -- -D warnings
./scripts/check-core-boundary.sh
```

Expected: clean.

- [ ] **Step 5.5: Commit**

```bash
git add crates/cairn-core/src/wal/recover.rs crates/cairn-core/src/wal/mod.rs
git commit -m "feat(core): pure decide_recovery function (issue #55, brief §5.6)"
```

---

## Task 6: `StepRunner` adapter (`cairn-store-sqlite/src/wal/runner.rs`)

**Files:**
- Create: `crates/cairn-store-sqlite/src/wal/runner.rs`
- Modify: `crates/cairn-store-sqlite/src/wal/mod.rs`

- [ ] **Step 6.1: Create `runner.rs` with the `StepBody` trait and `StepRunner`**

Create `crates/cairn-store-sqlite/src/wal/runner.rs`:

```rust
//! Generic WAL step runner — drives a [`StepGraph`] against `wal_steps`.
//!
//! The runner is responsible for:
//! - Persisting a `wal_steps` row per attempt (PENDING → DONE/FAILED).
//! - Idempotent re-entry: a step in `Done` state is skipped.
//! - Retry policy: up to [`MAX_STEP_ATTEMPTS`] attempts with exponential
//!   backoff (100 ms / 400 ms / 1600 ms per brief §5.6).
//!
//! Step bodies (the actual side-effects) are supplied by callers via the
//! [`StepBody`] trait. This crate ships no production bodies for `upsert`
//! / `forget_record` / `expire` — those land in #57 and #58.

use std::sync::Arc;
use std::time::Duration;

use cairn_core::wal::{
    MAX_STEP_ATTEMPTS, OperationId, StepDef, StepGraph, StepState,
};
use rusqlite::Transaction;
use thiserror::Error;
use tokio_rusqlite::Connection;

/// Errors a step body can return.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StepBodyError {
    /// The body decided this attempt should fail; runner will retry up to
    /// the policy ceiling.
    #[error("step body failed: {0}")]
    Failed(String),
    /// Underlying storage / transaction error. Treated as a body failure
    /// for retry purposes.
    #[error("step body storage error")]
    Storage(#[source] rusqlite::Error),
}

/// A side-effect body invoked by the runner, once per step ord per attempt.
///
/// Implementations run inside the same SQLite transaction the runner
/// opened to record the `wal_steps` row update. Returning `Ok(())` causes
/// the runner to commit the transaction with the row marked `Done`.
/// Returning `Err` rolls back the body's writes (the runner re-opens a
/// fresh transaction to record `Failed`).
pub trait StepBody: Send + Sync {
    /// Run the side-effect for `step.ord`.
    ///
    /// # Errors
    /// Any error here causes the runner to mark the step `Failed` and
    /// schedule a retry (up to [`MAX_STEP_ATTEMPTS`]).
    fn run(
        &self,
        tx: &Transaction<'_>,
        op_id: &OperationId,
        step: &StepDef,
    ) -> Result<(), StepBodyError>;
}

/// Errors from the runner.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RunnerError {
    /// A step exhausted [`MAX_STEP_ATTEMPTS`] without succeeding. The
    /// `wal_steps` row is durably `Failed` with `attempts == MAX`. The
    /// caller (recovery) is responsible for the op-level transition to
    /// `Aborted`.
    #[error("step {step_ord} exhausted retries for op {op_id}")]
    Exhausted {
        /// Op id.
        op_id: OperationId,
        /// Step ord that exhausted.
        step_ord: u32,
    },
    /// SQLite / connection failure outside a step body.
    #[error("runner storage error")]
    Storage(#[source] tokio_rusqlite::Error),
}

impl From<tokio_rusqlite::Error> for RunnerError {
    fn from(e: tokio_rusqlite::Error) -> Self {
        Self::Storage(e)
    }
}

/// Drives steps `start_ord..graph.len()` against the given connection.
///
/// `body.run` is called inside a transaction the runner opens; on Ok the
/// transaction commits with the `wal_steps` row marked `Done`. On Err the
/// transaction is rolled back, then a separate transaction marks the row
/// `Failed` with the error message.
///
/// Idempotent re-entry: a step already in `Done` state is skipped — its
/// body is never re-invoked.
///
/// # Errors
/// - [`RunnerError::Exhausted`] when a step fails [`MAX_STEP_ATTEMPTS`] times.
/// - [`RunnerError::Storage`] on connection failure.
pub async fn run_from(
    conn: &Arc<Connection>,
    graph: &'static StepGraph,
    op_id: &OperationId,
    start_ord: u32,
    body: Arc<dyn StepBody>,
) -> Result<(), RunnerError> {
    let total = graph.len() as u32;
    for ord in start_ord..total {
        let step = *graph.step(ord).expect("ord < total checked");
        run_one_step(conn, op_id, &step, &body).await?;
    }
    Ok(())
}

async fn run_one_step(
    conn: &Arc<Connection>,
    op_id: &OperationId,
    step: &StepDef,
    body: &Arc<dyn StepBody>,
) -> Result<(), RunnerError> {
    // 1. Skip if already DONE.
    let existing = read_step_state(conn, op_id, step.ord).await?;
    if existing == Some(StepState::Done) {
        return Ok(());
    }

    // 2. Up to MAX_STEP_ATTEMPTS attempts with backoff.
    for attempt in 1..=MAX_STEP_ATTEMPTS {
        match try_one_attempt(conn, op_id, step, body, attempt).await? {
            AttemptOutcome::Done => return Ok(()),
            AttemptOutcome::Failed if attempt < MAX_STEP_ATTEMPTS => {
                tokio::time::sleep(backoff_for(attempt)).await;
            }
            AttemptOutcome::Failed => {
                return Err(RunnerError::Exhausted {
                    op_id: op_id.clone(),
                    step_ord: step.ord,
                });
            }
        }
    }
    // Unreachable: the loop above always returns.
    Err(RunnerError::Exhausted {
        op_id: op_id.clone(),
        step_ord: step.ord,
    })
}

#[derive(Debug, Clone, Copy)]
enum AttemptOutcome {
    Done,
    Failed,
}

/// Backoff durations per brief §5.6 retry policy: 100 ms / 400 ms / 1600 ms.
fn backoff_for(attempt: u32) -> Duration {
    match attempt {
        1 => Duration::from_millis(100),
        2 => Duration::from_millis(400),
        _ => Duration::from_millis(1600),
    }
}

async fn read_step_state(
    conn: &Arc<Connection>,
    op_id: &OperationId,
    ord: u32,
) -> Result<Option<StepState>, RunnerError> {
    let op = op_id.as_str().to_owned();
    let state_str: Option<String> = conn
        .call(move |c| {
            let row: rusqlite::Result<String> = c.query_row(
                "SELECT state FROM wal_steps WHERE operation_id = ?1 AND step_ord = ?2",
                rusqlite::params![op, ord],
                |r| r.get(0),
            );
            match row {
                Ok(s) => Ok(Some(s)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(tokio_rusqlite::Error::Rusqlite(e)),
            }
        })
        .await?;
    Ok(state_str.map(|s| match s.as_str() {
        "PENDING" => StepState::Pending,
        "DONE" => StepState::Done,
        "FAILED" => StepState::Failed,
        "COMPENSATED" => StepState::Compensated,
        // wal_steps.state is CHECK-constrained — any other value is a
        // schema invariant violation, not a runtime case.
        other => panic!("invariant: unknown wal_steps.state {other:?}"),
    }))
}

async fn try_one_attempt(
    conn: &Arc<Connection>,
    op_id: &OperationId,
    step: &StepDef,
    body: &Arc<dyn StepBody>,
    attempt: u32,
) -> Result<AttemptOutcome, RunnerError> {
    let op = op_id.as_str().to_owned();
    let step_owned = *step;
    let body = Arc::clone(body);
    let now = now_ms();

    let outcome: Result<Result<AttemptOutcome, String>, tokio_rusqlite::Error> = conn
        .call(move |c| {
            let tx = c.transaction()?;

            // Upsert the wal_steps row to PENDING with attempts = attempt.
            // INSERT … ON CONFLICT updates state, attempts, started_at; the
            // FSM trigger allows PENDING→PENDING (same-state, no-op) and
            // FAILED→PENDING (legal retry).
            tx.execute(
                "INSERT INTO wal_steps \
                   (operation_id, step_ord, step_kind, state, attempts, started_at) \
                 VALUES (?1, ?2, ?3, 'PENDING', ?4, ?5) \
                 ON CONFLICT(operation_id, step_ord) DO UPDATE SET \
                   state = 'PENDING', \
                   attempts = ?4, \
                   started_at = ?5",
                rusqlite::params![op, step_owned.ord, step_owned.name, attempt, now],
            )?;

            // Run the body inside the same transaction.
            let body_result = body.run(&tx, &OperationId::parse(op.clone()).expect("valid"), &step_owned);

            match body_result {
                Ok(()) => {
                    let finished = now_ms();
                    tx.execute(
                        "UPDATE wal_steps SET state = 'DONE', finished_at = ?1 \
                         WHERE operation_id = ?2 AND step_ord = ?3",
                        rusqlite::params![finished, op, step_owned.ord],
                    )?;
                    tx.commit()?;
                    Ok::<_, tokio_rusqlite::Error>(Ok(AttemptOutcome::Done))
                }
                Err(StepBodyError::Failed(msg)) => {
                    // Roll back any body writes; record FAILED in a fresh txn.
                    drop(tx);
                    let finished = now_ms();
                    c.execute(
                        "UPDATE wal_steps SET state = 'FAILED', last_error = ?1, finished_at = ?2 \
                         WHERE operation_id = ?3 AND step_ord = ?4",
                        rusqlite::params![msg, finished, op, step_owned.ord],
                    )?;
                    Ok::<_, tokio_rusqlite::Error>(Ok(AttemptOutcome::Failed))
                }
                Err(StepBodyError::Storage(e)) => {
                    drop(tx);
                    let msg = format!("storage: {e}");
                    let finished = now_ms();
                    c.execute(
                        "UPDATE wal_steps SET state = 'FAILED', last_error = ?1, finished_at = ?2 \
                         WHERE operation_id = ?3 AND step_ord = ?4",
                        rusqlite::params![msg, finished, op, step_owned.ord],
                    )?;
                    Ok::<_, tokio_rusqlite::Error>(Ok(AttemptOutcome::Failed))
                }
            }
        })
        .await;

    match outcome? {
        Ok(o) => Ok(o),
        Err(_unused) => unreachable!("outer Ok branch covers both arms"),
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
```

- [ ] **Step 6.2: Register the new module**

Edit `crates/cairn-store-sqlite/src/wal/mod.rs`:

```rust
//! WAL helpers for `cairn-store-sqlite`.
//!
//! Each submodule corresponds to one `wal_ops.kind` and provides async
//! functions that drive the §5.6 FSM (ISSUED → PREPARED → COMMITTED / ABORTED)
//! against the `wal_ops` table.

pub mod lint_repair;
pub mod runner;

pub use runner::{RunnerError, StepBody, StepBodyError, run_from};
```

- [ ] **Step 6.3: Verify compilation**

```bash
cargo check -p cairn-store-sqlite --locked
cargo clippy -p cairn-store-sqlite --all-targets --locked -- -D warnings
```

Expected: clean. (If clippy fires on `unwrap_or(0)` in `now_ms`, replace with a `match` arm.)

- [ ] **Step 6.4: Commit**

```bash
git add crates/cairn-store-sqlite/src/wal/runner.rs crates/cairn-store-sqlite/src/wal/mod.rs
git commit -m "feat(store): WAL StepRunner — drives step graph against wal_steps (issue #55)"
```

---

## Task 7: `recover_pending` entry point (`cairn-store-sqlite/src/wal/recovery.rs`)

**Files:**
- Create: `crates/cairn-store-sqlite/src/wal/recovery.rs`
- Modify: `crates/cairn-store-sqlite/src/wal/mod.rs`

- [ ] **Step 7.1: Implement `recover_pending` and `RecoveryConfig`**

Create `crates/cairn-store-sqlite/src/wal/recovery.rs`:

```rust
//! Boot-time WAL recovery (brief §5.6 "Boot-time recovery").
//!
//! Reads `wal_ops` + `wal_steps` from the open SQLite connection, calls
//! `cairn_core::wal::decide_recovery` for each non-terminal op, and applies
//! the returned [`RecoveryDecision`].
//!
//! Decision-only mode: when no [`StepBodyRegistry`] is configured, the
//! recovery routine handles the decisions that don't require a body
//! ([`RecoveryDecision::NoOp`], `FinalizeRejected`, `FinalizeCommitted`)
//! and emits a `tracing::warn!` for each `Resume` / `AbortAndCompensate`
//! decision it cannot complete. This is the v0.1 default until #57/#58
//! land.

use std::collections::HashMap;
use std::sync::Arc;

use cairn_core::wal::{
    OpSnapshot, OpState, OperationId, RecoveryDecision, StepRow, StepState, WalKind,
    decide_recovery, graph_for,
};
use thiserror::Error;
use tokio_rusqlite::Connection;
use tracing::warn;

use crate::wal::runner::{self, RunnerError, StepBody};

/// Errors from [`recover_pending`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RecoveryError {
    /// Underlying storage failure.
    #[error("recovery storage error")]
    Storage(#[source] tokio_rusqlite::Error),
    /// `wal_ops.state` or `wal_ops.kind` held a value the FSM/CHECK
    /// constraints should have rejected — schema invariant violation.
    #[error("recovery invariant violated: {0}")]
    Invariant(String),
    /// Wraps a runner error so the recovery report can surface it.
    #[error("recovery runner error")]
    Runner(#[source] RunnerError),
}

impl From<tokio_rusqlite::Error> for RecoveryError {
    fn from(e: tokio_rusqlite::Error) -> Self {
        Self::Storage(e)
    }
}

/// Maps each [`WalKind`] to the [`StepBody`] that should run its steps.
/// Implementations are owned by sibling issues #57 / #58.
pub trait StepBodyRegistry: Send + Sync {
    /// Returns the body for the given kind, or `None` if no body is
    /// registered. `None` causes `Resume` and `AbortAndCompensate` to be
    /// skipped with a structured warn.
    fn body_for(&self, kind: WalKind) -> Option<Arc<dyn StepBody>>;
}

/// Empty registry — every kind returns `None`. Used as the default while
/// #57/#58 are in flight; lets terminal-finalize cases run without a body.
pub struct EmptyRegistry;

impl StepBodyRegistry for EmptyRegistry {
    fn body_for(&self, _kind: WalKind) -> Option<Arc<dyn StepBody>> {
        None
    }
}

/// Configuration for [`recover_pending`].
pub struct RecoveryConfig {
    /// Set `false` to skip recovery entirely (e.g. in tests that pre-seed
    /// fixture state and don't want it advanced).
    pub enabled: bool,
    /// Body registry. Default is [`EmptyRegistry`].
    pub bodies: Box<dyn StepBodyRegistry>,
}

impl Default for RecoveryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bodies: Box::new(EmptyRegistry),
        }
    }
}

impl std::fmt::Debug for RecoveryConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecoveryConfig")
            .field("enabled", &self.enabled)
            .field("bodies", &"<dyn StepBodyRegistry>")
            .finish()
    }
}

/// Outcome of one recovery pass.
#[derive(Debug, Default)]
pub struct RecoveryReport {
    /// Ops finalized to `COMMITTED` during this pass.
    pub finalized_committed: Vec<OperationId>,
    /// Ops finalized to `REJECTED` (recovered from `ISSUED`).
    pub finalized_rejected: Vec<OperationId>,
    /// Ops finalized to `ABORTED` because a step exhausted retries.
    pub aborted: Vec<(OperationId, u32)>,
    /// Ops resumed that ran successfully and finalized to `COMMITTED`.
    pub resumed_committed: Vec<OperationId>,
    /// Ops where `Resume` / `AbortAndCompensate` was needed but no body
    /// was registered; skipped with a warn.
    pub skipped_no_body: Vec<(OperationId, RecoveryDecision)>,
    /// Ops already terminal — no action.
    pub no_op: Vec<OperationId>,
}

/// Runs recovery on every non-terminal `wal_ops` row.
///
/// Processes ops in `issued_seq` order (oldest first). Returns the
/// [`RecoveryReport`] for the caller to log/metric on.
///
/// # Errors
/// - [`RecoveryError::Storage`] on connection failure during snapshot loads
///   or finalize transitions.
/// - [`RecoveryError::Invariant`] if the schema produced an unexpected
///   `wal_ops.state` / `kind` value.
/// - [`RecoveryError::Runner`] propagated from a body exhaustion that the
///   second-pass `decide_recovery` couldn't handle (should not happen — a
///   second pass on Exhausted always returns `AbortAndCompensate`).
pub async fn recover_pending(
    conn: &Arc<Connection>,
    config: &RecoveryConfig,
) -> Result<RecoveryReport, RecoveryError> {
    let mut report = RecoveryReport::default();

    if !config.enabled {
        return Ok(report);
    }

    let pending = list_open_ops(conn).await?;

    for (op_id, kind, op_state) in pending {
        let snapshot = load_snapshot(conn, &op_id, kind, op_state).await?;
        let decision = decide_recovery(&snapshot);
        apply_decision(conn, &snapshot, decision, &op_id, config, &mut report).await?;
    }

    Ok(report)
}

async fn list_open_ops(
    conn: &Arc<Connection>,
) -> Result<Vec<(OperationId, WalKind, OpState)>, RecoveryError> {
    let rows: Vec<(String, String, String)> = conn
        .call(|c| {
            let mut stmt = c.prepare(
                "SELECT operation_id, kind, state \
                 FROM wal_ops \
                 WHERE state IN ('ISSUED', 'PREPARED') \
                 ORDER BY issued_seq ASC",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, tokio_rusqlite::Error>(rows)
        })
        .await?;

    rows.into_iter()
        .filter_map(|(op, kind, state)| {
            let op_id = OperationId::parse(op).ok()?;
            let kind = parse_kind(&kind).ok()?;
            let state = parse_op_state(&state).ok()?;
            Some(Ok((op_id, kind, state)))
        })
        .collect()
}

fn parse_kind(s: &str) -> Result<WalKind, RecoveryError> {
    match s {
        "upsert" => Ok(WalKind::Upsert),
        "forget_record" => Ok(WalKind::ForgetRecord),
        "expire" => Ok(WalKind::Expire),
        // Other kinds (promote, forget_session, evolve, graph_*, lint_repair)
        // are out of scope for #55. Recovery skips them with an invariant
        // marker — the body for those kinds is the existing per-kind helper
        // (lint_repair) or a future issue.
        other => Err(RecoveryError::Invariant(format!(
            "wal_ops.kind {other:?} not handled by #55 scaffold"
        ))),
    }
}

fn parse_op_state(s: &str) -> Result<OpState, RecoveryError> {
    match s {
        "ISSUED" => Ok(OpState::Issued),
        "PREPARED" => Ok(OpState::Prepared),
        "COMMITTED" => Ok(OpState::Committed),
        "ABORTED" => Ok(OpState::Aborted),
        "REJECTED" => Ok(OpState::Rejected),
        other => Err(RecoveryError::Invariant(format!(
            "wal_ops.state {other:?} violates schema CHECK"
        ))),
    }
}

fn parse_step_state(s: &str) -> Result<StepState, RecoveryError> {
    match s {
        "PENDING" => Ok(StepState::Pending),
        "DONE" => Ok(StepState::Done),
        "FAILED" => Ok(StepState::Failed),
        "COMPENSATED" => Ok(StepState::Compensated),
        other => Err(RecoveryError::Invariant(format!(
            "wal_steps.state {other:?} violates schema CHECK"
        ))),
    }
}

async fn load_snapshot(
    conn: &Arc<Connection>,
    op_id: &OperationId,
    kind: WalKind,
    state: OpState,
) -> Result<OpSnapshot, RecoveryError> {
    let op = op_id.as_str().to_owned();
    let rows: Vec<(u32, String, u32, Option<String>)> = conn
        .call(move |c| {
            let mut stmt = c.prepare(
                "SELECT step_ord, state, attempts, last_error \
                 FROM wal_steps \
                 WHERE operation_id = ?1 \
                 ORDER BY step_ord ASC",
            )?;
            let rows = stmt
                .query_map([op], |r| {
                    Ok((
                        r.get::<_, u32>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, u32>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, tokio_rusqlite::Error>(rows)
        })
        .await?;

    let mut steps = Vec::with_capacity(rows.len());
    for (ord, state_str, attempts, last_error) in rows {
        steps.push(StepRow {
            ord,
            state: parse_step_state(&state_str)?,
            attempts,
            last_error,
        });
    }

    Ok(OpSnapshot { kind, state, steps })
}

async fn apply_decision(
    conn: &Arc<Connection>,
    snapshot: &OpSnapshot,
    decision: RecoveryDecision,
    op_id: &OperationId,
    config: &RecoveryConfig,
    report: &mut RecoveryReport,
) -> Result<(), RecoveryError> {
    match decision {
        RecoveryDecision::NoOp => {
            report.no_op.push(op_id.clone());
            Ok(())
        }
        RecoveryDecision::FinalizeRejected => {
            finalize(conn, op_id, OpState::Rejected, "recovered_from_issued").await?;
            report.finalized_rejected.push(op_id.clone());
            Ok(())
        }
        RecoveryDecision::FinalizeCommitted => {
            finalize(conn, op_id, OpState::Committed, "recovered").await?;
            report.finalized_committed.push(op_id.clone());
            Ok(())
        }
        RecoveryDecision::Resume { next_ord } => {
            handle_resume(conn, snapshot, next_ord, op_id, config, report).await
        }
        RecoveryDecision::AbortAndCompensate { failed_ord } => {
            // Compensation invocation is deferred to #57/#58. Mark op
            // ABORTED and warn so the gap is observable.
            let reason = format!("recovered: step {failed_ord} exhausted");
            finalize(conn, op_id, OpState::Aborted, &reason).await?;
            warn!(
                op_id = %op_id,
                kind = ?snapshot.kind,
                failed_ord,
                "WAL op aborted by recovery; compensations not run (deferred to #57/#58)"
            );
            report.aborted.push((op_id.clone(), failed_ord));
            Ok(())
        }
    }
}

async fn handle_resume(
    conn: &Arc<Connection>,
    snapshot: &OpSnapshot,
    next_ord: u32,
    op_id: &OperationId,
    config: &RecoveryConfig,
    report: &mut RecoveryReport,
) -> Result<(), RecoveryError> {
    let Some(body) = config.bodies.body_for(snapshot.kind) else {
        warn!(
            op_id = %op_id,
            kind = ?snapshot.kind,
            next_ord,
            "WAL op resume skipped — no StepBody registered (decision-only mode)"
        );
        report
            .skipped_no_body
            .push((op_id.clone(), RecoveryDecision::Resume { next_ord }));
        return Ok(());
    };

    let graph = graph_for(snapshot.kind);
    match runner::run_from(conn, graph, op_id, next_ord, body).await {
        Ok(()) => {
            // Reload snapshot and re-decide. The follow-up decision must be
            // FinalizeCommitted or FinalizeRejected (terminal-bound).
            let reloaded = load_snapshot(conn, op_id, snapshot.kind, OpState::Prepared).await?;
            match decide_recovery(&reloaded) {
                RecoveryDecision::FinalizeCommitted => {
                    finalize(conn, op_id, OpState::Committed, "recovered").await?;
                    report.resumed_committed.push(op_id.clone());
                }
                RecoveryDecision::AbortAndCompensate { failed_ord } => {
                    let reason = format!("recovered: step {failed_ord} exhausted");
                    finalize(conn, op_id, OpState::Aborted, &reason).await?;
                    report.aborted.push((op_id.clone(), failed_ord));
                }
                other => {
                    return Err(RecoveryError::Invariant(format!(
                        "post-resume decision unexpected: {other:?}"
                    )));
                }
            }
            Ok(())
        }
        Err(RunnerError::Exhausted { op_id: e_op, step_ord }) => {
            let reason = format!("recovered: step {step_ord} exhausted");
            finalize(conn, &e_op, OpState::Aborted, &reason).await?;
            report.aborted.push((e_op, step_ord));
            Ok(())
        }
        Err(e) => Err(RecoveryError::Runner(e)),
    }
}

async fn finalize(
    conn: &Arc<Connection>,
    op_id: &OperationId,
    new_state: OpState,
    reason: &str,
) -> Result<(), RecoveryError> {
    let op = op_id.as_str().to_owned();
    let new_state_str = new_state.as_str();
    let reason_owned = reason.to_owned();
    let now = now_ms();

    conn.call(move |c| {
        c.execute(
            "UPDATE wal_ops \
               SET state = ?1, reason = COALESCE(reason, ?2), updated_at = ?3 \
             WHERE operation_id = ?4",
            rusqlite::params![new_state_str, reason_owned, now, op],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await?;
    Ok(())
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

// Forces the `HashMap` import to stay live — registries built by
// downstream issues will commonly use one. Remove when #57/#58 wire a real
// registry through.
#[allow(dead_code)]
fn _hint_for_registry_authors() {
    let _: HashMap<WalKind, Arc<dyn StepBody>> = HashMap::new();
}
```

- [ ] **Step 7.2: Register the new module**

Edit `crates/cairn-store-sqlite/src/wal/mod.rs`:

```rust
//! WAL helpers for `cairn-store-sqlite`.
//!
//! Each submodule corresponds to one `wal_ops.kind` and provides async
//! functions that drive the §5.6 FSM (ISSUED → PREPARED → COMMITTED / ABORTED)
//! against the `wal_ops` table.

pub mod lint_repair;
pub mod recovery;
pub mod runner;

pub use recovery::{
    EmptyRegistry, RecoveryConfig, RecoveryError, RecoveryReport, StepBodyRegistry,
    recover_pending,
};
pub use runner::{RunnerError, StepBody, StepBodyError, run_from};
```

- [ ] **Step 7.3: Re-export from the crate root**

Edit `crates/cairn-store-sqlite/src/lib.rs` — add to the `pub use` block (alphabetical):

```rust
pub use wal::{RecoveryConfig, RecoveryReport, recover_pending};
```

- [ ] **Step 7.4: Verify**

```bash
cargo check -p cairn-store-sqlite --locked
cargo clippy -p cairn-store-sqlite --all-targets --locked -- -D warnings
```

Expected: clean.

- [ ] **Step 7.5: Commit**

```bash
git add crates/cairn-store-sqlite/src/wal/recovery.rs crates/cairn-store-sqlite/src/wal/mod.rs crates/cairn-store-sqlite/src/lib.rs
git commit -m "feat(store): recover_pending — boot WAL recovery (issue #55, brief §5.6)"
```

---

## Task 8: Integration test scenarios (`tests/wal_recovery.rs`)

**Files:**
- Create: `crates/cairn-store-sqlite/tests/wal_recovery.rs`

- [ ] **Step 8.1: Create the integration test fixture**

Create `crates/cairn-store-sqlite/tests/wal_recovery.rs`:

```rust
//! Integration tests for boot-recovery (issue #55, brief §5.6).
//!
//! Uses a synthetic 6-step graph that is re-shaped from the real `upsert`
//! graph (same WalKind, same step count) so production code paths are
//! exercised. The synthetic body is configured per ord with one of three
//! behaviours: succeed, fail-once-then-succeed, always-fail.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use cairn_core::wal::{OperationId, WalKind};
use cairn_store_sqlite::open_in_memory_sync;
use cairn_store_sqlite::wal::{
    EmptyRegistry, RecoveryConfig, StepBody, StepBodyError, StepBodyRegistry, recover_pending,
};
use rusqlite::Transaction;
use rusqlite::params;
use tokio_rusqlite::Connection;

#[derive(Clone, Copy, Debug)]
enum BodyBehavior {
    Succeed,
    FailOnceThenSucceed,
    AlwaysFail,
}

struct SyntheticBody {
    behaviors: Vec<BodyBehavior>,
    /// Per-ord call counters — `[ord]` increments on every call to
    /// `run`. Used to assert idempotency.
    call_counts: Vec<AtomicU32>,
}

impl SyntheticBody {
    fn new(behaviors: Vec<BodyBehavior>) -> Arc<Self> {
        let n = behaviors.len();
        Arc::new(Self {
            behaviors,
            call_counts: (0..n).map(|_| AtomicU32::new(0)).collect(),
        })
    }

    fn calls(&self, ord: u32) -> u32 {
        self.call_counts[ord as usize].load(Ordering::SeqCst)
    }
}

impl StepBody for SyntheticBody {
    fn run(
        &self,
        _tx: &Transaction<'_>,
        _op_id: &OperationId,
        step: &cairn_core::wal::StepDef,
    ) -> Result<(), StepBodyError> {
        let count = self.call_counts[step.ord as usize].fetch_add(1, Ordering::SeqCst) + 1;
        match self.behaviors[step.ord as usize] {
            BodyBehavior::Succeed => Ok(()),
            BodyBehavior::FailOnceThenSucceed if count == 1 => {
                Err(StepBodyError::Failed("synthetic fail-once".into()))
            }
            BodyBehavior::FailOnceThenSucceed => Ok(()),
            BodyBehavior::AlwaysFail => {
                Err(StepBodyError::Failed("synthetic always-fail".into()))
            }
        }
    }
}

struct OneKindRegistry {
    kind: WalKind,
    body: Arc<dyn StepBody>,
}

impl StepBodyRegistry for OneKindRegistry {
    fn body_for(&self, kind: WalKind) -> Option<Arc<dyn StepBody>> {
        (kind == self.kind).then(|| Arc::clone(&self.body))
    }
}

async fn open_db() -> Arc<Connection> {
    let store = open_in_memory_sync().expect("open in-memory db");
    // open_in_memory_sync returns rusqlite::Connection synchronously; wrap
    // it in tokio_rusqlite::Connection by re-opening through the async API.
    // (Adjust if the helper already returns the async type.)
    drop(store);
    let async_conn = tokio_rusqlite::Connection::open_in_memory()
        .await
        .expect("async open");
    let conn = Arc::new(async_conn);
    // Run migrations through the existing helper.
    cairn_store_sqlite::migrations::apply_all(&conn)
        .await
        .expect("apply migrations");
    conn
}

async fn seed_op(
    conn: &Arc<Connection>,
    op_id: &str,
    kind: WalKind,
    state: &str,
    issued_seq: i64,
) {
    let op = op_id.to_owned();
    let kind_str = kind.as_str().to_owned();
    let state_str = state.to_owned();
    conn.call(move |c| {
        c.execute(
            "INSERT INTO wal_ops \
               (operation_id, issued_seq, kind, state, envelope, issuer, \
                target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, '{}', 'i', 'h', '{}', 0, 'sig', 0, 0)",
            params![op, issued_seq, kind_str, state_str],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("seed wal_ops");
}

async fn seed_step(
    conn: &Arc<Connection>,
    op_id: &str,
    ord: u32,
    name: &str,
    state: &str,
    attempts: u32,
) {
    let op = op_id.to_owned();
    let name_owned = name.to_owned();
    let state_owned = state.to_owned();
    conn.call(move |c| {
        c.execute(
            "INSERT INTO wal_steps \
               (operation_id, step_ord, step_kind, state, attempts) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![op, ord, name_owned, state_owned, attempts],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("seed wal_steps");
}

async fn read_op_state(conn: &Arc<Connection>, op_id: &str) -> String {
    let op = op_id.to_owned();
    conn.call(move |c| {
        let s: String = c.query_row(
            "SELECT state FROM wal_ops WHERE operation_id = ?1",
            params![op],
            |r| r.get(0),
        )?;
        Ok::<_, tokio_rusqlite::Error>(s)
    })
    .await
    .expect("read op state")
}

fn upsert_step_names() -> Vec<&'static str> {
    cairn_core::wal::UPSERT_STEPS.iter().map(|s| s.name).collect()
}

// ------------------------------------------------------------------
// Scenarios
// ------------------------------------------------------------------

#[tokio::test]
async fn empty_wal_recovery_returns_empty_report() {
    let conn = open_db().await;
    let report = recover_pending(&conn, &RecoveryConfig::default())
        .await
        .expect("recover");
    assert!(report.finalized_committed.is_empty());
    assert!(report.finalized_rejected.is_empty());
    assert!(report.aborted.is_empty());
    assert!(report.resumed_committed.is_empty());
    assert!(report.skipped_no_body.is_empty());
    assert!(report.no_op.is_empty());
}

#[tokio::test]
async fn issued_orphan_finalizes_rejected() {
    let conn = open_db().await;
    seed_op(&conn, "op-issued", WalKind::Upsert, "ISSUED", 1).await;

    let report = recover_pending(&conn, &RecoveryConfig::default())
        .await
        .expect("recover");

    assert_eq!(report.finalized_rejected.len(), 1);
    assert_eq!(read_op_state(&conn, "op-issued").await, "REJECTED");
}

#[tokio::test]
async fn terminal_committed_is_idempotent_under_repeated_recovery() {
    let conn = open_db().await;
    seed_op(&conn, "op-done", WalKind::Upsert, "ISSUED", 1).await;
    // Seed minimal forward path so the trigger allows the COMMITTED state.
    let conn2 = Arc::clone(&conn);
    conn2
        .call(|c| {
            c.execute(
                "UPDATE wal_ops SET state = 'PREPARED', updated_at = 1 WHERE operation_id = 'op-done'",
                [],
            )?;
            c.execute(
                "UPDATE wal_ops SET state = 'COMMITTED', updated_at = 2 WHERE operation_id = 'op-done'",
                [],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("forward to COMMITTED");

    for _ in 0..3 {
        let report = recover_pending(&conn, &RecoveryConfig::default())
            .await
            .expect("recover");
        assert_eq!(report.no_op.len(), 1);
        assert!(report.finalized_committed.is_empty());
        assert!(report.finalized_rejected.is_empty());
    }
    assert_eq!(read_op_state(&conn, "op-done").await, "COMMITTED");
}

#[tokio::test]
async fn prepared_no_steps_resumes_from_zero_with_body() {
    let conn = open_db().await;
    seed_op(&conn, "op-prep", WalKind::Upsert, "ISSUED", 1).await;
    let c2 = Arc::clone(&conn);
    c2.call(|c| {
        c.execute(
            "UPDATE wal_ops SET state = 'PREPARED', updated_at = 1 WHERE operation_id = 'op-prep'",
            [],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("ISSUED -> PREPARED");

    let body = SyntheticBody::new(vec![BodyBehavior::Succeed; 6]);
    let cfg = RecoveryConfig {
        enabled: true,
        bodies: Box::new(OneKindRegistry {
            kind: WalKind::Upsert,
            body: body.clone(),
        }),
    };

    let report = recover_pending(&conn, &cfg).await.expect("recover");

    assert_eq!(report.resumed_committed.len(), 1);
    assert_eq!(read_op_state(&conn, "op-prep").await, "COMMITTED");
    for ord in 0..6 {
        assert_eq!(body.calls(ord), 1, "step {ord} should run exactly once");
    }
}

#[tokio::test]
async fn prepared_partial_resumes_from_next_step_only() {
    let conn = open_db().await;
    seed_op(&conn, "op-partial", WalKind::Upsert, "ISSUED", 1).await;
    let c2 = Arc::clone(&conn);
    c2.call(|c| {
        c.execute(
            "UPDATE wal_ops SET state = 'PREPARED', updated_at = 1 WHERE operation_id = 'op-partial'",
            [],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("ISSUED -> PREPARED");
    let names = upsert_step_names();
    seed_step(&conn, "op-partial", 0, names[0], "DONE", 1).await;
    seed_step(&conn, "op-partial", 1, names[1], "DONE", 1).await;

    let body = SyntheticBody::new(vec![BodyBehavior::Succeed; 6]);
    let cfg = RecoveryConfig {
        enabled: true,
        bodies: Box::new(OneKindRegistry {
            kind: WalKind::Upsert,
            body: body.clone(),
        }),
    };

    let report = recover_pending(&conn, &cfg).await.expect("recover");

    assert_eq!(report.resumed_committed.len(), 1);
    assert_eq!(read_op_state(&conn, "op-partial").await, "COMMITTED");
    // Steps 0,1 already DONE — body NOT re-invoked.
    assert_eq!(body.calls(0), 0, "step 0 already DONE; body must not run");
    assert_eq!(body.calls(1), 0, "step 1 already DONE; body must not run");
    for ord in 2..6 {
        assert_eq!(body.calls(ord), 1, "step {ord} should run exactly once");
    }
}

#[tokio::test]
async fn retry_exhaustion_aborts_op() {
    let conn = open_db().await;
    seed_op(&conn, "op-fail", WalKind::Upsert, "ISSUED", 1).await;
    let c2 = Arc::clone(&conn);
    c2.call(|c| {
        c.execute(
            "UPDATE wal_ops SET state = 'PREPARED', updated_at = 1 WHERE operation_id = 'op-fail'",
            [],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("ISSUED -> PREPARED");

    // Body succeeds on step 0 and always fails on step 1.
    let mut behaviors = vec![BodyBehavior::Succeed; 6];
    behaviors[1] = BodyBehavior::AlwaysFail;
    let body = SyntheticBody::new(behaviors);
    let cfg = RecoveryConfig {
        enabled: true,
        bodies: Box::new(OneKindRegistry {
            kind: WalKind::Upsert,
            body: body.clone(),
        }),
    };

    let report = recover_pending(&conn, &cfg).await.expect("recover");

    assert_eq!(report.aborted.len(), 1);
    assert_eq!(report.aborted[0].1, 1, "step 1 should be the failed ord");
    assert_eq!(read_op_state(&conn, "op-fail").await, "ABORTED");
    // Step 1 body called 3 times (MAX_STEP_ATTEMPTS).
    assert_eq!(body.calls(1), cairn_core::wal::MAX_STEP_ATTEMPTS);
}

#[tokio::test]
async fn fault_injection_fail_once_then_succeed() {
    let conn = open_db().await;
    seed_op(&conn, "op-flake", WalKind::Upsert, "ISSUED", 1).await;
    let c2 = Arc::clone(&conn);
    c2.call(|c| {
        c.execute(
            "UPDATE wal_ops SET state = 'PREPARED', updated_at = 1 WHERE operation_id = 'op-flake'",
            [],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("ISSUED -> PREPARED");

    // Step 2 fails on attempt 1, succeeds on attempt 2.
    let mut behaviors = vec![BodyBehavior::Succeed; 6];
    behaviors[2] = BodyBehavior::FailOnceThenSucceed;
    let body = SyntheticBody::new(behaviors);
    let cfg = RecoveryConfig {
        enabled: true,
        bodies: Box::new(OneKindRegistry {
            kind: WalKind::Upsert,
            body: body.clone(),
        }),
    };

    let report = recover_pending(&conn, &cfg).await.expect("recover");

    assert_eq!(report.resumed_committed.len(), 1);
    assert_eq!(read_op_state(&conn, "op-flake").await, "COMMITTED");
    // Step 2 called twice (1 fail + 1 success); other steps once each.
    assert_eq!(body.calls(2), 2);
    assert_eq!(body.calls(0), 1);
    assert_eq!(body.calls(5), 1);
}

#[tokio::test]
async fn repeated_recovery_after_partial_is_idempotent() {
    let conn = open_db().await;
    seed_op(&conn, "op-rep", WalKind::Upsert, "ISSUED", 1).await;
    let c2 = Arc::clone(&conn);
    c2.call(|c| {
        c.execute(
            "UPDATE wal_ops SET state = 'PREPARED', updated_at = 1 WHERE operation_id = 'op-rep'",
            [],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("ISSUED -> PREPARED");
    let names = upsert_step_names();
    seed_step(&conn, "op-rep", 0, names[0], "DONE", 1).await;

    let body = SyntheticBody::new(vec![BodyBehavior::Succeed; 6]);
    let cfg = RecoveryConfig {
        enabled: true,
        bodies: Box::new(OneKindRegistry {
            kind: WalKind::Upsert,
            body: body.clone(),
        }),
    };

    // First pass: drives PREPARED -> COMMITTED.
    let r1 = recover_pending(&conn, &cfg).await.expect("recover #1");
    assert_eq!(r1.resumed_committed.len(), 1);

    // Two more passes should be no-ops (terminal COMMITTED).
    for i in 2..=3 {
        let r = recover_pending(&conn, &cfg)
            .await
            .unwrap_or_else(|e| panic!("recover #{i}: {e}"));
        assert_eq!(r.no_op.len(), 1);
        assert!(r.resumed_committed.is_empty());
    }

    // Each step body called exactly once across all 3 passes.
    for ord in 1..6 {
        assert_eq!(body.calls(ord), 1, "step {ord} body must run once");
    }
}

#[tokio::test]
async fn decision_only_mode_skips_resume_with_warn() {
    let conn = open_db().await;
    seed_op(&conn, "op-skip", WalKind::Upsert, "ISSUED", 1).await;
    let c2 = Arc::clone(&conn);
    c2.call(|c| {
        c.execute(
            "UPDATE wal_ops SET state = 'PREPARED', updated_at = 1 WHERE operation_id = 'op-skip'",
            [],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await
    .expect("ISSUED -> PREPARED");

    // Default config = EmptyRegistry — no bodies registered.
    let cfg = RecoveryConfig::default();
    let report = recover_pending(&conn, &cfg).await.expect("recover");

    assert_eq!(report.skipped_no_body.len(), 1);
    assert!(report.resumed_committed.is_empty());
    assert!(report.aborted.is_empty());
    // Op stays in PREPARED — recovery did not advance it.
    assert_eq!(read_op_state(&conn, "op-skip").await, "PREPARED");
}
```

> **Implementation note:** if `cairn_store_sqlite::migrations::apply_all` does not exist, replace the call in `open_db` with whatever the existing migration helper is (likely
> `cairn_store_sqlite::migrations::apply` or it's invoked automatically by `Connection::open_in_memory` — grep `crates/cairn-store-sqlite/src/migrations/` to confirm). Same for `open_in_memory_sync` returning the right type. The intent is: open an in-memory DB with the full migration set applied.

- [ ] **Step 8.2: Verify the helper API names**

```bash
grep -n "pub fn apply\|pub async fn apply" crates/cairn-store-sqlite/src/migrations/mod.rs
grep -n "pub async fn open_in_memory\|pub fn open_in_memory" crates/cairn-store-sqlite/src/open.rs | head
```

If the names differ from `apply_all` / async `open_in_memory_sync`, update the test's `open_db()` helper to use the actual symbols — the test does not need to use the production open path, just needs migrated tables.

- [ ] **Step 8.3: Run the integration tests**

```bash
cargo nextest run -p cairn-store-sqlite --test wal_recovery --locked
```

Expected: all 8 tests pass.

- [ ] **Step 8.4: Commit**

```bash
git add crates/cairn-store-sqlite/tests/wal_recovery.rs
git commit -m "test(store): WAL recovery integration scenarios (issue #55)"
```

---

## Task 9: Cross-validation proptest

**Files:**
- Create: `crates/cairn-store-sqlite/tests/wal_fsm_cross_validation.rs`

- [ ] **Step 9.1: Write the cross-validation proptest**

Create `crates/cairn-store-sqlite/tests/wal_fsm_cross_validation.rs`:

```rust
//! Cross-validation: `cairn_core::wal::fsm` legal-transition predicates
//! must agree with the SQLite triggers in
//! `crates/cairn-store-sqlite/src/migrations/sql/0002_wal.sql`.
//!
//! The pure FSM in core is the single source of truth at the API layer;
//! the SQLite triggers are the single source of truth at the DB layer.
//! Drift between the two would let invalid transitions slip through one
//! side and be rejected by the other. This proptest seeds an in-memory DB
//! with a minimal `wal_ops` row, then for every (from, to) pair in the
//! 5×5 OpState matrix asserts the pure function and the trigger agree.

use cairn_core::wal::{OpState, legal_op_transition};
use cairn_store_sqlite::open_in_memory_sync;
use proptest::prelude::*;
use rusqlite::Connection;

fn arb_state() -> impl Strategy<Value = OpState> {
    prop_oneof![
        Just(OpState::Issued),
        Just(OpState::Prepared),
        Just(OpState::Committed),
        Just(OpState::Aborted),
        Just(OpState::Rejected),
    ]
}

fn seed(conn: &Connection, op_id: &str, seq: i64) {
    conn.execute(
        "INSERT INTO wal_ops (operation_id, issued_seq, kind, state, envelope, issuer, \
          target_hash, scope_json, expires_at, signature, issued_at, updated_at) \
         VALUES (?, ?, 'upsert', 'ISSUED', '{}', 'i', 'h', '{}', 0, 'sig', 0, 0)",
        rusqlite::params![op_id, seq],
    )
    .expect("seed");
}

fn force_state(conn: &Connection, op_id: &str, target: OpState) -> bool {
    // Forward-walk through any legal path to land at `target`.
    // Issued -> Prepared -> {Committed, Aborted}; Issued -> Rejected.
    // For COMMITTED/ABORTED we need to first reach PREPARED.
    let path: &[OpState] = match target {
        OpState::Issued => &[],
        OpState::Prepared => &[OpState::Prepared],
        OpState::Committed => &[OpState::Prepared, OpState::Committed],
        OpState::Aborted => &[OpState::Prepared, OpState::Aborted],
        OpState::Rejected => &[OpState::Rejected],
    };
    for s in path {
        let n = conn
            .execute(
                "UPDATE wal_ops SET state = ? WHERE operation_id = ?",
                rusqlite::params![s.as_str(), op_id],
            )
            .ok();
        if n.is_none() {
            return false;
        }
    }
    true
}

fn current_state(conn: &Connection, op_id: &str) -> String {
    conn.query_row(
        "SELECT state FROM wal_ops WHERE operation_id = ?",
        rusqlite::params![op_id],
        |r| r.get(0),
    )
    .expect("read state")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn pure_fn_agrees_with_sqlite_trigger(from in arb_state(), to in arb_state()) {
        let conn = open_in_memory_sync().expect("open");
        let op = "op-cross";
        seed(&conn, op, 1);
        prop_assert!(force_state(&conn, op, from), "could not reach {from:?}");
        prop_assert_eq!(current_state(&conn, op), from.as_str().to_owned());

        let pure_says = legal_op_transition(from, to);

        // Same-state writes don't fire the trigger; SQLite returns Ok with
        // 1 row updated. legal_op_transition also returns true for f==t.
        // For different states, the trigger ABORTs on illegal transitions
        // and on terminal-immutable rows (which the pure fn also says
        // false for).
        let result = conn.execute(
            "UPDATE wal_ops SET state = ? WHERE operation_id = ?",
            rusqlite::params![to.as_str(), op],
        );
        let sqlite_says = result.is_ok();

        prop_assert_eq!(
            pure_says, sqlite_says,
            "drift on {:?} -> {:?}: pure={}, sqlite={:?}",
            from, to, pure_says, result.err()
        );
    }
}
```

- [ ] **Step 9.2: Run the proptest**

```bash
cargo nextest run -p cairn-store-sqlite --test wal_fsm_cross_validation --locked
```

Expected: passes (64 cases by default; tries every (from, to) pair multiple times).

- [ ] **Step 9.3: Commit**

```bash
git add crates/cairn-store-sqlite/tests/wal_fsm_cross_validation.rs
git commit -m "test(store): cross-validate pure WAL FSM vs SQLite triggers (issue #55)"
```

---

## Task 10: Wire `recover_pending` into the store-open path

**Files:**
- Modify: `crates/cairn-store-sqlite/src/open.rs`

- [ ] **Step 10.1: Locate the open path**

```bash
grep -n "pub async fn open\|pub fn open\|async fn open_with_embedder\|pub async fn open_in_memory" crates/cairn-store-sqlite/src/open.rs | head
```

You're looking for the function(s) that return the live `Connection` after migrations have run. There are several public entry points (`open`, `open_with_embedder`, `open_in_memory`, etc.). Find the **single private helper they all funnel through after migrations are applied**. If no such single point exists, the cleanest hook is right after the migration call in each public entry point — but consolidating into one helper is preferable.

- [ ] **Step 10.2: Add a `with_recovery` helper**

Edit `crates/cairn-store-sqlite/src/open.rs`. Near the top, add an import:

```rust
use crate::wal::{RecoveryConfig, recover_pending};
```

Add a private helper after the existing helpers (precise placement depends on the file's existing layout; aim for "next to the place migrations are run"):

```rust
/// Runs WAL boot recovery (issue #55, brief §5.6). Called after migrations
/// from every public open path. Errors propagate so a corrupt WAL fails
/// the open rather than serving requests against partial state.
async fn run_boot_recovery(
    conn: &std::sync::Arc<tokio_rusqlite::Connection>,
) -> Result<(), crate::error::StoreError> {
    let cfg = RecoveryConfig::default();
    match recover_pending(conn, &cfg).await {
        Ok(report) => {
            tracing::info!(
                finalized_committed = report.finalized_committed.len(),
                finalized_rejected = report.finalized_rejected.len(),
                aborted = report.aborted.len(),
                resumed_committed = report.resumed_committed.len(),
                skipped_no_body = report.skipped_no_body.len(),
                no_op = report.no_op.len(),
                "WAL boot recovery complete"
            );
            Ok(())
        }
        Err(e) => {
            tracing::error!(error = %e, "WAL boot recovery failed");
            Err(crate::error::StoreError::Recovery(format!("{e}")))
        }
    }
}
```

- [ ] **Step 10.3: Add the `Recovery` variant to `StoreError`**

Edit `crates/cairn-store-sqlite/src/error.rs` — add to the `StoreError` enum:

```rust
    /// WAL boot recovery failed.
    #[error("wal recovery: {0}")]
    Recovery(String),
```

(Place it next to other variants; the exact location depends on the existing enum layout.)

- [ ] **Step 10.4: Invoke `run_boot_recovery` from each public open path**

For every `pub async fn open*` and `pub async fn open_in_memory*` in `open.rs`, find the line where migrations finish (typically a `migrations::apply…` call) and add right after it:

```rust
    run_boot_recovery(&conn).await?;
```

`pub fn open_in_memory_sync` and `pub fn open_sync` (test helpers) should NOT call recovery synchronously — leave them unchanged. The async paths are the production entry points.

> **Why sync helpers skip recovery:** they exist for tests/migrations that pre-seed fixture state and run their own assertions; running recovery on every sync open would surprise existing callers. Recovery is opt-in for sync paths via direct `recover_pending` invocation.

- [ ] **Step 10.5: Run the existing store test suite**

```bash
cargo nextest run -p cairn-store-sqlite --locked
```

Expected: all existing tests still pass. If any test pre-seeds `wal_ops` and breaks because recovery advances it, the test should be updated to disable recovery (`RecoveryConfig { enabled: false, .. }`) by opening through the sync helper.

- [ ] **Step 10.6: Add a wiring smoke test to `wal_recovery.rs`**

Append to `crates/cairn-store-sqlite/tests/wal_recovery.rs`:

```rust
#[tokio::test]
async fn open_path_runs_recovery_on_terminal_finalizable_op() {
    // Open a real tempdir-backed DB through the production open path,
    // pre-seed a PREPARED op with all steps DONE, close, re-open, assert
    // the second open finalized the op to COMMITTED.

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cairn.db");

    // First open: seed via raw SQL after migrations.
    {
        let conn = cairn_store_sqlite::open(&path)
            .await
            .expect("open #1");
        let inner = conn.inner_connection_for_tests(); // see note below
        seed_op(&inner, "op-wire", WalKind::Upsert, "ISSUED", 1).await;
        // Walk the FSM forward.
        let inner2 = inner.clone();
        inner2
            .call(|c| {
                c.execute(
                    "UPDATE wal_ops SET state = 'PREPARED', updated_at = 1 \
                     WHERE operation_id = 'op-wire'",
                    [],
                )?;
                Ok::<_, tokio_rusqlite::Error>(())
            })
            .await
            .expect("PREPARED");
        // Mark all 6 upsert steps DONE so the next open finalizes COMMITTED.
        for (ord, name) in cairn_core::wal::UPSERT_STEPS.iter().enumerate() {
            seed_step(&inner, "op-wire", ord as u32, name.name, "DONE", 1).await;
        }
        // Drop closes the connection.
    }

    // Second open: recovery should fire and finalize COMMITTED.
    let conn = cairn_store_sqlite::open(&path).await.expect("open #2");
    let inner = conn.inner_connection_for_tests();
    let state = read_op_state(&inner, "op-wire").await;
    assert_eq!(state, "COMMITTED");
}
```

> **Implementation note:** the test above assumes a way to get the underlying `Arc<Connection>` out of the public `SqliteMemoryStore` (or whatever `cairn_store_sqlite::open` returns). If no such accessor exists, either (a) add a `#[cfg(test)] pub fn inner_connection_for_tests(&self) -> Arc<Connection>` accessor on the store struct, or (b) write the seed via the public store API. Pick whichever is least invasive — the behavior under test is "recovery runs on open", not "the store exposes its connection".

- [ ] **Step 10.7: Run all integration tests**

```bash
cargo nextest run -p cairn-store-sqlite --test wal_recovery --locked
```

Expected: 9 tests pass (8 from Task 8 plus the wiring smoke test).

- [ ] **Step 10.8: Final clippy + workspace check**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
```

Expected: every command exits 0.

- [ ] **Step 10.9: Commit**

```bash
git add crates/cairn-store-sqlite/src/open.rs crates/cairn-store-sqlite/src/error.rs crates/cairn-store-sqlite/tests/wal_recovery.rs
git commit -m "feat(store): run WAL boot recovery on every open (issue #55, brief §5.6)"
```

- [ ] **Step 10.10: Open the PR**

```bash
gh pr create --base main --title "feat(store,core): WAL state machine + boot recovery scaffold (issue #55)" --body "$(cat <<'EOF'
## Summary

Scaffolds the §5.6 WAL state machine, per-step `wal_steps` markers, and
boot-time recovery. Pure FSM and `decide_recovery` in `cairn-core`;
`StepRunner` and `recover_pending` in `cairn-store-sqlite`. Boot recovery
runs on every async open path.

Closes #55. Step bodies for `upsert` / `forget_record` / `expire` are
deferred to siblings #57 and #58 — this PR ships in **decision-only mode**
where ops in terminal-finalizable states are advanced and `Resume` /
`AbortAndCompensate` cases are skipped with a structured warn.

## Brief sections

- §5.6 Write-Ahead Operations + Crash-Safe Apply
- §19.a v0.1 KISS subset

## Invariants touched

- #5 (WAL + two-phase apply) — scaffold only; bodies in #57/#58.
- §6.11 (WAL state machine in core as pure functions) — followed for new code.
- §10.1 (single-writer ordering via deps) — out of scope for #55, processed in `issued_seq` order; deps DAG handled in #56.

## Test plan

- [ ] Pure unit tests for `OpState`/`StepState` transition matrices (`cargo nextest run -p cairn-core wal::`).
- [ ] Pure unit tests for `decide_recovery` covering every decision-table row plus 3 corner cases.
- [ ] 8 integration scenarios in `tests/wal_recovery.rs` covering crash-after-PREPARE, crash-during-APPLY, retry exhaustion, fault-injection, repeated recovery idempotency, decision-only skip path.
- [ ] Cross-validation proptest in `tests/wal_fsm_cross_validation.rs` asserting `cairn-core` FSM agrees with the SQLite triggers from migration `0002_wal.sql`.
- [ ] Wiring smoke test: open production path twice, second open finalizes a pre-seeded all-DONE PREPARED op to COMMITTED.

## Verification

```
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked --no-fail-fast
./scripts/check-core-boundary.sh
```

All green locally.

## Out of scope

- Side-effect step bodies for `upsert` / `forget_record` / `expire` (#57, #58).
- Compensation runners (#57, #58).
- Lock acquisition + dependency DAG handling (#56).
- Migrating existing `wal/lint_repair.rs` onto the new scaffold (follow-up).
EOF
)"
```

---

## Self-review (post-write)

**Spec coverage**

| Spec section                             | Tasks                                                                     |
|------------------------------------------|---------------------------------------------------------------------------|
| §4.1 Crate layering                      | Task 1 (scaffold), Task 6/7 (adapter modules)                             |
| §4.2 Pure FSM                            | Task 2                                                                    |
| §4.3 Step graphs                         | Task 4                                                                    |
| §4.4 Recovery decision                   | Task 5                                                                    |
| §4.5 Idempotency                         | Task 3                                                                    |
| §4.6 Step runner                         | Task 6                                                                    |
| §4.7 Boot recovery                       | Task 7                                                                    |
| §4.8 Wiring into open.rs                 | Task 10                                                                   |
| §5.1 Core unit/proptest                  | Tasks 2, 3, 4, 5 (per-task tests)                                         |
| §5.2 Integration scenarios (8 cases)     | Task 8                                                                    |
| §5.3 Wiring test                         | Task 10 step 10.6                                                         |
| §5.4 Cross-validation proptest           | Task 9                                                                    |

**Placeholder scan:** no TODOs / TBDs / "implement later". Two implementation
notes flag fixable assumptions about helper API names — the engineer is told
to grep and confirm, with the actual symbols already documented if the
guess is wrong.

**Type consistency:** `OperationId`, `StepKey`, `StepLog`, `StepDef`,
`StepGraph`, `WalKind`, `OpSnapshot`, `RecoveryDecision`, `StepRow`,
`MAX_STEP_ATTEMPTS` use the same names everywhere they appear. Field names
(`operation_id`, `step_ord`, `attempts`, `last_error`) match the SQL
schema column names exactly. `RecoveryReport` field names (`finalized_committed`,
`finalized_rejected`, `aborted`, `resumed_committed`, `skipped_no_body`,
`no_op`) are used consistently across recovery.rs, the integration tests,
and the open-path wiring.

**Scope check:** focused on a single PR. Side-effect bodies, compensation,
locks, deps DAG all explicitly out of scope and assigned to sibling issues.

---

## Plan complete

Plan saved to `docs/superpowers/plans/2026-05-05-issue-55-wal-state-machine-recovery.md`.

Two execution options:

1. **Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.
2. **Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?
