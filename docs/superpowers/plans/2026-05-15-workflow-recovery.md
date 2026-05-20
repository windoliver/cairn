# Workflow Recovery + Retry + Metrics + Lint Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land issue [#92](https://github.com/windoliver/cairn/issues/92) — add `FailureClass` taxonomy, dead-letter surfacing, three new workflow `MetricEvent` variants, startup crash recovery, and a `workflow_health` lint check fed by a new `WorkflowJobsReader` trait.

**Architecture:** Five sequential PR-shaped phases. Phase 1 evolves the `JobStore` + `HandlerOutcome` contracts (cairn-core), updates the SQLite adapter signature without persisting yet, and migrates the three workflow handlers. Phase 2 adds SQLite migration 0062 + async `Scheduler::start` w/ startup reap. Phase 3 adds three `MetricEvent` variants + worker/reaper emission. Phase 4 adds `WorkflowJobsReader` + `workflow_health` lint check. Phase 5 wires the config block + regenerates docs.

**Tech Stack:** Rust edition 2024, tokio, rusqlite, rusqlite_migration, async_trait, thiserror, serde_json, insta, rstest, proptest, cargo-nextest.

**Spec:** [`docs/superpowers/specs/2026-05-15-workflow-recovery-design.md`](../specs/2026-05-15-workflow-recovery-design.md)

---

## File Structure

### Phase 1 — Contracts (PR-1)
- **Create:** `crates/cairn-core/src/contract/job_store/failure_class.rs` — new `FailureClass` enum + serde
- **Modify:** `crates/cairn-core/src/contract/job_store.rs` — re-export `FailureClass`; add `failure_class` param to `fail()`; add `failure_class: Option<FailureClass>` to `LeasedJob`
- **Modify:** `crates/cairn-workflows/src/scheduler/handler.rs` — evolve `HandlerOutcome` variants
- **Modify:** `crates/cairn-workflows/src/scheduler/worker.rs` — pass class to `fail`; enforce Validation/Poison → Permanent invariant; debug_assert handler doesn't return scheduler-only classes
- **Modify:** `crates/cairn-workflows/src/sqlite_store.rs` — accept new `class` param in `fail` impl, **stash it locally without persisting** (Phase 2 wires persistence)
- **Modify:** `crates/cairn-workflows/src/dream/handler.rs` — class-stamp Retry/Permanent returns
- **Modify:** `crates/cairn-workflows/src/expiration/handler.rs` — same
- **Modify:** `crates/cairn-workflows/src/evaluation/handler.rs` — same
- **Modify:** `crates/cairn-workflows/src/consolidation/handler.rs` — same
- **Test:** `crates/cairn-core/src/contract/job_store/failure_class.rs` (inline `#[cfg(test)] mod tests`)
- **Test:** `crates/cairn-workflows/src/scheduler/worker.rs` (inline class-override invariant tests via rstest)

### Phase 2 — Migration 0062 + startup reap (PR-2)
- **Create:** `crates/cairn-store-sqlite/src/migrations/sql/0062_workflow_dead_letter.sql`
- **Modify:** `crates/cairn-store-sqlite/src/migrations/mod.rs` — register `M0062_*` const + manifest row + `migrations()` push
- **Modify:** `crates/cairn-workflows/src/sqlite_store.rs` — persist `failure_class`, `dead_letter_at_ms`, `completed_at_ms` in `fail()`/`complete()` impls
- **Modify:** `crates/cairn-workflows/src/scheduler/mod.rs` — `Scheduler::start` becomes `pub async fn`, awaits one `reap_expired` before worker spawn
- **Modify:** `crates/cairn-workflows/src/scheduler/mod.rs` — update existing in-module tests for async start
- **Modify:** `crates/cairn-cli/src/...` (search-and-update call sites that invoke `Scheduler::start`)
- **Create:** `crates/cairn-workflows/tests/startup_reap_fixture.rs` — integration test
- **Create:** `crates/cairn-workflows/tests/dead_letter_fixture.rs` — integration test for dead-letter columns

### Phase 3 — Workflow MetricEvent variants (PR-3)
- **Modify:** `crates/cairn-core/src/domain/metrics.rs` — add 3 new `MetricEvent` variants + round-trip tests
- **Modify:** `crates/cairn-workflows/src/scheduler/worker.rs` — accept `Arc<dyn MetricsSink>`, emit Started/Completed/Failed
- **Modify:** `crates/cairn-workflows/src/scheduler/reaper.rs` — accept `Arc<dyn MetricsSink>`, emit Failed for reclaimed leases
- **Modify:** `crates/cairn-workflows/src/scheduler/mod.rs` — thread `MetricsSink` through `SchedulerConfig`/`start`
- **Modify:** call sites that build a `Scheduler` to pass a sink (or `Arc::new(NoopMetricsSink)`)
- **Create:** `crates/cairn-workflows/tests/metrics_emission_fixture.rs` — Capturing sink integration test

### Phase 4 — Lint integration (PR-4)
- **Create:** `crates/cairn-core/src/contract/workflow_jobs.rs` — `WorkflowJobsReader` trait + `DeadLetterRow` struct
- **Modify:** `crates/cairn-core/src/contract/mod.rs` — register module
- **Modify:** `crates/cairn-core/src/verbs/lint/mod.rs` — extend `LintInputs` with `workflow_jobs: Option<&dyn WorkflowJobsReader>` + `now_ms: i64`
- **Create:** `crates/cairn-core/src/verbs/lint/checks/workflow_health.rs`
- **Modify:** `crates/cairn-core/src/verbs/lint/checks/mod.rs` — `pub mod workflow_health;`
- **Modify:** `crates/cairn-core/src/verbs/lint/mod.rs` — call `workflow_health::run(inputs)` inside `run_checks`
- **Modify:** `crates/cairn-idl/sources/verbs/lint.yaml` (or equivalent IDL source — verify path) — add 4 new `Kind` variants (`WorkflowDeadLetter`, `WorkflowStuck`, `WorkflowStaleSummary`, `WorkflowOverdue`)
- **Regenerate:** `cargo run -p cairn-idl --bin cairn-codegen` and commit generated outputs
- **Modify:** `crates/cairn-store-sqlite/src/...` — implement `WorkflowJobsReader` for the SQLite store (location chosen during Task 4.5; likely a new file `crates/cairn-store-sqlite/src/workflow_jobs_reader.rs`)
- **Modify:** `crates/cairn-cli/src/verbs/lint/...` — wire `workflow_jobs` + `now_ms` into `LintInputs`
- **Modify:** every existing `LintInputs { … }` construction in core tests to add `workflow_jobs: None, now_ms: 0` (or a fixture helper)

### Phase 5 — Config block + docs (PR-5)
- **Create:** `crates/cairn-core/src/config/workflows.rs` — `WorkflowsConfig { lint: WorkflowsLintConfig }` block
- **Modify:** `crates/cairn-core/src/config/mod.rs` — register module, embed into `CairnConfig`
- **Modify:** `crates/cairn-core/src/verbs/lint/checks/workflow_health.rs` — read thresholds from config
- **Regenerate:** `cargo run -p cairn-cli --bin cairn-docgen -- --write` and commit generated outputs under `docs/site/src/reference/generated/`

---

## Phase 1 — Contracts (PR-1)

### Task 1.1: Add `FailureClass` enum (TDD)

**Files:**
- Create: `crates/cairn-core/src/contract/job_store/failure_class.rs`
- Modify: `crates/cairn-core/src/contract/job_store.rs` — convert file into module dir (`job_store/mod.rs`) and re-export
- Test: inline in `failure_class.rs`

The cleanest way to add a submodule under `job_store` is to convert `job_store.rs` into `job_store/mod.rs`. Do that first.

- [ ] **Step 1: Move `job_store.rs` to `job_store/mod.rs`**

Run:
```bash
mkdir crates/cairn-core/src/contract/job_store
git mv crates/cairn-core/src/contract/job_store.rs crates/cairn-core/src/contract/job_store/mod.rs
```

- [ ] **Step 2: Create `failure_class.rs` with the failing test**

Create `crates/cairn-core/src/contract/job_store/failure_class.rs`:

```rust
//! `FailureClass` — typed taxonomy for workflow failure reasons.
//!
//! See spec §4.2. Handlers may return `Transient | Validation | Poison`;
//! the scheduler stamps `Timeout | LeaseLost`. `Validation` and `Poison`
//! always force terminal disposition regardless of handler-supplied
//! `FailDisposition`.

use serde::{Deserialize, Serialize};

/// Classification of why a workflow job failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FailureClass {
    /// Network blip, lock contention, transient I/O — retry helps.
    Transient,
    /// Bad payload, schema mismatch — retry will not help. Terminal.
    Validation,
    /// Repeated same error across attempts — terminal.
    Poison,
    /// Handler exceeded lease deadline — scheduler-stamped.
    Timeout,
    /// Heartbeat lost, watchdog fired — scheduler-stamped.
    LeaseLost,
}

impl FailureClass {
    /// `true` iff the scheduler must force `FailDisposition::Permanent`
    /// regardless of handler-supplied disposition.
    #[must_use]
    pub const fn forces_permanent(self) -> bool {
        matches!(self, Self::Validation | Self::Poison)
    }

    /// `true` iff this class is only legal from the scheduler (handlers
    /// must not return it).
    #[must_use]
    pub const fn is_scheduler_only(self) -> bool {
        matches!(self, Self::Timeout | Self::LeaseLost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forces_permanent_matrix() {
        assert!(!FailureClass::Transient.forces_permanent());
        assert!(FailureClass::Validation.forces_permanent());
        assert!(FailureClass::Poison.forces_permanent());
        assert!(!FailureClass::Timeout.forces_permanent());
        assert!(!FailureClass::LeaseLost.forces_permanent());
    }

    #[test]
    fn is_scheduler_only_matrix() {
        assert!(!FailureClass::Transient.is_scheduler_only());
        assert!(!FailureClass::Validation.is_scheduler_only());
        assert!(!FailureClass::Poison.is_scheduler_only());
        assert!(FailureClass::Timeout.is_scheduler_only());
        assert!(FailureClass::LeaseLost.is_scheduler_only());
    }

    #[test]
    fn json_round_trip_snake_case() {
        for c in [
            FailureClass::Transient,
            FailureClass::Validation,
            FailureClass::Poison,
            FailureClass::Timeout,
            FailureClass::LeaseLost,
        ] {
            let j = serde_json::to_string(&c).expect("serialize");
            let back: FailureClass = serde_json::from_str(&j).expect("deserialize");
            assert_eq!(c, back, "round trip for {c:?}");
        }
        assert_eq!(
            serde_json::to_string(&FailureClass::LeaseLost).unwrap(),
            "\"lease_lost\""
        );
    }
}
```

- [ ] **Step 3: Wire submodule + re-export**

