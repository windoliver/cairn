# Workflow recovery, retry policy, metrics, and lint integration

**Issue:** [#92](https://github.com/windoliver/cairn/issues/92)
**Parent epic:** [#16](https://github.com/windoliver/cairn/issues/16) — local workflow orchestrator
**Predecessor:** [#91](https://github.com/windoliver/cairn/issues/91) — Dream/Expire/Evaluate minimum paths
**Brief sections:** §5.6 WAL · §10 Continuous Learning · §15 Evaluation
**Phase:** v0.1 — P0
**Date:** 2026-05-15

## 1. Goal

Harden the local Tokio workflow orchestrator that landed in #91 so that:

- Crash mid-execution does not lose a job or double-apply a mutation.
- Repeated failures are surfaced (dead-letter), not silently retried forever.
- Workflow duration, queue lag, retry count, and failure reason emit as
  structured `MetricEvent`s to `.cairn/metrics.jsonl`.
- `cairn lint` reports workflow health (stuck jobs, repeated failures, stale
  summaries, overdue expiration/evaluation) with `job_id` and `JobKind` so the
  output is actionable.

Out of scope (deferred to v0.2): OpenTelemetry export, dashboards, alerting
sinks, multi-process orchestration semantics.

## 2. Non-goals

- Replacing `HandlerOutcome` with a typed-error contract across handlers. The
  binary `Retry` / `Permanent` shape stays; we add a typed `FailureClass`
  taxonomy on top.
- Splitting `workflow_jobs` into a separate `workflow_dead_letter` table.
  Dead-letter is a state of the same row, not a different table.
- Adding live re-queue UX to `cairn lint`. Lint reports; remediation is a
  follow-up `cairn admin` subcommand (filed separately if needed).

## 3. Background — what #91 left in place

`crates/cairn-workflows/src/scheduler/` already provides:

- `Scheduler::start` spawns N workers + a reaper over a `JobStore`.
- `worker::run_worker` races handler / heartbeat / deadline watchdog. Lease
  loss propagates through a `CancellationToken`; the worker abandons execution
  rather than committing side effects under a dead lease.
- `reaper::run_reaper` calls `JobStore::reap_expired` every 5 s.
- `JobStore` trait (in `cairn-core::contract::job_store`) defines
  `enqueue / lease / heartbeat / complete / fail / reap_expired`, plus
  `RetryPolicy { max_attempts, base_backoff_ms, backoff_multiplier,
  max_backoff_ms }` with `DEFAULT = 5 × 1s × 2 capped at 60 s`.
- `HandlerOutcome = { Done | Retry { reason } | Permanent { reason } }`.
- `FailDisposition = { Retry | Permanent }`; the store auto-terminates a
  `Retry` when `attempts == max_attempts`.
- SQLite adapter in `crates/cairn-store-sqlite/src/` exposes `SqliteJobStore`
  with migration 0020 owning the `workflow_jobs` table.

Three workflow handlers exist (`dream.*`, `expire.*`, `evaluate.*`) all
returning today's binary outcome.

`MetricEvent` (in `cairn-core::domain::metrics`) currently has two variants:
`HotPrefixAssembled` and `EvaluationCompleted`. Sink is the existing
`MetricsSink` trait writing JSONL to `.cairn/metrics.jsonl`.

`cairn-core::verbs::lint` runs 11 checks via `run_checks(&LintInputs)`. The
struct has no field exposing workflow / job state.

## 4. Design

### 4.1 Architecture

Three vertical slices threaded through existing crates. No new crate, no new
top-level contract.

```
┌─────────────────────────────────────────────────────────────────┐
│  cairn-workflows (scheduler::worker, ::reaper, ::scheduler)     │
│   • Worker emits MetricEvent on lease/complete/fail             │
│   • Scheduler::start runs one-shot reap before worker spawn     │
└────────────┬──────────────────────────┬─────────────────────────┘
             │                          │
             ▼                          ▼
┌─────────────────────────┐   ┌──────────────────────────────────┐
│ cairn-core::contract::  │   │ cairn-core::contract::            │
│  job_store              │   │  metrics (existing) +             │
│   + FailureClass enum   │   │  WorkflowJobsReader (new trait)   │
│   + fail() takes class  │   │   exposes counts for lint         │
└──────────┬──────────────┘   └────────────────┬─────────────────┘
           ▼                                    ▼
┌─────────────────────────┐   ┌──────────────────────────────────┐
│ cairn-store-sqlite      │   │ cairn-core::verbs::lint           │
│  migration 0021:        │   │   checks/workflow_health.rs (new) │
│   + dead_letter_at_ms   │   │   reads WorkflowJobsReader        │
│   + failure_class       │   │   emits Findings w/ job_id + kind │
└─────────────────────────┘   └──────────────────────────────────┘
```

### 4.2 `FailureClass` taxonomy

New enum in `cairn-core::contract::job_store`:

```rust
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
```

**Invariants** enforced in `worker::execute_one`:

1. Handlers may only return `Transient | Validation | Poison`. The scheduler
   stamps `Timeout | LeaseLost` itself. A `debug_assert!` guards the boundary;
   release builds map illegal classes to `Transient` and log `warn`.
2. `Validation` and `Poison` always terminate. If a handler returns
   `HandlerOutcome::Retry { class: Validation | Poison }`, the scheduler
   converts it to `FailDisposition::Permanent` before calling `JobStore::fail`.
3. `Transient` respects the handler's binary disposition. `Retry` keeps the
   row eligible until `attempts == max_attempts`; `Permanent` terminates now.

### 4.3 `HandlerOutcome` evolution

```rust
pub enum HandlerOutcome {
    Done,
    Retry { reason: String, class: FailureClass },
    Permanent { reason: String, class: FailureClass },
}
```

The three existing handlers (`dream.light`, `expire.tier`, `evaluate.sweep`)
get updated in the same PR that lands the enum change. Test fixtures use a
helper constructor `HandlerOutcome::transient_retry(reason)` etc. to keep
non-load-bearing test code terse.

### 4.4 `JobStore` contract changes

- `fail()` gains a `class: FailureClass` parameter.
- `LeasedJob` gains `failure_class: Option<FailureClass>` so a leased row
  carries its prior classification forward (lets handlers compare attempt N
  vs N–1 if they want to escalate `Transient → Poison`).
- `FailDisposition` is unchanged.

These are breaking signature changes inside `cairn-core`, so the PR updates
every call site in one pass.

### 4.5 SQLite migration 0021

New file `crates/cairn-store-sqlite/migrations/0021_workflow_dead_letter.sql`:

```sql
ALTER TABLE workflow_jobs ADD COLUMN failure_class    TEXT;
ALTER TABLE workflow_jobs ADD COLUMN dead_letter_at_ms INTEGER;

-- Lint hot-path index.
CREATE INDEX workflow_jobs_dead_letter_idx
  ON workflow_jobs(dead_letter_at_ms)
  WHERE dead_letter_at_ms IS NOT NULL;
```

The adapter's `fail()` implementation writes `failure_class` on every call,
and writes `dead_letter_at_ms = now_ms` exactly when the row's new state is
`Failed`. Both columns are nullable for backward-compat with existing 0020
rows.

### 4.6 New `MetricEvent` variants

Three additive variants, `#[non_exhaustive]` already on the enum:

```rust
#[serde(rename = "workflow_job_started")]
WorkflowJobStarted {
    ts_ms: i64,
    job_id: String,
    kind: String,
    attempts: u32,
    queue_lag_ms: i64,           // now_ms − not_before_ms
    dedupe_key: Option<String>,
},

#[serde(rename = "workflow_job_completed")]
WorkflowJobCompleted {
    ts_ms: i64,
    job_id: String,
    kind: String,
    attempts: u32,
    duration_ms: u64,            // ts_ms − started_at_ms (worker-local)
},

#[serde(rename = "workflow_job_failed")]
WorkflowJobFailed {
    ts_ms: i64,
    job_id: String,
    kind: String,
    attempts: u32,
    disposition: String,         // "retry" | "permanent"
    failure_class: String,       // snake_case of FailureClass
    last_error: String,
    will_retry_at_ms: Option<i64>,
},
```

Emission rules:

- `Started` on successful `lease()` return.
- `Completed` on successful `complete()` return.
- `Failed` on every `fail()` call (retry and permanent both). The reaper also
  emits `Failed` for each reclaimed row with `class = LeaseLost`.
- Emit happens **after** the store mutation lands. Sink errors are logged
  `warn` and never abort the job (brief §15: missing line preferable to a
  broken workflow).

### 4.7 Crash recovery

`Scheduler::start` becomes `async fn` and awaits one reap before spawning
workers:

```rust
pub async fn start(...) -> Self {
    let now = clock.now_ms();
    // Best-effort: log warn on backend error, continue startup.
    if let Err(e) = store.reap_expired(now).await {
        warn!(error = %e, "startup reap failed; falling back to periodic reaper");
    }
    // …spawn workers + periodic reaper as today
}
```

This closes the 0–5 s window where a crashed predecessor's leased rows are
unreclaimable. The periodic reaper still runs for steady-state recovery.
Callers are already inside a tokio runtime (CLI uses `#[tokio::main]`; tests
use `#[tokio::test]`), so making `start` async is a straight-through change —
the only call sites are the two test fixtures in `scheduler/mod.rs` and the
CLI bootstrap in `cairn-cli`.

Worker behavior on crash is unchanged: heartbeat watchdog already prevents
side-effect commits under a dead lease, and the lease nonce ensures a
re-leased row can't be `complete()`d by the original worker if it wakes up.

### 4.8 `WorkflowJobsReader` trait

```rust
// cairn-core::contract::workflow_jobs

pub trait WorkflowJobsReader: Send + Sync {
    fn dead_letter_count(&self, kind: Option<&JobKind>) -> usize;
    fn oldest_queued_age_ms(&self, kind: Option<&JobKind>, now_ms: i64) -> Option<i64>;
    fn longest_held_lease_ms(&self, now_ms: i64) -> Option<i64>;
    fn last_success_ms(&self, kind: &JobKind) -> Option<i64>;
    fn dead_letter_rows(&self, limit: usize) -> Vec<DeadLetterRow>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadLetterRow {
    pub job_id: JobId,
    pub kind: JobKind,
    pub attempts: u32,
    pub failure_class: FailureClass,
    pub last_error: String,
    pub dead_letter_at_ms: i64,
}
```

Sync trait — lint is sync today. The SQLite adapter implements it with
small read-only queries against `workflow_jobs`. `last_success_ms` reads
the most recent `state = 'done'` row for a kind (we don't track success time
explicitly today; this needs an extra column — see §4.9).

### 4.9 Tracking last-success timestamps

Lint's "stale summary" and "overdue eval" findings need to know when a kind
last completed. Two options:

- (a) Add `completed_at_ms` column to `workflow_jobs` (nullable until a row
  reaches Done).
- (b) Keep a derived `workflow_kind_health` projection updated on
  `complete()`.

We pick **(a)** for minimum churn: one more nullable column in migration
0021, populated in the adapter's `complete()` impl, indexed for kind-grouped
lookups.

```sql
ALTER TABLE workflow_jobs ADD COLUMN completed_at_ms INTEGER;
CREATE INDEX workflow_jobs_kind_completed_idx
  ON workflow_jobs(kind, completed_at_ms);
```

### 4.10 `workflow_health` lint check

New file `crates/cairn-core/src/verbs/lint/checks/workflow_health.rs`:

```rust
pub fn run(inputs: &LintInputs<'_>) -> Vec<Finding> {
    let Some(jobs) = inputs.workflow_jobs else {
        // No reader wired → fixture mode → no findings.
        return vec![];
    };
    let cfg = &inputs.config.workflows.lint;
    let now = inputs.now_ms;        // see §4.12 for clock injection
    let mut out = vec![];

    // 1. Dead-letter rows → Error per row.
    for row in jobs.dead_letter_rows(cfg.max_dead_letter_listed) {
        out.push(dead_letter_finding(&row));
    }

    // 2. Stuck queue → Warning if oldest Queued > threshold.
    if let Some(age) = jobs.oldest_queued_age_ms(None, now) {
        if age > cfg.stuck_queue_threshold_ms {
            out.push(stuck_finding(age));
        }
    }

    // 3. Stale dream summary.
    if let Some(t) = jobs.last_success_ms(&JobKind::new("dream.light")) {
        if (now - t) > cfg.stale_dream_threshold_ms {
            out.push(stale_summary_finding(now - t));
        }
    }

    // 4. Overdue expire / eval.
    for kind in ["expire.tier", "evaluate.sweep"] {
        if let Some(t) = jobs.last_success_ms(&JobKind::new(kind)) {
            if (now - t) > cfg.overdue_threshold_ms {
                out.push(overdue_finding(kind, now - t));
            }
        }
    }

    out
}
```

Finding kinds added to the IDL `Kind` enum:

- `WorkflowDeadLetter` (Error) — `target.operation_id = job_id`, message
  includes `kind`, `failure_class`, `last_error`.
- `WorkflowStuck` (Warning) — message includes oldest queued age.
- `WorkflowStaleSummary` (Warning) — message includes hours since last
  successful `dream.light`.
- `WorkflowOverdue` (Warning) — per overdue kind.

The new `Kind` variants require regenerating `cairn-idl` outputs (run
`cargo run -p cairn-idl --bin cairn-codegen` and commit, per CLAUDE.md §8).

### 4.11 Config block

New `workflows.lint` block in `CairnConfig`:

```yaml
workflows:
  lint:
    max_dead_letter_listed: 10        # max dead-letter rows surfaced as findings
    stuck_queue_threshold_ms: 600000  # 10 min
    stale_dream_threshold_ms: 86400000  # 24 h
    overdue_threshold_ms: 172800000     # 48 h
```

Defaults in `cairn-core::config::CairnConfig::default`. Missing or malformed
block falls back to defaults (fail-soft per §4.13 below).

### 4.12 Clock injection into lint

Lint currently builds `LintInputs` without a `now_ms` field. We add one:

```rust
pub struct LintInputs<'a> {
    // …existing fields…
    pub workflow_jobs: Option<&'a (dyn WorkflowJobsReader + 'a)>,
    pub now_ms: i64,
}
```

`cairn-cli`'s lint handler passes `SystemClock::now_ms()`. Tests pass a fixed
millisecond value. No new contract; just a primitive.

### 4.13 Error handling

| Failure | Where | Behavior |
|---|---|---|
| `JobStoreError::LeaseLost` on `complete/fail` | worker | log `warn`, abandon (existing) |
| `JobStoreError::Backend` on `lease/heartbeat/reap` | worker / reaper | log `warn`, continue (existing) |
| `MetricsError` on emit | worker after store mutation | log `warn`, never abort job |
| Handler panics across await | watchdog | watchdog fires `lease_lost`; reaper reclaims; retry with `class = Timeout` |
| `WorkflowJobsReader` query failure | lint dispatch | wrap in `LintError::DeferredCheck { reason }` → emit `DeferredCheck` Info finding; rest of checks proceed |
| Migration 0021 apply failure | startup | fatal — `cairn-cli` exits `EX_CONFIG` (78) |
| Malformed `workflows.lint` config | startup | fall back to defaults, log `warn` |

**Failure-class invariants** (debug-asserted, release-warned):

- `LeaseLost` and `Timeout` never appear in handler-supplied `HandlerOutcome`.
- `Validation` and `Poison` always force `FailDisposition::Permanent` regardless
  of handler's stated disposition.

**Metric ordering rule:** emit after the store mutation lands. If the
`JobStore` call errors, no metric line is written for that attempt.

## 5. Testing

### 5.1 Unit

- `RetryPolicy::delay_for_attempt` — existing coverage, unchanged.
- `FailureClass` JSON round-trip per variant (`insta` snapshot).
- `HandlerOutcome` class-override invariants — `rstest` over the
  `(disposition × class)` matrix; assert scheduler converts illegal combos.
- `workflow_health` check — fixture-driven, each finding kind covered
  (dead-letter present, stuck queue, stale dream, overdue expire/eval).
  Empty reader (= `None`) → zero findings.

### 5.2 Integration (`crates/cairn-workflows/tests/`)

- `crash_retry_fixture.rs` — enqueue, lease, drop worker mid-handler, restart
  scheduler, assert job resumes with `attempts++` and side effects apply
  exactly once. Real `tempfile` SQLite, not a mock.
- `dead_letter_fixture.rs` — enqueue → fail × `max_attempts` → assert
  `state = Failed`, `dead_letter_at_ms` set, `failure_class` matches last
  handler-supplied class. Lint pass over the same DB yields exactly one
  `WorkflowDeadLetter` finding with the right `job_id`.
- `metrics_emission_fixture.rs` — `CapturingMetricsSink` records Started →
  Completed for job A and Started → Failed×N → terminal for job B. Asserts
  count, order, `queue_lag_ms`, and `duration_ms` against `MockClock`.
- `startup_reap_fixture.rs` — pre-seed a `Leased` row with expired lease,
  call `Scheduler::start` with `worker_count = 0`, assert the row is `Queued`
  before any worker tick (proves reap runs at startup, not steady-state).

### 5.3 Property (`proptest`)

- `FailureClass` JSON round-trip totality over the enum.
- Scheduler state invariant: for any sequence of `(handler_outcome,
  lease_state)`, final row state ∈ `{Queued, Done, Failed}` and
  `failure_class IS NOT NULL` iff state = Failed.

### 5.4 Snapshot (`insta`)

- `workflow_health` check: one snap per finding kind under
  `crates/cairn-core/src/verbs/lint/checks/snapshots/`.
- `MetricEvent` JSON wire format: three new round-trip snapshots in
  `domain::metrics::tests` mirroring `HotPrefixAssembled` style.

### 5.5 Acceptance-criterion → test mapping

| Issue criterion | Tests |
|---|---|
| Crash does not lose job / double-apply | `crash_retry_fixture` + `startup_reap_fixture` |
| Repeated failures visible & actionable | `dead_letter_fixture` + `workflow_health::dead_letter` snapshot |
| Lint reports workflow health w/ ids | `workflow_health` unit suite + `dead_letter_fixture` lint pass |

## 6. Implementation order (for the plan that follows this spec)

1. **PR-1: contracts.** Add `FailureClass`, evolve `HandlerOutcome`, change
   `JobStore::fail` signature, evolve `LeasedJob` with `failure_class`,
   update three workflow handlers. The `cairn-store-sqlite` adapter's
   `fail()` impl is updated for the new signature but does **not** persist
   the new fields yet — it accepts the param and discards it. Builds and
   tests stay green. No SQLite migration.
2. **PR-2: SQLite migration 0021 + startup reap.** Add `failure_class`,
   `dead_letter_at_ms`, `completed_at_ms` columns + indexes. Adapter writes
   them on `fail()` / `complete()`. Make `Scheduler::start` async and call
   `reap_expired` once before worker spawn.
3. **PR-3: workflow metrics.** Add three `MetricEvent` variants + worker /
   reaper emission. Pure additive on `cairn-core`; ties into `cairn-cli`
   metrics sink wiring already in place.
4. **PR-4: lint integration.** Add `WorkflowJobsReader` trait + SQLite impl,
   extend `LintInputs` with `workflow_jobs` + `now_ms`, add
   `workflow_health.rs` check + four new `Kind` variants. Run `cairn-codegen`
   and commit generated outputs.
5. **PR-5: config schema + docs.** `workflows.lint` block, update
   `cairn docgen`, regenerate `docs/site/src/reference/generated/`.

Each PR carries its own integration tests; the spec's full test matrix
finishes landing at PR-5.

## 7. Verification

Per CLAUDE.md §8 — every PR runs:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

PR-4 also regenerates IDL outputs (`cairn-codegen` write pass), PR-5 also
regenerates docs (`cargo run -p cairn-cli --bin cairn-docgen -- --write`).

## 8. Risks and trade-offs

- **Breaking signature changes inside `cairn-core`.** `JobStore::fail` gains
  a parameter; `HandlerOutcome` variants gain a field. We're pre-1.0 and
  these traits are unstable by policy (CLAUDE.md §10), so this is acceptable
  but it's the largest blast radius of the change.
- **`completed_at_ms` is one extra column for one lint check.** Lower-cost
  alternative would be reading `consent_journal` for the latest workflow
  commit, but joins across two tables make the lint reader awkward and
  cross-table consistency unobvious. One nullable column is the right
  trade-off.
- **`Scheduler::start` becomes async.** Trivial signature change with a
  small ripple — two test fixtures and the CLI bootstrap. The startup reap
  could not stay sync without `block_on`-from-inside-a-runtime hazards.
- **Lint `now_ms` is wall-clock.** Test fixtures pass synthetic values; CLI
  passes `SystemClock`. Workflows already coordinate on epoch-ms clocks via
  `JobStore` callers, so no new clock authority is needed.

## 9. Out of scope (explicit)

- OpenTelemetry export / Grafana dashboards (v0.2 per issue body).
- Multi-process orchestration (P1 only; P0 is single binary).
- `cairn admin workflows requeue` UX for dead-letter rows (separate issue if
  demand emerges).
- Per-handler retry-policy customization. `RetryPolicy::DEFAULT` stays the
  only one; handlers don't override.
