# Issue #55 — WAL state machine, step markers, and boot recovery

**Date:** 2026-05-05
**Issue:** [#55](https://github.com/windoliver/cairn/issues/55)
**Parent epic:** [#8](https://github.com/windoliver/cairn/issues/8)
**Brief sources:** §5.6 Write-Ahead Operations, §19.a v0.1 KISS subset
**Related issues:** #54 (closed), #45 (closed), #56 (locks), #57 (upsert/expire bodies), #58 (forget_record bodies)

---

## 1. Goal

Provide the crash-safe **scaffold** that lets P0 mutation operations
(`upsert`, `forget_record`, `expire`) record per-step durability markers and
recover deterministically after a crash. This issue ships only the FSM,
step-graph definitions, recovery decision logic, the generic step runner, and
the boot-recovery entry point. **Side-effect step bodies** (CoW row
activation, vector/FTS/edges drain, primary purge, etc.) are deferred to
sibling issues #57 and #58.

The acceptance criteria for #55 are framed in terms of crash safety, not
domain semantics, so a scaffold-only PR is testable end-to-end against a
synthetic step graph.

---

## 2. Non-goals

- Implementing the actual side-effect bodies for `upsert`, `forget_record`,
  or `expire` (→ #57, #58).
- Lock acquisition / fencing / heartbeat (→ #56).
- WAL machines for `forget_session`, `promote`, or `evolve` (out of scope per
  the issue body; P1/P2).
- Compensation runners for the production op kinds. The recovery decision
  function emits `AbortAndCompensate` outcomes, but the **per-step
  compensation bodies** are owned by the sibling issues that supply the
  forward bodies.
- Migrating the existing `cairn-store-sqlite/src/wal/lint_repair.rs` helper
  onto the new scaffold. That migration is intentionally a follow-up: it
  changes a production code path and warrants its own review.

---

## 3. Background

The WAL substrate already exists at the schema layer:

- `wal_ops` (migrations `0002_wal.sql`, widened by `0041_wal_kind_widening.sql`)
  with state CHECK and FSM trigger enforcing `ISSUED → PREPARED → COMMITTED |
  ABORTED` and `ISSUED → REJECTED`. Triggers also make `wal_ops` append-only
  and lock terminal-state rows immutable.
- `wal_steps` with state CHECK `(PENDING | DONE | FAILED | COMPENSATED)` and
  trigger enforcing legal step transitions (PENDING→DONE/FAILED,
  FAILED→PENDING/COMPENSATED, DONE→COMPENSATED). Append-only.
- `wal_op_deps` with acyclic-DAG trigger.
- `wal_fsm.rs` proptest verifies the SQLite-side FSM matches §5.6.
- `cairn-store-sqlite/src/wal/lint_repair.rs` is the only existing kind
  helper. It uses `ISSUED → PREPARED → COMMITTED` directly without inserting
  any `wal_steps` rows — i.e., it does not exercise per-step durability.

What is missing for #55:

1. A pure FSM and recovery-decision layer in `cairn-core`, per brief §6.11
   ("WAL state machine lives in `cairn-core` as pure functions; the adapter
   only persists its outputs").
2. Step-graph definitions for `upsert`, `forget_record`, `expire`.
3. A generic `StepRunner` in `cairn-store-sqlite` that drives a step graph,
   persists `wal_steps` rows, enforces idempotency, and applies the retry
   policy.
4. A `recover_pending` entry point that the store-open path invokes on every
   daemon start, reading only from `wal_ops` + `wal_steps`.

---

## 4. Architecture

### 4.1 Crate layering

```
cairn-core/src/wal/        ← NEW — pure, no I/O, no workspace deps
  fsm.rs                   ← OpState, StepState, transition validators
  step_graph.rs            ← StepGraph + StepDef; static graphs for P0 kinds
  recover.rs               ← decide_recovery() pure function
  idempotency.rs           ← IdempotencyKey type + IdempotencyTable contract
  mod.rs                   ← re-exports

cairn-store-sqlite/src/wal/
  lint_repair.rs           ← UNCHANGED (migration deferred)
  runner.rs                ← NEW — StepRunner; drives a step graph against SQLite
  recovery.rs              ← NEW — recover_pending(); loads snapshots, calls
                              core::decide_recovery, applies the decision
  mod.rs                   ← re-exports
```

The new `cairn-core/src/wal/` module follows the same crate-boundary rules as
the rest of `cairn-core`: zero workspace dependencies, no I/O, no
`unsafe_code`, no `unwrap()`/`expect()`. Enforced by the existing
`scripts/check-core-boundary.sh`.

### 4.2 Pure FSM (`cairn-core/src/wal/fsm.rs`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum OpState { Issued, Prepared, Committed, Aborted, Rejected }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StepState { Pending, Done, Failed, Compensated }

pub fn legal_op_transition(from: OpState, to: OpState) -> bool;
pub fn legal_step_transition(from: StepState, to: StepState) -> bool;

pub fn is_terminal_op(s: OpState) -> bool;     // Committed | Aborted | Rejected
pub fn is_terminal_step(s: StepState) -> bool; // Done | Compensated (Failed is retryable)
```

The `legal_*_transition` functions mirror the SQLite triggers in
`0002_wal.sql` byte-for-byte. The existing `tests/wal_fsm.rs` proptest will
gain a sibling test that exercises the same input set against the pure
function and asserts equivalence with the trigger.

### 4.3 Step graphs (`cairn-core/src/wal/step_graph.rs`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WalKind {
    Upsert, ForgetRecord, Expire,
    // Other kinds (Promote, ForgetSession, Evolve, Graph*, LintRepair) are
    // not in scope for this issue. They will be added when their owning
    // issues land. Marked `#[non_exhaustive]` so additions are non-breaking.
}

#[derive(Debug, Clone, Copy)]
pub struct StepDef {
    pub ord: u32,
    pub name: &'static str,
    pub idempotent: bool, // §5.6 [idem] marker
}

#[derive(Debug, Clone, Copy)]
pub struct StepGraph {
    pub kind: WalKind,
    pub steps: &'static [StepDef],
}

pub const UPSERT_STEPS: &[StepDef] = &[
    StepDef { ord: 0, name: "snapshot.stage",      idempotent: false },
    StepDef { ord: 1, name: "primary.upsert_cow",  idempotent: true  },
    StepDef { ord: 2, name: "vector.upsert",       idempotent: true  },
    StepDef { ord: 3, name: "fts.upsert",          idempotent: true  },
    StepDef { ord: 4, name: "edges.upsert",        idempotent: true  },
    StepDef { ord: 5, name: "primary.activate",    idempotent: true  },
    // step 7 (consent_log_materializer) is async and not part of the WAL
    // step graph per §5.6.
];

pub const FORGET_RECORD_STEPS: &[StepDef] = &[
    StepDef { ord: 0, name: "primary.mark_tombstone", idempotent: true  },
    StepDef { ord: 1, name: "vector.drain",            idempotent: true  },
    StepDef { ord: 2, name: "fts.drain",               idempotent: true  },
    StepDef { ord: 3, name: "edges.drain",             idempotent: true  },
    StepDef { ord: 4, name: "primary.purge",           idempotent: false },
    StepDef { ord: 5, name: "wal.purge_pre_images",    idempotent: true  },
    StepDef { ord: 6, name: "snapshot.purge",          idempotent: true  },
];

pub const EXPIRE_STEPS: &[StepDef] = &[
    StepDef { ord: 0, name: "snapshot.stage",       idempotent: false },
    StepDef { ord: 1, name: "primary.mark_expired", idempotent: true  },
    StepDef { ord: 2, name: "vector.drain",         idempotent: true  },
    StepDef { ord: 3, name: "fts.drain",            idempotent: true  },
    StepDef { ord: 4, name: "edges.drain",          idempotent: true  },
];

pub fn graph_for(kind: WalKind) -> &'static StepGraph;
```

Step names mirror brief §5.6 fan-out tables. They are stable identifiers
written into `wal_steps.step_kind`. Adding or renaming a step is a wire-
compat change requiring a schema migration to map old names forward.

### 4.4 Recovery decision (`cairn-core/src/wal/recover.rs`)

```rust
#[derive(Debug, Clone)]
pub struct StepRow {
    pub ord: u32,
    pub state: StepState,
    pub attempts: u32,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OpSnapshot {
    pub kind: WalKind,
    pub state: OpState,
    pub steps: Vec<StepRow>, // sorted by ord
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecoveryDecision {
    /// Op is terminal; nothing to do. Idempotent on re-entry.
    NoOp,
    /// Op is ISSUED with no progress; re-fail validation and reject.
    /// (At P0 the same-txn collapse means a persisted ISSUED is by
    /// definition a recovery from a daemon crash before PREPARED was
    /// reached — finalize as REJECTED with reason="recovered_from_issued".)
    FinalizeRejected,
    /// Op is PREPARED, all steps DONE; flip to COMMITTED.
    FinalizeCommitted,
    /// Op is PREPARED, last DONE step is N; resume from N+1 (or 0 if none).
    Resume { next_ord: u32 },
    /// Op has a step exceeding max attempts (3); abort and run compensations.
    AbortAndCompensate { failed_ord: u32 },
}

pub const MAX_STEP_ATTEMPTS: u32 = 3;

pub fn decide_recovery(snapshot: &OpSnapshot) -> RecoveryDecision;
```

Decision rules (extracted from brief §5.6 "Boot-time recovery"):

| Op state         | Steps observed                            | Decision                       |
|------------------|-------------------------------------------|--------------------------------|
| `Committed`      | (any)                                     | `NoOp`                         |
| `Aborted`        | (any)                                     | `NoOp`                         |
| `Rejected`       | (any)                                     | `NoOp`                         |
| `Issued`         | (any)                                     | `FinalizeRejected`             |
| `Prepared`       | all steps DONE (and count == graph.len()) | `FinalizeCommitted`            |
| `Prepared`       | any step FAILED with attempts ≥ 3         | `AbortAndCompensate{ord}`      |
| `Prepared`       | otherwise                                 | `Resume{next_ord}`             |

`next_ord` is computed as `1 + max(ord where state == Done)`, or `0` if no
DONE rows exist. Step rows in `Failed` (attempts < 3) are treated as
in-flight retries and do not advance `next_ord`.

Brief §5.6 explicitly notes: *"TTL applies to new external requests, not to
WAL recovery. Recovery of an already‑PREPARED op runs regardless of TTL."*
The `decide_recovery` function therefore does **not** consult `expires_at`.
The store-side wrapper that admits *new* ops will check TTL; recovery does
not.

### 4.5 Idempotency (`cairn-core/src/wal/idempotency.rs`)

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StepKey {
    pub operation_id: OperationId,
    pub step_ord: u32,
}

/// Contract the adapter implements; pure-test impls live in cairn-test-fixtures.
pub trait StepLog {
    fn step_state(&self, key: &StepKey) -> Option<StepState>;
    fn record_attempt(&mut self, key: &StepKey, attempt: u32) -> Result<(), StepLogError>;
    fn record_done(&mut self, key: &StepKey) -> Result<(), StepLogError>;
    fn record_failed(&mut self, key: &StepKey, err: &str) -> Result<(), StepLogError>;
}
```

`OperationId(String)` is a newtype already established by adjacent code (or
introduced here if absent — to be confirmed during implementation).

### 4.6 Step runner (`cairn-store-sqlite/src/wal/runner.rs`)

```rust
pub struct StepRunner<'a> {
    conn: &'a Arc<Connection>,
    graph: &'static StepGraph,
}

pub trait StepBody: Send + Sync {
    fn run(&self, tx: &Transaction<'_>, ord: u32) -> Result<(), StepBodyError>;
}

impl<'a> StepRunner<'a> {
    pub async fn run_from(
        &self,
        op_id: &OperationId,
        start_ord: u32,
        body: &dyn StepBody,
    ) -> Result<(), RunnerError>;
}
```

For each step ord ≥ start_ord:

1. Open a single SQLite transaction.
2. Read the `wal_steps` row for `(operation_id, step_ord)`. If `state ==
   Done`, skip — idempotent re-entry.
3. Otherwise upsert the row to PENDING with `attempts = attempts + 1`,
   `started_at = now()`.
4. Call `body.run(tx, ord)`.
5. On Ok: mark DONE with `finished_at = now()`. Commit.
6. On Err: mark FAILED with `last_error = err.to_string()`. Commit. If
   `attempts < MAX_STEP_ATTEMPTS`, sleep with exponential backoff
   (100ms / 400ms / 1600ms per brief §5.6 "Retry policy") and retry. After
   final failure, return `RunnerError::Exhausted`.

Side-effect bodies (`StepBody` implementations) are owned by sibling issues
and not included in this PR. The integration test ships a
`SyntheticStepBody` whose behaviour (succeed / fail-once-then-succeed /
always-fail) is configured per ord, used to drive every recovery scenario.

### 4.7 Boot recovery (`cairn-store-sqlite/src/wal/recovery.rs`)

```rust
#[derive(Debug)]
pub struct RecoveryReport {
    pub finalized_committed: Vec<OperationId>,
    pub finalized_rejected: Vec<OperationId>,
    pub resumed: Vec<(OperationId, u32)>, // (op, next_ord that was attempted)
    pub aborted: Vec<(OperationId, u32)>,
    pub no_op: Vec<OperationId>,
}

pub async fn recover_pending(
    conn: &Arc<Connection>,
    bodies: &dyn StepBodyRegistry,
) -> Result<RecoveryReport, RecoveryError>;
```

Where `StepBodyRegistry` is a trait that maps `WalKind → &dyn StepBody`.
This PR ships:
- A test-only registry implementation used by integration tests.
- An empty/panicking registry used by `open.rs` until #57/#58 land. Wiring
  to `open.rs` is feature-gated or behind a `RecoveryConfig::enabled` flag
  so the existing P0 store keeps opening cleanly while sibling issues are
  in flight.

`recover_pending` algorithm:

1. `SELECT operation_id, kind, state FROM wal_ops WHERE state IN ('ISSUED',
   'PREPARED') ORDER BY issued_seq ASC` — process oldest first.
2. For each op, load its `wal_steps` rows, build an `OpSnapshot`, call
   `decide_recovery`.
3. Apply the decision:
   - `NoOp` → record in report; continue.
   - `FinalizeRejected` → `UPDATE wal_ops SET state='REJECTED', reason='recovered_from_issued', updated_at=?`. Single txn; no compensation since no steps could have run.
   - `FinalizeCommitted` → `UPDATE wal_ops SET state='COMMITTED', reason=COALESCE(reason, 'recovered'), updated_at=?`. Single txn. Sets the recovered-marker per brief §5.6 step 6.
   - `Resume { next_ord }` → invoke `StepRunner::run_from(op, next_ord, body)`. After the runner returns (Ok or `RunnerError::Exhausted`), reload the op snapshot and call `decide_recovery` again. The follow-up decision is one of `FinalizeCommitted` (all steps DONE) or `AbortAndCompensate` (a step exhausted retries). Apply that decision in the same recovery pass — the op never lingers in `PREPARED` after a `Resume` returns. This is a bounded recursion: each pass can only transition the op forward in the FSM, and `FinalizeCommitted` / `AbortAndCompensate` are terminal-bound.
   - `AbortAndCompensate { failed_ord }` → mark `wal_ops.state = ABORTED, reason = format!("recovered: step {failed_ord} exhausted")`; **compensation body invocation is deferred to #57/#58**. This PR records the abort transition (which is itself durable and visible to readers) and emits a structured `tracing::warn!` event so the gap between abort-marked and compensations-run is observable. Once #57/#58 land, their `StepBody` impls supply the per-step compensation runner; this design gates that hookup behind the same `StepBodyRegistry` trait.
4. Brief §5.6 note: Phase B physical-purge children of a COMMITTED forget op
   are retried idempotently — handled the same way (Resume), no special
   case.
5. Returns the `RecoveryReport`. Caller decides whether to log/metric/error.

The dependency-DAG handling (`wal_op_deps`) is in scope for #56 (locks +
ordering) per the parent epic. This PR processes ops in `issued_seq` order;
that ordering is sufficient for the P0 single-writer model where deps are
empty.

### 4.8 Wiring into `open.rs`

`recover_pending` is called inside `open_async` (and `open_in_memory_sync`'s
async cousin) **after** migrations run and **before** the function returns.
A `RecoveryConfig` controls behaviour:

```rust
pub struct RecoveryConfig {
    pub enabled: bool,           // default: true
    pub bodies: Option<Box<dyn StepBodyRegistry>>, // None until #57/#58 land
    pub on_abort: AbortPolicy,   // Log | Error
}
```

When `bodies` is `None`, `recover_pending` runs in **decision-only mode**:
it processes `NoOp` / `FinalizeCommitted` / `FinalizeRejected` (these don't
need step bodies) and skips `Resume` / `AbortAndCompensate` cases with a
`tracing::warn!`. This lets #55 land before #57/#58 without breaking
existing tests. Once #57/#58 land they wire a real registry into `open.rs`'s
default config.

---

## 5. Test plan

All tests use either `cairn::memory:` SQLite or `tempfile::tempdir()`. No
mocking of the DB.

### 5.1 `cairn-core` unit + property tests

- `fsm::legal_op_transition` covers the full transition matrix (5 × 5 = 25
  combinations) — table test.
- `fsm::legal_step_transition` covers 4 × 4 = 16 combinations — table test.
- Proptest: random sequences of `OpState` transitions; pure function output
  agrees with the SQLite-trigger output captured by the existing
  `tests/wal_fsm.rs` proptest. (Cross-validation against the trigger.)
- `decide_recovery` table test: one row per row in §4.4's decision table,
  plus three corner cases: empty `wal_steps`, all-DONE-but-state=Committed
  (must be NoOp not double-finalize), step ord gap (steps 0,1,3 DONE — must
  Resume from 2 not 4).

### 5.2 `cairn-store-sqlite` integration tests

In a new `crates/cairn-store-sqlite/tests/wal_recovery.rs`:

A synthetic 3-step graph registered behind a test-only `WalKind::TestSynthetic` (added under `#[cfg(test)]` to avoid leaking into production enums) drives:

1. **Crash after PREPARE, no steps** — seed `wal_ops.state = PREPARED` + 0
   `wal_steps`. Recovery. Assert: synthetic body called for ord 0, 1, 2;
   final state = COMMITTED; reason contains "recovered".
2. **Crash during APPLY, partial steps** — seed PREPARED + steps 0, 1 DONE.
   Recovery. Assert: body called for ord 2 only; final state = COMMITTED.
3. **Terminal idempotent (3×)** — seed COMMITTED. Run recovery 3 times.
   Assert: every run returns `NoOp`, no `wal_ops` updates, no `wal_steps`
   inserts (verified by row counts before/after).
4. **ISSUED orphan** — seed `wal_ops.state = ISSUED` + 0 steps. Recovery.
   Assert: state = REJECTED, reason = "recovered_from_issued", no body
   calls.
5. **Retry exhaustion** — synthetic body always fails on ord 1. Recovery.
   Assert: 3 attempts logged in `wal_steps` (state cycles
   PENDING→FAILED→PENDING→FAILED→PENDING→FAILED), final
   `wal_ops.state = ABORTED`, reason mentions step 1.
6. **Fault injection mid-step** — synthetic body fails once on ord 2 then
   succeeds. First recovery pass marks step 2 FAILED. Second recovery pass
   marks step 2 DONE and finalizes COMMITTED. (Tests the
   FAILED→PENDING→DONE retry edge.)
7. **Repeated recovery on Resume case** — seed PREPARED + step 0 DONE. Run
   recovery 3 times in a row with a body that succeeds. Assert: body called
   exactly 2 times total (not 6), final state COMMITTED. Validates
   idempotency at the runner level.
8. **Empty WAL** — open a fresh DB. Recovery. Assert: empty
   `RecoveryReport`.

### 5.3 Wiring test

A test in `tests/wal_recovery.rs` that opens the store with
`RecoveryConfig::default()` (no bodies registered) and seeds a PREPARED op
with all steps DONE. Asserts the COMMITTED finalize path runs even without
a body registry, and that a Resume case is *skipped with a warn* rather
than panicking.

### 5.4 Cross-validation

The `cairn-core::fsm` property test runs the same proptest input against
both the pure function and a fresh in-memory SQLite DB; the two outcomes
must agree on every input. This is the single point of truth that the FSM
in code matches the FSM in the schema.

---

## 6. Migration / schema impact

**None.** All required schema is already in place (`wal_ops`, `wal_steps`,
`wal_op_deps`, FSM triggers). No migration is added by this issue.

If implementation discovers a gap (e.g., a missing column on `wal_steps` for
something like a structured error code), a new sequenced migration will be
added — but the design above does not require any. `wal_steps.last_error
TEXT` is sufficient for serializing error context; structured error codes
can land in a follow-up if needed.

---

## 7. Risks and mitigations

| Risk | Mitigation |
|------|------------|
| Existing `lint_repair.rs` stays inconsistent with the new scaffold for an unknown duration. | File a follow-up issue at PR time to migrate it. The lint_repair kind is not on the P0 hot path. |
| `RecoveryConfig::bodies = None` mode lets ops sit in `Resume`/`Abort` states unhandled across daemon restarts. | Decision-only mode logs a structured `warn` per skipped op, including `operation_id`, `kind`, and the decision; lint can pick this up. The mode is explicitly transitional — #57/#58 wire real bodies. |
| `decide_recovery` and the SQLite triggers drift over time. | The cross-validation proptest in §5.4 fails on any drift. Add a CI assertion on the test name so it can't be silently disabled. |
| Step-name string drift between code and on-disk `wal_steps.step_kind`. | Step names are `&'static str` const arrays; renaming requires a code change that obviously must be paired with a migration. The code review checklist will call this out. |
| Recovery fires on every store open, including in tests that don't expect it. | `RecoveryConfig::enabled` defaults to `true` only in the production `open_async` path. In-memory test helpers default to `enabled: false` unless the test explicitly opts in. |

---

## 8. Acceptance criteria → test mapping

| Issue criterion | Test |
|-----------------|------|
| Crash after PREPARE resumes without losing or duplicating work | §5.2 case 1 (no steps), case 7 (idempotent re-runs) |
| Crash during APPLY resumes from the last durable step marker | §5.2 case 2, case 6 |
| Terminal states are idempotent under repeated recovery runs | §5.2 case 3 (Committed × 3), case 7 (Resume × 3 with ≤ N body calls) |
| Run fault-injection tests at each WAL state transition | §5.2 case 5 (retry exhaustion), case 6 (fail-once-then-succeed); cross-validation proptest §5.4 |
| Run boot recovery tests against partially applied fixture DBs | §5.2 cases 1, 2, 4, 6 |
| Run idempotency tests that apply the same operation repeatedly | §5.2 cases 3, 7; runner-level idempotency check in §5.1 (skip-DONE branch) |

---

## 9. Open questions (to confirm during implementation)

1. **OperationId newtype** — does one already exist in `cairn-core`? If yes,
   reuse it. If not, this PR introduces it under
   `cairn-core/src/wal/idempotency.rs` (or a higher-level module if other
   subsystems already needed one).
2. **`RecoveryConfig` field shape** — `bodies: Option<Box<dyn StepBodyRegistry>>` may need to be
   `Arc` if the registry is shared across recovery passes within a single
   process lifetime. Settle during implementation.
3. **`AbortPolicy::Error` semantics** — does aborting recovery mid-pass leave
   the store in a half-recovered state? Probably yes by design (the next
   open will redo it), but the test plan should confirm this is observable
   and idempotent.