In `crates/cairn-core/src/contract/job_store/mod.rs`, add near the top (after the file's existing module docs):

```rust
pub mod failure_class;
pub use failure_class::FailureClass;
```

- [ ] **Step 4: Run the failing → passing test**

Run: `cargo nextest run -p cairn-core --no-fail-fast failure_class`
Expected: 3 passing tests.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/contract/job_store
git commit -m "feat(core): add FailureClass taxonomy (issue #92, spec §4.2)"
```

### Task 1.2: Add `failure_class` field to `LeasedJob`

**Files:**
- Modify: `crates/cairn-core/src/contract/job_store/mod.rs:194-208`

- [ ] **Step 1: Add the field**

In `LeasedJob` (around line 194 of `mod.rs`):

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeasedJob {
    pub job_id: JobId,
    pub kind: JobKind,
    pub payload: JobPayload,
    pub attempts: u32,
    pub retry: RetryPolicy,
    pub lease: LeaseToken,
    /// Class of the most recent failure, if any. `None` for first
    /// attempts and successfully completed retries. Set by the store
    /// when a previously-failed row is re-leased.
    pub failure_class: Option<FailureClass>,
}
```

- [ ] **Step 2: Update every constructor in tests + adapter**

Run `rg "LeasedJob {" --type rust` to find all construction sites. For each, add `failure_class: None,`. Likely sites:
- `crates/cairn-workflows/src/sqlite_store.rs` — `lease()` impl returns
- `crates/cairn-workflows/src/scheduler/worker.rs` — test fixtures
- `crates/cairn-workflows/tests/sqlite_job_store.rs`

- [ ] **Step 3: Build to find missed sites**

Run: `cargo check -p cairn-core -p cairn-workflows -p cairn-store-sqlite --all-targets --locked`
Expected: PASS after every `LeasedJob` constructor has the new field.

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p cairn-core -p cairn-workflows --locked --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(core): carry failure_class through LeasedJob (spec §4.4)"
```

### Task 1.3: Evolve `HandlerOutcome` to stamp `FailureClass`

**Files:**
- Modify: `crates/cairn-workflows/src/scheduler/handler.rs:11-25`

- [ ] **Step 1: Edit the enum**

Replace the `HandlerOutcome` definition in `handler.rs`:

```rust
use cairn_core::contract::job_store::{FailureClass, JobKind, JobPayload};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandlerOutcome {
    /// Handler succeeded; scheduler calls `complete`.
    Done,
    /// Retryable failure; scheduler calls `fail(Retry, class)`. Note: if
    /// `class.forces_permanent()` is true (`Validation`/`Poison`), the
    /// scheduler converts disposition to `Permanent` before calling the
    /// store.
    Retry {
        /// Error message persisted into `workflow_jobs.last_error`.
        reason: String,
        /// Failure classification. Must NOT be `Timeout` or `LeaseLost`
        /// — those are scheduler-stamped.
        class: FailureClass,
    },
    /// Permanent failure; scheduler calls `fail(Permanent, class)`.
    Permanent {
        /// Error message persisted into `workflow_jobs.last_error`.
        reason: String,
        /// Failure classification. Must NOT be `Timeout` or `LeaseLost`.
        class: FailureClass,
    },
}

impl HandlerOutcome {
    /// Convenience constructor for a transient retry.
    #[must_use]
    pub fn transient_retry(reason: impl Into<String>) -> Self {
        Self::Retry {
            reason: reason.into(),
            class: FailureClass::Transient,
        }
    }

    /// Convenience constructor for a validation-permanent failure.
    #[must_use]
    pub fn validation_permanent(reason: impl Into<String>) -> Self {
        Self::Permanent {
            reason: reason.into(),
            class: FailureClass::Validation,
        }
    }
}
```

- [ ] **Step 2: Run cargo check**

Run: `cargo check -p cairn-workflows --all-targets --locked`
Expected: many compile errors at `HandlerOutcome::Retry { reason }` and `HandlerOutcome::Permanent { reason }` callers. That's the whole point — we'll fix them in Tasks 1.4–1.8.

### Task 1.4: Update worker to stamp scheduler-only classes + enforce invariants

**Files:**
- Modify: `crates/cairn-workflows/src/scheduler/worker.rs:182-230`

- [ ] **Step 1: Replace the outcome handling block**

In `worker.rs`, replace the `let outcome = tokio::select! { … }` and the trailing `let result = match outcome { … }` block with:

```rust
    let outcome = tokio::select! {
        o = handler.handle(&leased.payload) => o,
        () = cancel.cancelled() => HandlerOutcome::Retry {
            reason: "scheduler shutdown".into(),
            class: FailureClass::Transient,
        },
        () = lease_lost.cancelled() => HandlerOutcome::Retry {
            reason: "heartbeat lost or lease deadline exceeded".into(),
            class: FailureClass::Timeout,
        },
    };
    hb_token.cancel();
    let _ = timeout(Duration::from_secs(1), hb_handle).await;
    let _ = timeout(Duration::from_secs(1), watchdog_handle).await;

    if lease_lost.is_cancelled() {
        warn!(job = %leased.job_id, "abandoning execution after lease loss");
        return;
    }

    let now = clock.now_ms();
    let result = match outcome {
        HandlerOutcome::Done => store.complete(&leased.job_id, &leased.lease, now).await,
        HandlerOutcome::Retry { reason, class } => {
            debug_assert!(
                !class.is_scheduler_only() || class == FailureClass::Timeout,
                "handler returned scheduler-only class {class:?}",
            );
            // Invariant: Validation/Poison force Permanent regardless of
            // handler-supplied disposition (spec §4.2).
            let disposition = if class.forces_permanent() {
                FailDisposition::Permanent
            } else {
                FailDisposition::Retry
            };
            store
                .fail(&leased.job_id, &leased.lease, disposition, class, &reason, now)
                .await
        }
        HandlerOutcome::Permanent { reason, class } => {
            debug_assert!(
                !class.is_scheduler_only(),
                "handler returned scheduler-only class {class:?} on Permanent",
            );
            store
                .fail(
                    &leased.job_id,
                    &leased.lease,
                    FailDisposition::Permanent,
                    class,
                    &reason,
                    now,
                )
                .await
        }
    };
    if let Err(e) = result {
        error!(error = %e, job = %leased.job_id, "worker finalize failed");
    }
```

Note: this passes `class` to `store.fail`. The trait signature changes in Task 1.5.

Also add the import at the top:

```rust
use cairn_core::contract::job_store::{FailureClass, FailDisposition, JobStore, LeasedJob};
```

(keep existing imports; only add `FailureClass`).

### Task 1.5: Add `class` param to `JobStore::fail` trait

**Files:**
- Modify: `crates/cairn-core/src/contract/job_store/mod.rs:357-364`

- [ ] **Step 1: Update the trait method**

Replace the `fail` method on the `JobStore` trait:

```rust
    /// Record a failure. With [`FailDisposition::Retry`] the row goes
    /// back to `Queued` (or terminates if `attempts == max_attempts`);
    /// [`FailDisposition::Permanent`] forces terminal `Failed`.
    /// `class` is persisted on the row when a terminal disposition is
    /// reached (used by the lint workflow_health check).
    ///
    /// # Errors
    ///
    /// [`JobStoreError::LeaseLost`] if the lease no longer matches.
    async fn fail(
        &self,
        job_id: &JobId,
        lease: &LeaseToken,
        disposition: FailDisposition,
        class: FailureClass,
        last_error: &str,
        now_ms: i64,
    ) -> Result<(), JobStoreError>;
```

- [ ] **Step 2: Update the SQLite adapter signature (stash, don't persist)**

In `crates/cairn-workflows/src/sqlite_store.rs` find `async fn fail` around line 896. Update its signature to match the trait and pass `class` through to `cas_fail`. For Phase 1 we accept the new parameter but **do not** write it to a column — Phase 2 adds the column and persistence.

Add `_class: FailureClass` parameter (underscore-prefixed) to both `fail` and the private `cas_fail` helper:

```rust
async fn fail(
    &self,
    job_id: &JobId,
    lease: &LeaseToken,
    disposition: FailDisposition,
    _class: FailureClass,    // Phase 2 persists this.
    last_error: &str,
    now_ms: i64,
) -> Result<(), JobStoreError> {
    // existing body unchanged
}
```

(propagate `_class` into `cas_fail` similarly).

Also add to the imports at top of `sqlite_store.rs`:

```rust
use cairn_core::contract::job_store::{
    EnqueueRequest, FailDisposition, FailureClass, JobId, JobKind, JobStore, JobStoreError,
    LeasedJob, /* …rest unchanged… */
};
```

- [ ] **Step 3: Build**

Run: `cargo check -p cairn-core -p cairn-workflows -p cairn-store-sqlite --all-targets --locked`
Expected: remaining compile errors only inside handlers (`dream`, `expiration`, `evaluation`, `consolidation`) and the conformance suite that constructs `HandlerOutcome` literals.

### Task 1.6: Migrate `dream` handler

**Files:**
- Modify: `crates/cairn-workflows/src/dream/handler.rs:419-490` (and any test sites)

- [ ] **Step 1: Replace `HandlerOutcome::Permanent { reason: ... }` with class-stamped variants**

Find all `HandlerOutcome::Permanent { reason: ... }` and `HandlerOutcome::Retry { reason: ... }` in `dream/handler.rs`. For each:

- If the `reason` describes a missing-config / bad-payload / no-LLM-configured situation → use `HandlerOutcome::Permanent { reason, class: FailureClass::Validation }`
- If the `reason` describes an external error (LLM timeout, network) → use `HandlerOutcome::Retry { reason, class: FailureClass::Transient }`

Concretely, the three existing `Permanent` returns (around lines 423, 436, 442) are all configuration / payload-validation situations — use `FailureClass::Validation`. The one `Retry` return (around line 449) wraps an LLM error — use `FailureClass::Transient`.

Add the import at top:

```rust
use cairn_core::contract::job_store::FailureClass;
```

- [ ] **Step 2: Update tests in the same file**

Replace pattern matches like `assert!(matches!(outcome, HandlerOutcome::Permanent { .. }))` with `assert!(matches!(outcome, HandlerOutcome::Permanent { class: FailureClass::Validation, .. }))` where the test specifically wants to validate the class. Otherwise leave the `..` wildcard, which still compiles.

- [ ] **Step 3: Build + test**

```bash
cargo check -p cairn-workflows --all-targets --locked
cargo nextest run -p cairn-workflows --locked --no-fail-fast dream::
```
Expected: PASS.

### Task 1.7: Migrate `expiration` and `evaluation` handlers

**Files:**
- Modify: `crates/cairn-workflows/src/expiration/handler.rs`
- Modify: `crates/cairn-workflows/src/evaluation/handler.rs`

Repeat the Task 1.6 pattern in each file:

- [ ] **Step 1: Audit each `HandlerOutcome::*` in expiration/handler.rs and class-stamp**

Use the same rule: validation-shaped reasons → `FailureClass::Validation`; transient/external reasons → `FailureClass::Transient`. Add the `FailureClass` import.

- [ ] **Step 2: Same for evaluation/handler.rs**

- [ ] **Step 3: Build + test**

```bash
cargo check -p cairn-workflows --all-targets --locked
cargo nextest run -p cairn-workflows --locked --no-fail-fast
```
Expected: PASS.

### Task 1.8: Migrate `consolidation` handler

**Files:**
- Modify: `crates/cairn-workflows/src/consolidation/handler.rs`

- [ ] **Step 1: Class-stamp every `HandlerOutcome::*` in consolidation/handler.rs**

Same rule as Tasks 1.6/1.7.

- [ ] **Step 2: Build + test the whole workspace**

```bash
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
```
Expected: PASS. If any test that constructs `HandlerOutcome::Retry/Permanent` is still failing to compile, add `class: FailureClass::Transient` (the safe default) to the literal.

### Task 1.9: Worker class-override invariant tests (rstest)

**Files:**
- Modify: `crates/cairn-workflows/src/scheduler/worker.rs` (inline `#[cfg(test)]` block)

- [ ] **Step 1: Add `rstest` to dev-deps if not already present**

Check `crates/cairn-workflows/Cargo.toml` `[dev-dependencies]` for `rstest`. If missing, add:

```toml
rstest = { workspace = true }
```

Verify `rstest` is in the workspace deps (`Cargo.toml` at repo root). If not, add it as a workspace dep.

- [ ] **Step 2: Add the failing tests**

In `crates/cairn-workflows/src/scheduler/worker.rs`, append to the existing `#[cfg(test)] mod tests { … }` block:

```rust
    use cairn_core::contract::job_store::{FailDisposition, FailureClass};

    struct CapturingStore {
        fails: std::sync::Mutex<Vec<(FailDisposition, FailureClass)>>,
        completes: std::sync::Mutex<usize>,
    }
    impl CapturingStore {
        fn new() -> Self {
            Self {
                fails: std::sync::Mutex::new(vec![]),
                completes: std::sync::Mutex::new(0),
            }
        }
    }
    #[async_trait::async_trait]
    impl JobStore for CapturingStore {
        async fn enqueue(&self, _: cairn_core::contract::job_store::EnqueueRequest) -> Result<(), cairn_core::contract::job_store::JobStoreError> { Ok(()) }
        async fn lease(&self, _: &str, _: i64, _: i64) -> Result<Option<LeasedJob>, cairn_core::contract::job_store::JobStoreError> { Ok(None) }
        async fn heartbeat(&self, _: &cairn_core::contract::job_store::JobId, _: &cairn_core::contract::job_store::LeaseToken, _: i64, _: i64) -> Result<(), cairn_core::contract::job_store::JobStoreError> { Ok(()) }
        async fn complete(&self, _: &cairn_core::contract::job_store::JobId, _: &cairn_core::contract::job_store::LeaseToken, _: i64) -> Result<(), cairn_core::contract::job_store::JobStoreError> {
            *self.completes.lock().unwrap() += 1;
            Ok(())
        }
        async fn fail(
            &self,
            _: &cairn_core::contract::job_store::JobId,
            _: &cairn_core::contract::job_store::LeaseToken,
            disposition: FailDisposition,
            class: FailureClass,
            _: &str,
            _: i64,
        ) -> Result<(), cairn_core::contract::job_store::JobStoreError> {
            self.fails.lock().unwrap().push((disposition, class));
            Ok(())
        }
        async fn reap_expired(&self, _: i64) -> Result<usize, cairn_core::contract::job_store::JobStoreError> { Ok(0) }
    }

    use rstest::rstest;

    #[rstest]
    #[case(FailureClass::Transient, false)]
    #[case(FailureClass::Validation, true)]
    #[case(FailureClass::Poison, true)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handler_retry_with_class_forces_permanent_when_required(
        #[case] class: FailureClass,
        #[case] should_force_permanent: bool,
    ) {
        use cairn_core::contract::job_store::{JobId, JobKind, LeaseToken, RetryPolicy};
        struct Handler(FailureClass);
        #[async_trait::async_trait]
        impl JobHandler for Handler {
            fn kind(&self) -> JobKind { JobKind::new("test.k") }
            async fn handle(&self, _: &cairn_core::contract::job_store::JobPayload) -> HandlerOutcome {
                HandlerOutcome::Retry { reason: "x".into(), class: self.0 }
            }
        }
        let store: Arc<dyn JobStore> = Arc::new(CapturingStore::new());
        let captured: Arc<CapturingStore> = unsafe { std::mem::transmute(store.clone()) };
        let registry = crate::scheduler::HandlerRegistryBuilder::default()
            .with(Arc::new(Handler(class)))
            .build();
        let clock = Arc::new(crate::scheduler::MockClock::at(1_000)) as Arc<dyn Clock>;
        let cancel = CancellationToken::new();
        let leased = LeasedJob {
            job_id: JobId::new("j-1"),
            kind: JobKind::new("test.k"),
            payload: vec![],
            attempts: 1,
            retry: RetryPolicy::DEFAULT,
            lease: LeaseToken {
                owner: "w-0".into(),
                nonce: "n-0".into(),
                expires_at_ms: 30_000,
            },
            failure_class: None,
        };
        execute_one(&store, &registry, &clock, &cancel, &leased, &WorkerConfig {
            lease_ms: 30_000,
            heartbeat_every_ms: 10_000,
            idle_poll_ms: 50,
        }).await;
        let fails = captured.fails.lock().unwrap();
        assert_eq!(fails.len(), 1);
        let (disp, got_class) = fails[0];
        assert_eq!(got_class, class);
        let expected = if should_force_permanent {
            FailDisposition::Permanent
        } else {
            FailDisposition::Retry
        };
        assert_eq!(disp, expected);
    }
```

Note: the `unsafe { std::mem::transmute }` is ugly. Prefer concretely typed `Arc<CapturingStore>` and pass `&store` directly into `execute_one`. The existing `execute_one` takes `&Arc<dyn JobStore>`. Adjust by introducing a small `let store_dyn: Arc<dyn JobStore> = store.clone();` and pass that.

Cleaner version (replace the `let store: Arc<dyn JobStore>` + transmute lines):

```rust
        let store_cap = Arc::new(CapturingStore::new());
        let store_dyn: Arc<dyn JobStore> = store_cap.clone();
        // …same registry, clock, cancel…
        execute_one(&store_dyn, &registry, &clock, &cancel, &leased, &WorkerConfig { … }).await;
        let fails = store_cap.fails.lock().unwrap();
```

- [ ] **Step 3: Run the test**

```bash
cargo nextest run -p cairn-workflows --locked --no-fail-fast scheduler::worker::tests::handler_retry_with_class
```
Expected: 3 cases (Transient/Validation/Poison) PASS.

- [ ] **Step 4: Commit Phase 1**

```bash
git add -A
git commit -m "feat(workflows): FailureClass-aware HandlerOutcome + worker invariants (#92, spec §4.2-4.4)"
```

### Task 1.10: Phase 1 verification gate

- [ ] **Step 1: Run the CLAUDE.md §8 checks**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```
Expected: all green. If `cargo-codegen --check` flags drift, run `cargo run -p cairn-idl --bin cairn-codegen --locked` (no --check) and commit the generated outputs.

---

## Phase 2 — Migration 0062 + startup reap (PR-2)

### Task 2.1: Create migration 0062 SQL

**Files:**
- Create: `crates/cairn-store-sqlite/src/migrations/sql/0062_workflow_dead_letter.sql`

- [ ] **Step 1: Write the SQL**

```sql
-- Migration 0062: workflow dead-letter columns + completion timestamps.
-- Issue #92. Spec: docs/superpowers/specs/2026-05-15-workflow-recovery-design.md
-- Brief sources: §5.6 WAL, §10 Continuous Learning.
--
-- Adds three nullable columns to workflow_jobs:
--   * failure_class     — last FailureClass stamped by the worker on fail()
--   * dead_letter_at_ms — wall-clock when the row transitioned to state='failed'
--   * completed_at_ms   — wall-clock when the row transitioned to state='done'
--
-- All nullable for backward compat with existing 0020 rows.

ALTER TABLE workflow_jobs ADD COLUMN failure_class    TEXT;
ALTER TABLE workflow_jobs ADD COLUMN dead_letter_at_ms INTEGER;
ALTER TABLE workflow_jobs ADD COLUMN completed_at_ms   INTEGER;

-- Lint hot-path: enumerate dead-letter rows.
CREATE INDEX workflow_jobs_dead_letter_idx
  ON workflow_jobs(dead_letter_at_ms)
  WHERE dead_letter_at_ms IS NOT NULL;

-- Lint hot-path: last-success lookup per kind.
CREATE INDEX workflow_jobs_kind_completed_idx
  ON workflow_jobs(kind, completed_at_ms);

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (62, '0062_workflow_dead_letter', '', strftime('%s','now') * 1000);
```

### Task 2.2: Register migration in `migrations/mod.rs`

**Files:**
- Modify: `crates/cairn-store-sqlite/src/migrations/mod.rs:122-302,308-364`

- [ ] **Step 1: Add the const after the existing 0061 const**

After `const M0061_SALIENCE_ACCESS: …` (around line 122) add:

```rust
// Issue #92 — workflow dead-letter columns + completion timestamps.
const M0062_WORKFLOW_DEAD_LETTER: &str = include_str!("sql/0062_workflow_dead_letter.sql");
```

- [ ] **Step 2: Add to the manifest array**

After `(61, "0061_salience_access", M0061_SALIENCE_ACCESS),` add:

```rust
    (
        62,
        "0062_workflow_dead_letter",
        M0062_WORKFLOW_DEAD_LETTER,
    ),
```

- [ ] **Step 3: Add to the `migrations()` push list**

After `M::up(M0061_SALIENCE_ACCESS),` (near line 363) add:

```rust
        M::up(M0062_WORKFLOW_DEAD_LETTER),
```

- [ ] **Step 4: Build**

```bash
cargo check -p cairn-store-sqlite --all-targets --locked
```
Expected: PASS.

### Task 2.3: Update the workflow_jobs schema fixture test

**Files:**
- Modify: `crates/cairn-store-sqlite/tests/workflow_jobs_schema.rs`

- [ ] **Step 1: Read the existing test to understand its shape**

Run: `cat crates/cairn-store-sqlite/tests/workflow_jobs_schema.rs`

It asserts the column set after migration 0020. The new columns from 0062 must appear when all migrations have run. Add a separate assertion or expand the existing one.

- [ ] **Step 2: Add the new column assertions**

Append a new test (don't modify the old one — it asserts a specific historical state):

```rust
#[test]
fn migration_0062_adds_dead_letter_columns() {
    let conn = rusqlite::Connection::open_in_memory().expect("open");
    cairn_store_sqlite::migrations::migrations()
        .to_latest(&mut { conn })
        .expect("migrate");
    // Re-open since `to_latest` consumed conn.
    // (Test idiom: rebuild the connection inside an `apply` helper if
    // the crate provides one; otherwise refactor this test.)
    let conn2 = rusqlite::Connection::open_in_memory().expect("open");
    cairn_store_sqlite::migrations::migrations()
        .to_latest(&mut { conn2 })
        .expect("migrate");
    let cols: Vec<String> = conn2
        .prepare("SELECT name FROM pragma_table_info('workflow_jobs') ORDER BY cid")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(cols.contains(&"failure_class".to_string()), "cols = {cols:?}");
    assert!(cols.contains(&"dead_letter_at_ms".to_string()));
    assert!(cols.contains(&"completed_at_ms".to_string()));
}
```

Note: if `migrations::migrations()` is not directly callable from a test (private), reuse the existing test's helper.

- [ ] **Step 3: Run the test**

```bash
cargo nextest run -p cairn-store-sqlite --test workflow_jobs_schema --locked
```
Expected: PASS.

- [ ] **Step 4: Commit 2.1–2.3**

```bash
git add -A
git commit -m "feat(store-sqlite): migration 0062 dead-letter + completion columns (#92, spec §4.5)"
```

### Task 2.4: Persist `failure_class` + `dead_letter_at_ms` in `fail()`

**Files:**
- Modify: `crates/cairn-workflows/src/sqlite_store.rs:1258-1330` (the `cas_fail` helper)

- [ ] **Step 1: Read the existing cas_fail body**

Run: `sed -n '1258,1330p' crates/cairn-workflows/src/sqlite_store.rs`

Identify the UPDATE statement that flips state to 'failed'.

- [ ] **Step 2: Add the new columns to the UPDATE**

The UPDATE that flips to 'failed' (terminal branch) should now also set:

```sql
UPDATE workflow_jobs
   SET state = 'failed',
       failure_class = ?,
       dead_letter_at_ms = ?,
       last_error = ?,
       attempts = ?,
       updated_at = ?,
       lease_owner = NULL, lease_nonce = NULL,
       lease_started = NULL, lease_expires_at = NULL
 WHERE job_id = ? AND lease_nonce = ? AND state = 'leased';
```

Bind `class.as_str()` and `now_ms` for the new params. For `class`, use `serde_json::to_string(&class).map(|s| s.trim_matches('"').to_string())` or, cleaner, add an inherent `as_str()` method on `FailureClass` returning `&'static str`:

In `failure_class.rs` add:

```rust
impl FailureClass {
    /// Snake-case string used by the SQLite adapter for the
    /// `failure_class` column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Transient => "transient",
            Self::Validation => "validation",
            Self::Poison => "poison",
            Self::Timeout => "timeout",
            Self::LeaseLost => "lease_lost",
        }
    }
}
```

And a `TryFrom<&str> for FailureClass` for the read path (used in Phase 4):

```rust
impl std::str::FromStr for FailureClass {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "transient" => Self::Transient,
            "validation" => Self::Validation,
            "poison" => Self::Poison,
            "timeout" => Self::Timeout,
            "lease_lost" => Self::LeaseLost,
            other => return Err(format!("unknown failure_class: {other}")),
        })
    }
}
```

The retry branch (non-terminal) of `cas_fail` does **not** write `failure_class` or `dead_letter_at_ms` — these only land when state becomes 'failed'.

- [ ] **Step 3: Drop the underscore prefix on `_class` parameter**

Now that the parameter is used, rename from `_class` to `class` in:
- `JobStore::fail` impl signature
- `cas_fail` helper signature

- [ ] **Step 4: Build**

```bash
cargo check -p cairn-workflows --all-targets --locked
```

### Task 2.5: Persist `completed_at_ms` in `complete()`

**Files:**
- Modify: `crates/cairn-workflows/src/sqlite_store.rs` — find `async fn complete` and its CAS helper

- [ ] **Step 1: Find the complete() impl**

Run: `rg "fn complete" crates/cairn-workflows/src/sqlite_store.rs -n`

- [ ] **Step 2: Add `completed_at_ms = ?` to the UPDATE clause**

Add `completed_at_ms = ?` to the UPDATE that flips state to 'done', binding `now_ms`. The retry/terminal branches stay unchanged.

- [ ] **Step 3: Build**

```bash
cargo check -p cairn-workflows --all-targets --locked
cargo nextest run -p cairn-workflows --locked --no-fail-fast
```
Expected: PASS — existing tests don't read the new column, so they're unaffected.

### Task 2.6: Failing integration test for dead-letter columns

**Files:**
- Create: `crates/cairn-workflows/tests/dead_letter_fixture.rs`

- [ ] **Step 1: Write the failing test**

```rust
//! Issue #92 — fail × max_attempts persists failure_class + dead_letter_at_ms.

use cairn_core::contract::job_store::{
    EnqueueRequest, FailDisposition, FailureClass, JobId, JobKind, JobStore, LeaseToken,
    RetryPolicy,
};
use cairn_workflows::SqliteJobStore;
use rusqlite::Connection;

fn open_store() -> SqliteJobStore {
    let conn = Connection::open_in_memory().expect("open");
    cairn_workflows::sqlite_store::install_for_tests(&conn);
    SqliteJobStore::new(conn).expect("store")
}

#[tokio::test]
async fn fail_to_exhaustion_writes_dead_letter_columns() {
    let store = open_store();
    let store_dyn: &dyn JobStore = &store;
    let policy = RetryPolicy {
        max_attempts: 2,
        base_backoff_ms: 1,
        backoff_multiplier: 1,
        max_backoff_ms: 1,
    };
    store_dyn
        .enqueue(EnqueueRequest {
            job_id: JobId::new("j-dead"),
            kind: JobKind::new("test.dl"),
            payload: vec![],
            queue_key: None,
            dedupe_key: None,
            not_before_ms: 0,
            retry: policy,
        })
        .await
        .unwrap();
    // Lease, heartbeat (to consume an attempt), fail twice with Transient
    // retry — second fail hits max_attempts and terminates.
    for attempt in 1..=2 {
        let leased = store_dyn
            .lease("w-0", attempt * 100, 1_000)
            .await
            .unwrap()
            .expect("leased");
        store_dyn
            .heartbeat(&leased.job_id, &leased.lease, attempt * 100 + 1, attempt * 100 + 1_000)
            .await
            .unwrap();
        store_dyn
            .fail(
                &leased.job_id,
                &leased.lease,
                FailDisposition::Retry,
                FailureClass::Transient,
                &format!("attempt {attempt} bombed"),
                attempt * 100 + 2,
            )
            .await
            .unwrap();
    }
    // Inspect the row directly.
    let conn = Connection::open_in_memory().expect("scratch");
    // …actually we need the same store; expose its raw conn or read
    // through a helper. Easiest: query through the SqliteJobStore's
    // `conn` accessor if one exists; otherwise add a tiny helper.
    // For this fixture, leverage a debug accessor.
    let row = store
        .raw_inspect("SELECT state, failure_class, dead_letter_at_ms FROM workflow_jobs WHERE job_id = 'j-dead'")
        .expect("query");
    let (state, class, dl_at): (String, Option<String>, Option<i64>) = row;
    assert_eq!(state, "failed");
    assert_eq!(class.as_deref(), Some("transient"));
    assert!(dl_at.unwrap() > 0);
}
```

If `SqliteJobStore` does not expose a `raw_inspect` helper, add one gated by `#[cfg(any(test, feature = "test-support"))]` in `sqlite_store.rs`:

```rust
#[cfg(any(test, feature = "test-support"))]
impl SqliteJobStore {
    pub fn raw_inspect(&self, sql: &str) -> rusqlite::Result<(String, Option<String>, Option<i64>)> {
        let conn = self.conn_guard();   // existing accessor — check name
        conn.query_row(sql, [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
    }
}
```

Verify the connection accessor name by searching: `rg "fn conn|pub fn .*&self.*Connection" crates/cairn-workflows/src/sqlite_store.rs`.

- [ ] **Step 2: Run**

```bash
cargo nextest run -p cairn-workflows --test dead_letter_fixture --locked
```
Expected: PASS (now that 2.4 persists the columns).

- [ ] **Step 3: Commit Phase 2 so far**

```bash
git add -A
git commit -m "feat(workflows): persist failure_class + dead_letter_at_ms + completed_at_ms (spec §4.5, §4.9)"
```

### Task 2.7: Make `Scheduler::start` async + startup reap

**Files:**
- Modify: `crates/cairn-workflows/src/scheduler/mod.rs:53-84`

- [ ] **Step 1: Change the signature + body**

Replace the `start` function:

```rust
    /// Spawn N workers + 1 reaper and return a handle. Runs one
    /// best-effort `reap_expired` before spawning workers so a crashed
    /// predecessor's expired leases are reclaimed without waiting for
    /// the periodic reaper tick (spec §4.7).
    #[must_use]
    pub async fn start(
        incarnation_id: &str,
        store: Arc<dyn JobStore>,
        registry: &HandlerRegistry,
        clock: Arc<dyn Clock>,
        config: SchedulerConfig,
    ) -> Self {
        let now = clock.now_ms();
        if let Err(e) = store.reap_expired(now).await {
            tracing::warn!(
                error = %e,
                "startup reap failed; periodic reaper will recover"
            );
        }
        let cancel = CancellationToken::new();
        let tracker = TaskTracker::new();
        for i in 0..config.worker_count.max(1) {
            let owner = format!("{incarnation_id}:w{i}");
            let t = cancel.clone();
            let s = store.clone();
            let r = registry.clone();
            let c = clock.clone();
            tracker.spawn(worker::run_worker(owner, s, r, c, t, config.worker));
        }
        let t = cancel.clone();
        tracker.spawn(reaper::run_reaper(store, clock, t, config.reaper));
        tracker.close();
        Self { cancel, tracker }
    }
```

- [ ] **Step 2: Update in-module test**

In the same file's `#[cfg(test)] mod tests`, change the `start_and_shutdown_idempotent` test to `.await` the `Scheduler::start(...)` call. The test is already inside `#[tokio::test]`.

- [ ] **Step 3: Build the workspace**

```bash
cargo check --workspace --all-targets --locked
```
Expected: compile errors at every external `Scheduler::start(...)` call site (likely in `cairn-cli` and other tests).

- [ ] **Step 4: Update all call sites**

Search and update:

```bash
rg "Scheduler::start" --type rust -l | sort -u
```

For each call site:
- If inside an `async` context: add `.await`.
- If inside a sync context: wrap in `tokio::runtime::Handle::current().block_on(...)` only if the caller is on a tokio worker thread but inside a sync fn — should be rare. The CLI bootstrap is the most likely site; it should already be inside `#[tokio::main]`.

- [ ] **Step 5: Build + test**

```bash
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
```
Expected: PASS.

### Task 2.8: Startup reap integration fixture

**Files:**
- Create: `crates/cairn-workflows/tests/startup_reap_fixture.rs`

- [ ] **Step 1: Write the test**

```rust
//! Issue #92 — Scheduler::start runs one reap before spawning workers,
//! so a row with an expired lease is back to Queued before any worker
//! tick (spec §4.7).

use std::sync::Arc;

use cairn_core::contract::job_store::{
    EnqueueRequest, FailDisposition, FailureClass, JobId, JobKind, JobStore, RetryPolicy,
};
use cairn_workflows::scheduler::{HandlerRegistry, MockClock, Scheduler, SchedulerConfig, WorkerConfig};
use cairn_workflows::SqliteJobStore;
use rusqlite::Connection;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_reaps_expired_lease_before_workers_run() {
    let conn = Connection::open_in_memory().expect("open");
    cairn_workflows::sqlite_store::install_for_tests(&conn);
    let store: Arc<dyn JobStore> = Arc::new(SqliteJobStore::new(conn).expect("store"));
    // Enqueue + lease + heartbeat to put the row into Leased with a known expiry.
    store
        .enqueue(EnqueueRequest {
            job_id: JobId::new("j-orphan"),
            kind: JobKind::new("test.reap"),
            payload: vec![],
            queue_key: None,
            dedupe_key: None,
            not_before_ms: 0,
            retry: RetryPolicy::DEFAULT,
        })
        .await
        .unwrap();
    let leased = store.lease("predecessor", 1_000, 100).await.unwrap().expect("leased");
    // Don't heartbeat; let the lease expire by advancing the mock clock.

    // worker_count = 0 means no workers spawn — proves the reap is in start(), not the worker loop.
    let config = SchedulerConfig {
        worker_count: 0,
        worker: WorkerConfig::default(),
        reaper: cairn_workflows::scheduler::ReaperConfig { interval_ms: 60_000 }, // never tick during test
    };
    let registry = HandlerRegistry::default();
    // Use a wall-clock far past the lease expiry so reap_expired reclaims it.
    let clock: Arc<dyn cairn_workflows::scheduler::Clock> = Arc::new(MockClock::at(10_000));
    let s = Scheduler::start("inc-test", store.clone(), &registry, clock, config).await;
    // The row should now be Queued without any worker or reaper tick.
    let again = store.lease("post-reap-worker", 11_000, 100).await.unwrap();
    assert!(again.is_some(), "row should be lease-eligible after startup reap");
    s.shutdown().await;
    let _ = leased; // suppress unused
}
```

- [ ] **Step 2: Run**

```bash
cargo nextest run -p cairn-workflows --test startup_reap_fixture --locked
```
Expected: PASS.

- [ ] **Step 3: Commit Phase 2**

```bash
git add -A
git commit -m "feat(workflows): async Scheduler::start with one-shot startup reap (#92, spec §4.7)"
```

### Task 2.9: Phase 2 verification gate

- [ ] **Step 1: Run §8 checks**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
```
Expected: all green.

---

## Phase 3 — Workflow MetricEvent variants (PR-3)

### Task 3.1: Add three `MetricEvent` variants (TDD)

**Files:**
- Modify: `crates/cairn-core/src/domain/metrics.rs:16-61`

- [ ] **Step 1: Add the variants**

After `EvaluationCompleted` and before the closing `}` of the enum:

```rust
    /// Emitted on successful `JobStore::lease` return.
    #[serde(rename = "workflow_job_started")]
    WorkflowJobStarted {
        ts_ms: i64,
        job_id: String,
        kind: String,
        attempts: u32,
        /// `now_ms - not_before_ms` at lease time.
        queue_lag_ms: i64,
        dedupe_key: Option<String>,
    },
    /// Emitted on successful `JobStore::complete` return.
    #[serde(rename = "workflow_job_completed")]
    WorkflowJobCompleted {
        ts_ms: i64,
        job_id: String,
        kind: String,
        attempts: u32,
        /// `ts_ms - started_at_ms` (worker-local).
        duration_ms: u64,
    },
    /// Emitted on every `JobStore::fail` and on reclaimed reaper leases.
    #[serde(rename = "workflow_job_failed")]
    WorkflowJobFailed {
        ts_ms: i64,
        job_id: String,
        kind: String,
        attempts: u32,
        /// `"retry"` or `"permanent"`.
        disposition: String,
        /// `FailureClass::as_str()`.
        failure_class: String,
        last_error: String,
        /// Absent for terminal failures, present for retries (next eligible time).
        will_retry_at_ms: Option<i64>,
    },
```

- [ ] **Step 2: Add round-trip tests in the same file**

Append to the `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn workflow_job_started_round_trips() {
        let e = MetricEvent::WorkflowJobStarted {
            ts_ms: 1_700_000_000_000,
            job_id: "j-1".into(),
            kind: "dream.light".into(),
            attempts: 1,
            queue_lag_ms: 42,
            dedupe_key: Some("op-x".into()),
        };
        let j = serde_json::to_string(&e).unwrap();
        assert!(j.contains("\"event\":\"workflow_job_started\""));
        let back: MetricEvent = serde_json::from_str(&j).unwrap();
        match back {
            MetricEvent::WorkflowJobStarted { queue_lag_ms, .. } => assert_eq!(queue_lag_ms, 42),
            _ => panic!("variant mismatch"),
        }
    }

    #[test]
    fn workflow_job_completed_round_trips() {
        let e = MetricEvent::WorkflowJobCompleted {
            ts_ms: 1,
            job_id: "j-1".into(),
            kind: "dream.light".into(),
            attempts: 1,
            duration_ms: 123,
        };
        let j = serde_json::to_string(&e).unwrap();
        assert!(j.contains("\"event\":\"workflow_job_completed\""));
        let _: MetricEvent = serde_json::from_str(&j).unwrap();
    }

    #[test]
    fn workflow_job_failed_round_trips() {
        let e = MetricEvent::WorkflowJobFailed {
            ts_ms: 1,
            job_id: "j-1".into(),
            kind: "dream.light".into(),
            attempts: 3,
            disposition: "retry".into(),
            failure_class: "transient".into(),
            last_error: "boom".into(),
            will_retry_at_ms: Some(1_500),
        };
        let j = serde_json::to_string(&e).unwrap();
        assert!(j.contains("\"event\":\"workflow_job_failed\""));
        assert!(j.contains("\"failure_class\":\"transient\""));
        let _: MetricEvent = serde_json::from_str(&j).unwrap();
    }
```

- [ ] **Step 3: Run**

```bash
cargo nextest run -p cairn-core --locked --no-fail-fast metrics::tests
```
Expected: 5 passing tests (2 existing + 3 new).

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat(core): add WorkflowJob{Started,Completed,Failed} MetricEvent variants (#92, spec §4.6)"
```

### Task 3.2: Thread `MetricsSink` through Scheduler

**Files:**
- Modify: `crates/cairn-workflows/src/scheduler/mod.rs:21-45,53-84`
- Modify: `crates/cairn-workflows/src/scheduler/worker.rs:40-71` (signature)
- Modify: `crates/cairn-workflows/src/scheduler/reaper.rs:28-48` (signature)

- [ ] **Step 1: Add `sink: Arc<dyn MetricsSink>` to `SchedulerConfig`**

In `scheduler/mod.rs`, add to imports:

```rust
use cairn_core::contract::metrics::{MetricsSink, NoopMetricsSink};
```

Replace `SchedulerConfig`:

```rust
#[derive(Clone)]
pub struct SchedulerConfig {
    pub worker: WorkerConfig,
    pub reaper: ReaperConfig,
    pub worker_count: u32,
    pub metrics: Arc<dyn MetricsSink>,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            worker: WorkerConfig::default(),
            reaper: ReaperConfig::default(),
            worker_count: 1,
            metrics: Arc::new(NoopMetricsSink),
        }
    }
}

impl SchedulerConfig {
    /// P0 default — 2 workers, 30s leases, 5s reap interval, no metrics.
    #[must_use]
    pub fn p0() -> Self {
        Self {
            worker: WorkerConfig {
                lease_ms: 30_000,
                heartbeat_every_ms: 10_000,
                idle_poll_ms: 200,
            },
            reaper: ReaperConfig { interval_ms: 5_000 },
            worker_count: 2,
            metrics: Arc::new(NoopMetricsSink),
        }
    }
}
```

Note: `Default` derive is removed because `Arc<dyn MetricsSink>` isn't `Default`-derivable; explicit impl above.

- [ ] **Step 2: Thread sink into `start`**

In `Scheduler::start`, pass `config.metrics.clone()` to `run_worker` and `run_reaper`. Update those signatures next.

### Task 3.3: Worker emits Started / Completed / Failed

**Files:**
- Modify: `crates/cairn-workflows/src/scheduler/worker.rs`

- [ ] **Step 1: Add sink param to `run_worker` + `execute_one`**

Update signatures to take `Arc<dyn MetricsSink>`. Pass from `Scheduler::start`.

- [ ] **Step 2: Emit Started after successful lease**

After `let leased = match store.lease(...)? { Ok(Some(job)) => job, ... };` insert:

```rust
let started_at_ms = clock.now_ms();
let _ = metrics
    .emit(MetricEvent::WorkflowJobStarted {
        ts_ms: started_at_ms,
        job_id: leased.job_id.to_string(),
        kind: leased.kind.to_string(),
        attempts: leased.attempts,
        queue_lag_ms: started_at_ms.saturating_sub(leased.not_before_ms),
        dedupe_key: leased.dedupe_key.clone(),
    })
    .await;
```

Wait — `LeasedJob` does not currently expose `not_before_ms` or `dedupe_key`. Two options:
- Add `not_before_ms: i64` and `dedupe_key: Option<String>` to `LeasedJob`.
- Compute `queue_lag_ms` differently (e.g. now − enqueued_at — needs `enqueued_at_ms` on LeasedJob).

**Decision:** add `not_before_ms: i64` and `dedupe_key: Option<String>` to `LeasedJob` in `cairn-core::contract::job_store`. The SQLite adapter already reads them from the DB, just doesn't return them. Update the lease() impl to populate them.

Sub-steps:
1. Add the two fields to `LeasedJob` in `crates/cairn-core/src/contract/job_store/mod.rs`.
2. In `crates/cairn-workflows/src/sqlite_store.rs`, the `lease()` SELECT already SELECTs those columns — populate them in the row → struct mapping.
3. Update test constructors to set `not_before_ms: 0, dedupe_key: None,`.

- [ ] **Step 3: Emit Completed on Done**

Inside the `HandlerOutcome::Done` arm, after `store.complete(...).await` succeeds:

```rust
let done_at_ms = clock.now_ms();
let _ = metrics
    .emit(MetricEvent::WorkflowJobCompleted {
        ts_ms: done_at_ms,
        job_id: leased.job_id.to_string(),
        kind: leased.kind.to_string(),
        attempts: leased.attempts,
        duration_ms: done_at_ms.saturating_sub(started_at_ms).max(0) as u64,
    })
    .await;
```

Important: only emit on `Ok(_)` from `store.complete`. The `if let Err(e) = result { … }` block at the end already logs failures.

- [ ] **Step 4: Emit Failed on either Retry or Permanent**

Inside both `Retry { .. }` and `Permanent { .. }` arms, after `store.fail(...).await` succeeds, emit `WorkflowJobFailed` with the disposition string and `failure_class.as_str()`. `will_retry_at_ms` for `Retry` is `Some(now + retry.delay_for_attempt(leased.attempts + 1) as i64)`; for terminal/permanent it's `None`. The store knows whether the row terminated — for now, compute "will terminate" the same way the spec describes:

```rust
let will_terminate = matches!(disposition, FailDisposition::Permanent)
    || class.forces_permanent()
    || leased.attempts >= leased.retry.max_attempts;
let will_retry_at = if will_terminate {
    None
} else {
    Some(now.saturating_add(i64::from(leased.retry.delay_for_attempt(leased.attempts + 1))))
};
let _ = metrics
    .emit(MetricEvent::WorkflowJobFailed {
        ts_ms: now,
        job_id: leased.job_id.to_string(),
        kind: leased.kind.to_string(),
        attempts: leased.attempts,
        disposition: match disposition {
            FailDisposition::Retry => "retry".into(),
            FailDisposition::Permanent => "permanent".into(),
        },
        failure_class: class.as_str().to_owned(),
        last_error: reason.clone(),
        will_retry_at_ms: will_retry_at,
    })
    .await;
```

- [ ] **Step 5: Run worker tests**

```bash
cargo nextest run -p cairn-workflows --locked --no-fail-fast scheduler::worker
```
Expected: existing tests still PASS. Metric emission is a side effect they don't assert against.

### Task 3.4: Reaper emits Failed for reclaimed leases

**Files:**
- Modify: `crates/cairn-workflows/src/scheduler/reaper.rs`

- [ ] **Step 1: Add sink param**

Update `run_reaper` signature to take `Arc<dyn MetricsSink>`. Update `ReaperConfig` if you prefer to bundle it there — but separate param keeps the API simpler.

Actually since the reaper needs to know which rows were reclaimed (not just the count), the current `reap_expired -> usize` is too coarse. The cleanest fix is to have `reap_expired` return the reclaimed rows' identifiers:

**Sub-decision:** Either (a) extend `JobStore::reap_expired` to return `Vec<ReclaimedRow>`, or (b) keep the count and emit a generic "reaper reclaimed N orphans" log without per-row metric.

The spec §4.6 calls for per-row Failed emissions. Go with (a):

In `cairn-core::contract::job_store::mod.rs`, add:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReclaimedRow {
    pub job_id: JobId,
    pub kind: JobKind,
    pub attempts: u32,
}
```

Change `reap_expired` to return `Result<Vec<ReclaimedRow>, JobStoreError>`. Update the SQLite adapter's `reap_expired` to SELECT the reclaimed rows before / using RETURNING and return them.

- [ ] **Step 2: Update reaper to emit one Failed per reclaimed row**

```rust
match store.reap_expired(now).await {
    Ok(rows) => {
        for r in rows {
            let _ = metrics.emit(MetricEvent::WorkflowJobFailed {
                ts_ms: now,
                job_id: r.job_id.to_string(),
                kind: r.kind.to_string(),
                attempts: r.attempts,
                disposition: "retry".into(),
                failure_class: "lease_lost".into(),
                last_error: "reaper reclaimed expired lease".into(),
                will_retry_at_ms: Some(now),
            }).await;
        }
    }
    Err(e) => warn!(error = %e, "reap failed"),
}
```

- [ ] **Step 3: Update all `reap_expired` call sites**

Search `rg "reap_expired" --type rust`. The callers that just count (existing reaper tick, startup reap in Task 2.7) can wrap with `.map(|v| v.len())` or accept the new return.

For the startup reap in `Scheduler::start`, the new return type fits:

```rust
match store.reap_expired(now).await {
    Ok(rows) if !rows.is_empty() => tracing::info!(reclaimed = rows.len(), "startup reap"),
    Ok(_) => {}
    Err(e) => tracing::warn!(error = %e, "startup reap failed; periodic reaper will recover"),
}
```

- [ ] **Step 4: Run tests**

```bash
cargo nextest run --workspace --locked --no-fail-fast
```
Expected: PASS, after updating any reaper tests that asserted `usize`.

### Task 3.5: Metrics emission integration fixture

**Files:**
- Create: `crates/cairn-workflows/tests/metrics_emission_fixture.rs`

- [ ] **Step 1: Write the test**

```rust
//! Issue #92 — worker emits Started → Completed for a Done outcome and
//! Started → Failed for a Retry outcome.

use std::sync::Arc;

use cairn_core::contract::job_store::{
    EnqueueRequest, FailureClass, JobId, JobKind, JobPayload, RetryPolicy,
};
use cairn_core::contract::metrics::{CapturingMetricsSink, MetricsSink};
use cairn_core::domain::metrics::MetricEvent;
use cairn_workflows::scheduler::{
    Clock, HandlerOutcome, HandlerRegistryBuilder, JobHandler, MockClock, Scheduler,
    SchedulerConfig, WorkerConfig, ReaperConfig,
};
use cairn_workflows::SqliteJobStore;
use rusqlite::Connection;

struct Ok;
#[async_trait::async_trait]
impl JobHandler for Ok {
    fn kind(&self) -> JobKind { JobKind::new("test.ok") }
    async fn handle(&self, _: &JobPayload) -> HandlerOutcome { HandlerOutcome::Done }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn happy_path_emits_started_then_completed() {
    let conn = Connection::open_in_memory().unwrap();
    cairn_workflows::sqlite_store::install_for_tests(&conn);
    let store = Arc::new(SqliteJobStore::new(conn).unwrap());
    let sink = Arc::new(CapturingMetricsSink::new());
    let registry = HandlerRegistryBuilder::default()
        .with(Arc::new(Ok))
        .build();
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(1_000));
    let config = SchedulerConfig {
        worker_count: 1,
        worker: WorkerConfig { lease_ms: 5_000, heartbeat_every_ms: 1_000, idle_poll_ms: 50 },
        reaper: ReaperConfig { interval_ms: 60_000 },
        metrics: sink.clone(),
    };
    store
        .enqueue(EnqueueRequest {
            job_id: JobId::new("j-ok"),
            kind: JobKind::new("test.ok"),
            payload: vec![],
            queue_key: None,
            dedupe_key: None,
            not_before_ms: 0,
            retry: RetryPolicy::DEFAULT,
        })
        .await
        .unwrap();
    let s = Scheduler::start("inc-t", store, &registry, clock, config).await;
    // Poll until two events captured (Started + Completed).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if sink.snapshot().await.len() >= 2 || std::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    s.shutdown().await;
    let events = sink.snapshot().await;
    assert!(events.iter().any(|e| matches!(e, MetricEvent::WorkflowJobStarted { .. })));
    assert!(events.iter().any(|e| matches!(e, MetricEvent::WorkflowJobCompleted { .. })));
}
```

Add a similar test for the Retry path that asserts a `WorkflowJobFailed` is emitted with `failure_class = "transient"`.

- [ ] **Step 2: Run**

```bash
cargo nextest run -p cairn-workflows --test metrics_emission_fixture --locked
```
Expected: PASS.

- [ ] **Step 3: Commit Phase 3**

```bash
git add -A
git commit -m "feat(workflows): emit Started/Completed/Failed metric events (#92, spec §4.6)"
```

### Task 3.6: Phase 3 verification gate

- [ ] **Step 1: Run §8 checks**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
```
Expected: all green.

---

## Phase 4 — Lint integration (PR-4)

### Task 4.1: Add `WorkflowJobsReader` trait

**Files:**
- Create: `crates/cairn-core/src/contract/workflow_jobs.rs`
- Modify: `crates/cairn-core/src/contract/mod.rs` — `pub mod workflow_jobs;` and re-export

- [ ] **Step 1: Write the trait**

```rust
//! `WorkflowJobsReader` — read-only view of `workflow_jobs` for lint.
//!
//! Issue #92. Spec §4.8.

use crate::contract::job_store::{FailureClass, JobId, JobKind};

/// One dead-letter row surfaced to lint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadLetterRow {
    pub job_id: JobId,
    pub kind: JobKind,
    pub attempts: u32,
    pub failure_class: FailureClass,
    pub last_error: String,
    pub dead_letter_at_ms: i64,
}

/// Read-only adapter for the lint `workflow_health` check.
pub trait WorkflowJobsReader: Send + Sync {
    /// Count of `state = 'failed'` rows whose `dead_letter_at_ms IS NOT NULL`.
    /// `Some(kind)` filters; `None` counts across all kinds.
    fn dead_letter_count(&self, kind: Option<&JobKind>) -> usize;

    /// `now_ms - next_run_at` for the oldest queued row matching `kind`.
    /// Returns `None` when no queued row exists.
    fn oldest_queued_age_ms(&self, kind: Option<&JobKind>, now_ms: i64) -> Option<i64>;

    /// `now_ms - lease_expires_at` for the leased row whose lease is held
    /// the longest (or whose lease is most expired). Returns `None` when
    /// no leased rows exist.
    fn longest_held_lease_ms(&self, now_ms: i64) -> Option<i64>;

    /// `completed_at_ms` of the most recent `state = 'done'` row for `kind`.
    /// Returns `None` when no row has completed.
    fn last_success_ms(&self, kind: &JobKind) -> Option<i64>;

    /// Up to `limit` dead-letter rows ordered by `dead_letter_at_ms` desc.
    fn dead_letter_rows(&self, limit: usize) -> Vec<DeadLetterRow>;
}
```

- [ ] **Step 2: Register the module**

In `crates/cairn-core/src/contract/mod.rs`, find where other contract modules are declared (e.g. `pub mod metrics;`). Add:

```rust
pub mod workflow_jobs;
```

- [ ] **Step 3: Build**

```bash
cargo check -p cairn-core --all-targets --locked
```
Expected: PASS.

### Task 4.2: Add `WorkflowJobsReader` mock + first failing test for `workflow_health`

**Files:**
- Create: `crates/cairn-core/src/verbs/lint/checks/workflow_health.rs`
- Modify: `crates/cairn-core/src/verbs/lint/checks/mod.rs` — `pub mod workflow_health;`

- [ ] **Step 1: Add the module declaration**

In `crates/cairn-core/src/verbs/lint/checks/mod.rs`, append:

```rust
pub mod workflow_health;
```

- [ ] **Step 2: Create the check skeleton + failing test**

```rust
//! Issue #92 — workflow health lint check.
//!
//! Reads `WorkflowJobsReader` and emits Findings:
//!   * WorkflowDeadLetter (Error)
//!   * WorkflowStuck (Warning)
//!   * WorkflowStaleSummary (Warning)
//!   * WorkflowOverdue (Warning)
//!
//! Spec §4.10.

use crate::contract::job_store::JobKind;
use crate::contract::workflow_jobs::{DeadLetterRow, WorkflowJobsReader};
use crate::generated::verbs::lint::{Finding, Kind, Severity, Target};
use crate::verbs::lint::{LintInputs, finding};

pub fn run(inputs: &LintInputs<'_>) -> Vec<Finding> {
    let Some(jobs) = inputs.workflow_jobs else {
        return vec![];
    };
    let mut out = Vec::new();
    let cfg = inputs.config.workflows.lint.clone();
    let now = inputs.now_ms;
    for row in jobs.dead_letter_rows(cfg.max_dead_letter_listed as usize) {
        out.push(dead_letter_finding(&row));
    }
    if let Some(age) = jobs.oldest_queued_age_ms(None, now) {
        if age > cfg.stuck_queue_threshold_ms {
            out.push(stuck_finding(age));
        }
    }
    if let Some(t) = jobs.last_success_ms(&JobKind::new("dream.light")) {
        let age = now - t;
        if age > cfg.stale_dream_threshold_ms {
            out.push(stale_summary_finding(age));
        }
    }
    for kind in ["expire.tier", "evaluate.sweep"] {
        if let Some(t) = jobs.last_success_ms(&JobKind::new(kind)) {
            let age = now - t;
            if age > cfg.overdue_threshold_ms {
                out.push(overdue_finding(kind, age));
            }
        }
    }
    out
}

fn dead_letter_finding(row: &DeadLetterRow) -> Finding {
    let mut f = finding(
        Kind::WorkflowDeadLetter,
        Severity::Error,
        format!(
            "workflow {kind} job {job_id} dead-lettered after {attempts} attempts ({class}): {err}",
            kind = row.kind,
            job_id = row.job_id,
            attempts = row.attempts,
            class = row.failure_class.as_str(),
            err = row.last_error,
        ),
    );
    f.target = Some(Target {
        record_id: None,
        operation_id: Some(row.job_id.to_string()),
        path: None,
    });
    f
}

fn stuck_finding(age_ms: i64) -> Finding {
    finding(
        Kind::WorkflowStuck,
        Severity::Warning,
        format!("oldest queued workflow job has waited {age_ms}ms — workers idle?"),
    )
}

fn stale_summary_finding(age_ms: i64) -> Finding {
    finding(
        Kind::WorkflowStaleSummary,
        Severity::Warning,
        format!("no dream.light success in {age_ms}ms — rolling summary may be stale"),
    )
}

fn overdue_finding(kind: &str, age_ms: i64) -> Finding {
    finding(
        Kind::WorkflowOverdue,
        Severity::Warning,
        format!("no {kind} success in {age_ms}ms — schedule may be stalled"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::job_store::{FailureClass, JobId};
    use crate::contract::workflow_jobs::{DeadLetterRow, WorkflowJobsReader};
    use crate::verbs::lint::{empty_lint_inputs_with_reader, MockWorkflowJobsReader};

    #[test]
    fn dead_letter_row_emits_error_finding() {
        let row = DeadLetterRow {
            job_id: JobId::new("j-dead"),
            kind: JobKind::new("dream.light"),
            attempts: 3,
            failure_class: FailureClass::Validation,
            last_error: "bad payload".into(),
            dead_letter_at_ms: 500,
        };
        let reader = MockWorkflowJobsReader::default().with_dead_letter(row.clone());
        let inputs = empty_lint_inputs_with_reader(&reader, 1_000);
        let findings = super::run(&inputs);
        assert_eq!(findings.len(), 1);
        assert!(matches!(findings[0].severity, Severity::Error));
        assert!(matches!(findings[0].kind, Kind::WorkflowDeadLetter));
    }

    // Add stuck, stale, overdue tests in Task 4.4.
}
```

This test won't compile yet — we need `MockWorkflowJobsReader`, `empty_lint_inputs_with_reader`, the new `Kind` variants, `inputs.now_ms`, and `inputs.config.workflows.lint`. Add them in subsequent tasks.

### Task 4.3: Add 4 new `Kind` variants to the IDL

**Files:**
- Modify: IDL source for `verbs::lint::Kind` (search via `rg "WorkflowDeadLetter\|Kind::IndexDrift" crates/cairn-idl --type yaml --type rust` and find the canonical source)

- [ ] **Step 1: Locate the IDL definition**

Run:
```bash
rg "IndexDrift" crates/cairn-idl/ -l
rg "Kind =" crates/cairn-idl/sources -l
ls crates/cairn-idl/sources
```

The codegen-driven `Kind` enum source is likely under `crates/cairn-idl/sources/verbs/lint.{yaml,json}` or similar. Inspect to confirm.

- [ ] **Step 2: Add the four variants**

Append the new variants alphabetically near `StaleSchema` / `StaleProfileLine` so the generated output stays diff-friendly:

```yaml
# (illustrative — match actual IDL syntax)
  - name: WorkflowDeadLetter
    description: "Workflow job dead-lettered after exhausting retries"
  - name: WorkflowOverdue
    description: "Workflow kind has not run successfully in too long"
  - name: WorkflowStaleSummary
    description: "Dream summary workflow has not produced a success recently"
  - name: WorkflowStuck
    description: "Workflow queue has rows that have not leased in too long"
```

- [ ] **Step 3: Regenerate**

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked
```

This rewrites `crates/cairn-core/src/generated/verbs/lint.rs` and any sibling generated files. Inspect the diff.

- [ ] **Step 4: Update the `kind_key` match in lint/mod.rs**

In `crates/cairn-core/src/verbs/lint/mod.rs:297-330`, add the four new arms:

```rust
        Kind::WorkflowDeadLetter => "workflow_dead_letter",
        Kind::WorkflowStuck => "workflow_stuck",
        Kind::WorkflowStaleSummary => "workflow_stale_summary",
        Kind::WorkflowOverdue => "workflow_overdue",
```

- [ ] **Step 5: Build**

```bash
cargo check -p cairn-core --all-targets --locked
```
Expected: PASS.

### Task 4.4: Extend `LintInputs` with `workflow_jobs` and `now_ms`

**Files:**
- Modify: `crates/cairn-core/src/verbs/lint/mod.rs:88-152`

- [ ] **Step 1: Add the two fields to `LintInputs`**

```rust
    /// Read-only adapter for `workflow_jobs` (Issue #92, spec §4.8).
    /// `None` keeps the `workflow_health` check on the no-op path for
    /// fixture-only tests of other checks.
    pub workflow_jobs: Option<&'a (dyn crate::contract::workflow_jobs::WorkflowJobsReader + 'a)>,
    /// Wall-clock for time-based lint checks. CLI passes
    /// `SystemClock::now_ms()`; tests pass synthetic values.
    pub now_ms: i64,
```

- [ ] **Step 2: Update `Debug` impl**

In the `impl std::fmt::Debug for LintInputs<'_>` (line ~135), add:

```rust
            .field("workflow_jobs", &self.workflow_jobs.is_some())
            .field("now_ms", &self.now_ms)
```

- [ ] **Step 3: Add `workflow_health::run(inputs)` to `run_checks`**

In `run_checks` (around line 248), append after the existing `checks::index_drift::run` call:

```rust
    findings.extend(checks::workflow_health::run(inputs));
```

- [ ] **Step 4: Add `MockWorkflowJobsReader` for tests**

Append to `crates/cairn-core/src/verbs/lint/mod.rs` (in the `#[cfg(test)]` test-helpers region near `empty_*` functions):

```rust
#[cfg(test)]
#[derive(Default)]
pub(crate) struct MockWorkflowJobsReader {
    pub dead_letter: Vec<crate::contract::workflow_jobs::DeadLetterRow>,
    pub oldest_queued_age: Option<i64>,
    pub longest_lease: Option<i64>,
    pub last_success: std::collections::HashMap<String, i64>,
}

#[cfg(test)]
impl MockWorkflowJobsReader {
    pub fn with_dead_letter(mut self, row: crate::contract::workflow_jobs::DeadLetterRow) -> Self {
        self.dead_letter.push(row);
        self
    }
    pub fn with_oldest_queued_age(mut self, age_ms: i64) -> Self {
        self.oldest_queued_age = Some(age_ms);
        self
    }
    pub fn with_last_success(mut self, kind: &str, ms: i64) -> Self {
        self.last_success.insert(kind.to_string(), ms);
        self
    }
}

#[cfg(test)]
impl crate::contract::workflow_jobs::WorkflowJobsReader for MockWorkflowJobsReader {
    fn dead_letter_count(&self, _: Option<&crate::contract::job_store::JobKind>) -> usize {
        self.dead_letter.len()
    }
    fn oldest_queued_age_ms(&self, _: Option<&crate::contract::job_store::JobKind>, _: i64) -> Option<i64> {
        self.oldest_queued_age
    }
    fn longest_held_lease_ms(&self, _: i64) -> Option<i64> {
        self.longest_lease
    }
    fn last_success_ms(&self, kind: &crate::contract::job_store::JobKind) -> Option<i64> {
        self.last_success.get(kind.as_str()).copied()
    }
    fn dead_letter_rows(&self, limit: usize) -> Vec<crate::contract::workflow_jobs::DeadLetterRow> {
        self.dead_letter.iter().take(limit).cloned().collect()
    }
}

#[cfg(test)]
pub(crate) fn empty_lint_inputs_with_reader<'a>(
    reader: &'a dyn crate::contract::workflow_jobs::WorkflowJobsReader,
    now_ms: i64,
) -> LintInputs<'a> {
    use std::sync::OnceLock;
    static CFG: OnceLock<crate::config::CairnConfig> = OnceLock::new();
    let cfg = CFG.get_or_init(crate::config::CairnConfig::default);
    LintInputs {
        records: &[],
        config: cfg,
        index_stats: crate::contract::memory_store::IndexStats::new(0, 0),
        author_states: crate::verbs::lint::empty_author_states(),
        unresolvable_authors: crate::verbs::lint::empty_unresolvable_authors(),
        consent_lookup: None,
        source_artifacts: crate::verbs::lint::empty_source_artifacts(),
        source_forgets: crate::verbs::lint::empty_source_forgets(),
        vault_root: None,
        hot_body_loader: None,
        source_resolver: crate::verbs::lint::empty_source_resolver(),
        consent_journal: crate::verbs::lint::empty_consent_journal(),
        workflow_jobs: Some(reader),
        now_ms,
    }
}
```

- [ ] **Step 5: Update every existing `LintInputs { … }` construction**

Search:
```bash
rg "LintInputs \{" --type rust
```

For each construction (including the two in `lint/mod.rs` tests `run_checks_on_empty_inputs_returns_no_findings_yet` and `run_checks_is_record_order_independent`, plus `run_checks_with_one_record_aggregates_summary_correctly`, plus any in `cairn-cli`), add:

```rust
            workflow_jobs: None,
            now_ms: 0,
```

- [ ] **Step 6: Add the remaining workflow_health tests**

In `crates/cairn-core/src/verbs/lint/checks/workflow_health.rs` `#[cfg(test)] mod tests`, add:

```rust
    #[test]
    fn stuck_queue_emits_warning_when_above_threshold() {
        let reader = MockWorkflowJobsReader::default()
            .with_oldest_queued_age(11 * 60_000);  // 11 minutes > 10 min default
        let inputs = empty_lint_inputs_with_reader(&reader, 1_000_000);
        let findings = super::run(&inputs);
        assert!(findings.iter().any(|f| matches!(f.kind, Kind::WorkflowStuck)));
    }

    #[test]
    fn stale_dream_emits_warning() {
        let reader = MockWorkflowJobsReader::default()
            .with_last_success("dream.light", 0);
        let inputs = empty_lint_inputs_with_reader(&reader, 25 * 3_600_000); // 25 h > 24 h
        let findings = super::run(&inputs);
        assert!(findings.iter().any(|f| matches!(f.kind, Kind::WorkflowStaleSummary)));
    }

    #[test]
    fn overdue_expire_emits_warning() {
        let reader = MockWorkflowJobsReader::default()
            .with_last_success("expire.tier", 0);
        let inputs = empty_lint_inputs_with_reader(&reader, 49 * 3_600_000); // 49 h > 48 h
        let findings = super::run(&inputs);
        assert!(findings.iter().any(|f| matches!(f.kind, Kind::WorkflowOverdue)));
    }

    #[test]
    fn missing_reader_emits_nothing() {
        let cfg = crate::config::CairnConfig::default();
        let inputs = crate::verbs::lint::LintInputs {
            records: &[],
            config: &cfg,
            index_stats: crate::contract::memory_store::IndexStats::new(0, 0),
            author_states: crate::verbs::lint::empty_author_states(),
            unresolvable_authors: crate::verbs::lint::empty_unresolvable_authors(),
            consent_lookup: None,
            source_artifacts: crate::verbs::lint::empty_source_artifacts(),
            source_forgets: crate::verbs::lint::empty_source_forgets(),
            vault_root: None,
            hot_body_loader: None,
            source_resolver: crate::verbs::lint::empty_source_resolver(),
            consent_journal: crate::verbs::lint::empty_consent_journal(),
            workflow_jobs: None,
            now_ms: 0,
        };
        assert!(super::run(&inputs).is_empty());
    }
```

This test suite presumes `inputs.config.workflows.lint` exists; that's added in Phase 5 (Task 5.1). For now, gate the test reads on a temporary const if needed, OR land Phase 5's config block before this task. **Sequencing note:** if you prefer single-PR-per-phase, swap the order of Phase 4 and Phase 5 so the config exists when this test runs. The plan as written below assumes Phase 5 lands after — make Task 5.1 a prerequisite of these tests.

**Pragmatic path:** introduce a minimal `WorkflowsLintConfig` struct with hard-coded defaults inside this task, then enrich it in Phase 5. The struct lives at `crates/cairn-core/src/config/workflows.rs` from the start:

```rust
// crates/cairn-core/src/config/workflows.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowsConfig {
    pub lint: WorkflowsLintConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowsLintConfig {
    pub max_dead_letter_listed: u32,
    pub stuck_queue_threshold_ms: i64,
    pub stale_dream_threshold_ms: i64,
    pub overdue_threshold_ms: i64,
}

impl Default for WorkflowsConfig {
    fn default() -> Self { Self { lint: WorkflowsLintConfig::default() } }
}
impl Default for WorkflowsLintConfig {
    fn default() -> Self {
        Self {
            max_dead_letter_listed: 10,
            stuck_queue_threshold_ms: 600_000,
            stale_dream_threshold_ms: 86_400_000,
            overdue_threshold_ms: 172_800_000,
        }
    }
}
```

Register in `crates/cairn-core/src/config/mod.rs`:
- Add `pub mod workflows;`
- Add `pub workflows: workflows::WorkflowsConfig,` to `CairnConfig`
- Default impl on `CairnConfig` initializes `workflows: WorkflowsConfig::default()`

Phase 5 adds serde wiring + the YAML schema.

- [ ] **Step 7: Build + run lint tests**

```bash
cargo check --workspace --all-targets --locked
cargo nextest run -p cairn-core --locked --no-fail-fast workflow_health
cargo nextest run -p cairn-core --locked --no-fail-fast verbs::lint::tests
```
Expected: PASS.

### Task 4.5: Implement `WorkflowJobsReader` for the SQLite store

**Files:**
- Create: `crates/cairn-store-sqlite/src/workflow_jobs_reader.rs` (or pick a location matching the crate's convention; verify by inspecting `crates/cairn-store-sqlite/src/lib.rs` to see how other adapter impls are organized)
- Modify: `crates/cairn-store-sqlite/src/lib.rs` — register module + re-export the impl

- [ ] **Step 1: Inspect the crate layout**

```bash
ls crates/cairn-store-sqlite/src/
sed -n '1,40p' crates/cairn-store-sqlite/src/lib.rs
```

- [ ] **Step 2: Write the impl**

Sketch — adjust to match the crate's existing connection-access pattern:

```rust
use std::sync::Mutex;

use cairn_core::contract::job_store::{FailureClass, JobId, JobKind};
use cairn_core::contract::workflow_jobs::{DeadLetterRow, WorkflowJobsReader};
use rusqlite::Connection;

pub struct SqliteWorkflowJobsReader {
    conn: Mutex<Connection>,
}

impl SqliteWorkflowJobsReader {
    #[must_use]
    pub fn new(conn: Connection) -> Self {
        Self { conn: Mutex::new(conn) }
    }
}

impl WorkflowJobsReader for SqliteWorkflowJobsReader {
    fn dead_letter_count(&self, kind: Option<&JobKind>) -> usize {
        let conn = self.conn.lock().expect("workflow_jobs_reader poisoned");
        let (sql, bind): (&str, Option<&str>) = match kind {
            Some(k) => (
                "SELECT count(*) FROM workflow_jobs WHERE dead_letter_at_ms IS NOT NULL AND kind = ?1",
                Some(k.as_str()),
            ),
            None => ("SELECT count(*) FROM workflow_jobs WHERE dead_letter_at_ms IS NOT NULL", None),
        };
        let count: i64 = if let Some(k) = bind {
            conn.query_row(sql, rusqlite::params![k], |r| r.get(0))
        } else {
            conn.query_row(sql, [], |r| r.get(0))
        }
        .unwrap_or(0);
        count.try_into().unwrap_or(0)
    }

    fn oldest_queued_age_ms(&self, kind: Option<&JobKind>, now_ms: i64) -> Option<i64> {
        let conn = self.conn.lock().expect("poisoned");
        let row: Option<i64> = match kind {
            Some(k) => conn
                .query_row(
                    "SELECT min(next_run_at) FROM workflow_jobs WHERE state = 'queued' AND kind = ?1",
                    rusqlite::params![k.as_str()],
                    |r| r.get(0),
                )
                .ok(),
            None => conn
                .query_row(
                    "SELECT min(next_run_at) FROM workflow_jobs WHERE state = 'queued'",
                    [],
                    |r| r.get(0),
                )
                .ok(),
        };
        row.and_then(|t| if t > 0 { Some(now_ms - t) } else { None })
    }

    fn longest_held_lease_ms(&self, now_ms: i64) -> Option<i64> {
        let conn = self.conn.lock().expect("poisoned");
        conn.query_row(
            "SELECT min(lease_expires_at) FROM workflow_jobs WHERE state = 'leased'",
            [],
            |r| r.get::<_, Option<i64>>(0),
        )
        .ok()
        .flatten()
        .map(|t| now_ms - t)
    }

    fn last_success_ms(&self, kind: &JobKind) -> Option<i64> {
        let conn = self.conn.lock().expect("poisoned");
        conn.query_row(
            "SELECT max(completed_at_ms) FROM workflow_jobs WHERE kind = ?1 AND state = 'done'",
            rusqlite::params![kind.as_str()],
            |r| r.get::<_, Option<i64>>(0),
        )
        .ok()
        .flatten()
    }

    fn dead_letter_rows(&self, limit: usize) -> Vec<DeadLetterRow> {
        let conn = self.conn.lock().expect("poisoned");
        let mut stmt = match conn.prepare(
            "SELECT job_id, kind, attempts, failure_class, last_error, dead_letter_at_ms
               FROM workflow_jobs
              WHERE dead_letter_at_ms IS NOT NULL
              ORDER BY dead_letter_at_ms DESC
              LIMIT ?1",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        stmt.query_map(rusqlite::params![limit as i64], |r| {
            let job_id: String = r.get(0)?;
            let kind: String = r.get(1)?;
            let attempts: i64 = r.get(2)?;
            let class: String = r.get(3)?;
            let last_error: String = r.get(4)?;
            let dl_at: i64 = r.get(5)?;
            Ok(DeadLetterRow {
                job_id: JobId::new(job_id),
                kind: JobKind::new(kind),
                attempts: attempts.try_into().unwrap_or(0),
                failure_class: class.parse().unwrap_or(FailureClass::Transient),
                last_error,
                dead_letter_at_ms: dl_at,
            })
        })
        .map(|it| it.flatten().collect())
        .unwrap_or_default()
    }
}
```

Note: this reader keeps a separate `Mutex<Connection>` — adjust to share the main store's connection if the crate already provides a pooled handle. Inspect `SqliteStore` / `SqliteJobStore` for the pattern.

- [ ] **Step 3: Register the module**

In `crates/cairn-store-sqlite/src/lib.rs`, add `pub mod workflow_jobs_reader;` and re-export `SqliteWorkflowJobsReader`.

- [ ] **Step 4: Add a fixture test against a real SQLite**

Create `crates/cairn-store-sqlite/tests/workflow_jobs_reader.rs`:

```rust
//! Reader returns correct counts and lists against a real SQLite store.

use cairn_core::contract::job_store::{FailureClass, JobKind};
use cairn_core::contract::workflow_jobs::WorkflowJobsReader;
use rusqlite::Connection;

fn fresh_db() -> Connection {
    let mut conn = Connection::open_in_memory().expect("open");
    cairn_store_sqlite::migrations::migrations()
        .to_latest(&mut conn)
        .expect("migrate");
    conn
}

#[test]
fn dead_letter_rows_returns_failed_with_columns_populated() {
    let conn = fresh_db();
    conn.execute_batch(
        "INSERT INTO workflow_jobs (job_id, kind, payload, state, attempts, delivery_count, max_attempts, base_backoff_ms, backoff_multiplier, max_backoff_ms, queue_key, dedupe_key, next_run_at, enqueued_at, updated_at, failure_class, dead_letter_at_ms, last_error)
         VALUES ('j-1', 'dream.light', X'', 'failed', 3, 3, 3, 1, 2, 60000, NULL, NULL, 0, 0, 0, 'validation', 500, 'bad payload');"
    ).expect("insert");
    let reader = cairn_store_sqlite::SqliteWorkflowJobsReader::new(conn);
    let rows = reader.dead_letter_rows(10);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].failure_class, FailureClass::Validation);
    assert_eq!(rows[0].last_error, "bad payload");
    assert_eq!(reader.dead_letter_count(None), 1);
    assert_eq!(reader.dead_letter_count(Some(&JobKind::new("dream.light"))), 1);
}
```

- [ ] **Step 5: Run**

```bash
cargo nextest run -p cairn-store-sqlite --test workflow_jobs_reader --locked
```
Expected: PASS.

### Task 4.6: Wire the reader through `cairn-cli` lint

**Files:**
- Modify: `crates/cairn-cli/src/...` lint dispatch (search for the `LintInputs { … }` construction in cairn-cli)

- [ ] **Step 1: Locate the construction**

```bash
rg "LintInputs \{" crates/cairn-cli/src -l
```

- [ ] **Step 2: Build and pass the reader + `now_ms`**

At the construction site, instantiate `SqliteWorkflowJobsReader` (sharing the open connection if the CLI's store exposes one; otherwise open a read-only connection to the same vault DB), and pass it plus `SystemClock::now_ms()` (or equivalent — verify naming).

- [ ] **Step 3: Build + tests**

```bash
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
```

### Task 4.7: workflow_health insta snapshot tests

**Files:**
- Modify: `crates/cairn-core/src/verbs/lint/checks/workflow_health.rs`

- [ ] **Step 1: Add snapshot tests**

After the existing unit tests, append:

```rust
    #[test]
    fn snapshot_dead_letter_finding() {
        let row = DeadLetterRow {
            job_id: JobId::new("01JTESTJOBDEADLETTER0001"),
            kind: JobKind::new("dream.light"),
            attempts: 3,
            failure_class: FailureClass::Poison,
            last_error: "panic in step 2".into(),
            dead_letter_at_ms: 1_700_000_000_000,
        };
        let reader = MockWorkflowJobsReader::default().with_dead_letter(row);
        let inputs = empty_lint_inputs_with_reader(&reader, 1_700_000_001_000);
        let findings = super::run(&inputs);
        insta::assert_yaml_snapshot!(findings);
    }
```

- [ ] **Step 2: Generate snapshot**

```bash
cargo nextest run -p cairn-core --locked --no-fail-fast workflow_health::tests::snapshot
cargo insta review
```
Accept the snapshot when it looks right (one finding, severity Error, message containing the kind + job_id + poison + reason).

- [ ] **Step 3: Commit Phase 4**

```bash
git add -A
git commit -m "feat(lint): workflow_health check + WorkflowJobsReader contract (#92, spec §4.8-4.10)"
```

### Task 4.8: Phase 4 verification gate

- [ ] **Step 1: Run §8 checks**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```
Expected: all green.

---

## Phase 5 — Config schema + docs (PR-5)

### Task 5.1: Add serde + YAML to `WorkflowsConfig`

**Files:**
- Modify: `crates/cairn-core/src/config/workflows.rs` (the file created in Task 4.4)

- [ ] **Step 1: Add `Serialize`, `Deserialize`, `#[serde(default)]`**

Replace the file:

```rust
//! Workflows config block.
//! Issue #92, spec §4.11.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkflowsConfig {
    pub lint: WorkflowsLintConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkflowsLintConfig {
    /// Max number of dead-letter rows surfaced as Error findings.
    pub max_dead_letter_listed: u32,
    /// Oldest-queued-row age threshold for `WorkflowStuck` finding, ms.
    pub stuck_queue_threshold_ms: i64,
    /// `dream.light` last-success staleness threshold, ms.
    pub stale_dream_threshold_ms: i64,
    /// `expire.*` / `evaluate.*` last-success staleness threshold, ms.
    pub overdue_threshold_ms: i64,
}

impl Default for WorkflowsConfig {
    fn default() -> Self {
        Self { lint: WorkflowsLintConfig::default() }
    }
}

impl Default for WorkflowsLintConfig {
    fn default() -> Self {
        Self {
            max_dead_letter_listed: 10,
            stuck_queue_threshold_ms: 600_000,
            stale_dream_threshold_ms: 86_400_000,
            overdue_threshold_ms: 172_800_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_what_the_spec_says() {
        let c = WorkflowsLintConfig::default();
        assert_eq!(c.max_dead_letter_listed, 10);
        assert_eq!(c.stuck_queue_threshold_ms, 600_000);
        assert_eq!(c.stale_dream_threshold_ms, 86_400_000);
        assert_eq!(c.overdue_threshold_ms, 172_800_000);
    }

    #[test]
    fn missing_lint_block_yields_defaults() {
        let yaml = "{}";
        let c: WorkflowsConfig = serde_yaml::from_str(yaml).expect("parse");
        assert_eq!(c, WorkflowsConfig::default());
    }

    #[test]
    fn partial_lint_block_merges_with_defaults() {
        let yaml = r#"
lint:
  max_dead_letter_listed: 25
"#;
        let c: WorkflowsConfig = serde_yaml::from_str(yaml).expect("parse");
        assert_eq!(c.lint.max_dead_letter_listed, 25);
        // Untouched fields stay default.
        assert_eq!(c.lint.stuck_queue_threshold_ms, 600_000);
    }
}
```

- [ ] **Step 2: Register field in `CairnConfig`**

In `crates/cairn-core/src/config/mod.rs`:
- Add `pub mod workflows;` near the other `pub mod` lines.
- Add `pub workflows: workflows::WorkflowsConfig,` to the `CairnConfig` struct.
- Add `#[serde(default)]` on the field if `CairnConfig` uses serde.
- Initialize in `Default for CairnConfig`.

Verify the existing `CairnConfig` shape by reading the file before editing.

- [ ] **Step 3: Build + test**

```bash
cargo check -p cairn-core --all-targets --locked
cargo nextest run -p cairn-core --locked --no-fail-fast config::workflows
```
Expected: PASS.

### Task 5.2: Regenerate docs

- [ ] **Step 1: Run docgen**

```bash
cargo run -p cairn-cli --bin cairn-docgen --locked -- --write
```

Inspect new / modified files under `docs/site/src/reference/generated/`. The new config keys + lint findings should appear in the generated reference.

- [ ] **Step 2: Build the mdbook**

```bash
mdbook build docs/site
```
Expected: PASS, no broken links.

- [ ] **Step 3: Commit Phase 5**

```bash
git add -A
git commit -m "feat(config): workflows.lint block + regenerated docs (#92, spec §4.11)"
```

### Task 5.3: Phase 5 verification gate (full)

- [ ] **Step 1: Run the complete CLAUDE.md §8 checklist**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
mdbook build docs/site
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" \
  cargo doc --workspace --no-deps --document-private-items --locked
cargo deny check
cargo audit --deny warnings
cargo machete
```
Expected: all green.

### Task 5.4: Open PRs in order

- [ ] **Step 1: Phase 1 PR**

```bash
git push -u origin <branch>
gh pr create --title "feat(workflows): FailureClass + HandlerOutcome class (#92 PR-1)" --body "$(cat <<'EOF'
## Summary
- Add `FailureClass` taxonomy in `cairn-core::contract::job_store`
- Evolve `HandlerOutcome::Retry` / `Permanent` to carry a `FailureClass`
- Worker enforces Validation/Poison → Permanent invariant
- 3 workflow handlers + adapter signature updated; columns persisted in PR-2

Spec: `docs/superpowers/specs/2026-05-15-workflow-recovery-design.md` §4.2-4.4

## Test plan
- [x] Unit tests for `FailureClass` serde + helpers
- [x] rstest matrix for worker class-override invariants
- [x] `cargo nextest run --workspace --locked`
EOF
)"
```

- [ ] **Step 2 - 5:** Repeat for Phase 2–5 PRs after the prior one lands.

---

## Self-Review Notes

**Spec coverage check** (each spec section → task):

| Spec section | Tasks |
|---|---|
| §4.1 Architecture | (no task; described in plan) |
| §4.2 FailureClass | 1.1 |
| §4.3 HandlerOutcome | 1.3, 1.6, 1.7, 1.8 |
| §4.4 JobStore::fail signature + LeasedJob | 1.2, 1.5 |
| §4.5 Migration 0062 | 2.1, 2.2, 2.3 |
| §4.6 MetricEvent variants | 3.1, 3.3, 3.4 |
| §4.7 Crash recovery | 2.7, 2.8 |
| §4.8 WorkflowJobsReader | 4.1, 4.5 |
| §4.9 completed_at_ms | 2.1, 2.5 |
| §4.10 workflow_health check | 4.2, 4.4, 4.7 |
| §4.11 Config block | 4.4 (skeleton), 5.1 (serde) |
| §4.12 now_ms in LintInputs | 4.4 |
| §4.13 Error handling | 1.4 (debug_assert), 3.3 (sink emit ignores errors) |
| §5 Testing strategy | every TDD step + integration fixtures in 2.6, 2.8, 3.5, 4.5, 4.7 |
| §6 Implementation order | Phases 1-5 map 1:1 |
| §7 Verification | Tasks 1.10, 2.9, 3.6, 4.8, 5.3 |

**Placeholder scan:** None.

**Type consistency:** `FailureClass::as_str()` defined in 2.4 referenced in 3.3, 4.2, 4.5 — matches. `WorkflowJobsReader` defined in 4.1 matches signatures in 4.2 and 4.5. `WorkflowsLintConfig` field names match between 4.4 and 5.1.

**Known sequencing risk:** Task 4.4 introduces a minimal `WorkflowsConfig` struct so the `workflow_health` check can read thresholds; Task 5.1 then enriches with serde. If a phase boundary is enforced strictly between PRs, the worker doing PR-4 must accept that a tiny config-shape contribution lives in PR-4 (annotated in the commit so PR-5 doesn't appear to do less work than it claims).
