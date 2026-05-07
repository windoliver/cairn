# Issue #56 — Single-Writer Locks, Compatibility, and Fencing

**Status:** approved (brainstorming)
**Date:** 2026-05-06
**Issue:** [#56](https://github.com/windoliver/cairn/issues/56)
**Parent epic:** [#8](https://github.com/windoliver/cairn/issues/8) — WAL, locks, replay, record-level forget
**Depends on:** #55 (WAL state machine + boot recovery) — merged as `f8f1cd6b`
**Brief sources:** §5.6 (Lock compatibility), §3.0 (Atomicity model)

---

## 1. Problem

Concurrent writers, in-flight readers, and crashed/stale lease holders must serialize against the single SQLite store without deadlock and without letting a stale writer commit after losing ownership. Brief §5.6 specifies a SQLite lock-table protocol with epoch fencing, per-holder lease state, daemon-incarnation reclaim, and a reader-fence drain. Today the repo ships a placeholder subset that satisfies `lint --fix-markdown` but does not deliver the brief's correctness story.

### Current state (after #55)

- `0004_locks.sql` migration — `locks`, `lock_holders`, `reader_fence`, `daemon_incarnation` tables; compatibility triggers (`EXCLUSIVE` alone, `SHARED` blocked by `EXCLUSIVE`); identity-immutable triggers; reclaim-on-DELETE trigger.
- `crates/cairn-store-sqlite/src/locks.rs` — `acquire_exclusive`, `LockHandle`, best-effort `is_still_held`, TTL reclaim on acquisition. Single caller: `cairn-cli/src/verbs/lint.rs`.

### Deltas vs brief §5.6

| Brief requirement | Current | This PR |
|---|---|---|
| `locks.epoch` per-resource counter | absent | added |
| `lock_holders.acquired_epoch`, `owner_incarnation` | absent | added |
| `daemon_incarnation` wired into open + GC | table exists, not wired | wired |
| Per-holder fencing CAS used by writers | best-effort `is_still_held` only | added (`fencing_cas` on `LockHandle`) |
| `acquire_shared` API | absent | added |
| Typed lock kinds (Entity / Session / Vault × Shared / Exclusive) | string `scope_kind`/`scope_key` | added |
| `LockError` carries owner / operation / TTL / retry | only resource | added |
| Reader-fence register / clear / drain helpers | absent (table only) | added |
| BOOTTIME-ns lease clock | wall-clock | **deferred** |
| `boot_id` column | absent | **deferred** |
| `locks.waiters` queue | absent | **deferred** |

Acceptance criteria (issue body):
- [x] SQLite-backed lock tables and compatibility rules for read, write, reindex, forget, and workflow jobs — typed mapping documented; tables in place.
- [x] Fencing tokens to prevent stale writers from applying after losing ownership — epoch + per-holder fencing CAS.
- [x] Readers continue under SQLite WAL mode while writes serialize safely — read path takes no lock; SQLite WAL mode provides reader concurrency.
- [x] Concurrent writes serialize without deadlock — covered by `locks_concurrent_writers.rs`.
- [x] Stale fenced writer cannot commit authoritative changes — covered by `locks_stale_fence.rs`.
- [x] Lock errors include owner, operation, timeout, and recommended retry behavior — `LockError::Held` + `RetryHint`.

---

## 2. Module organization

```
crates/cairn-store-sqlite/src/locks/
├── mod.rs          ← re-exports; public surface
├── handle.rs       ← LockHandle (epoch-aware), FencingGuard
├── acquire.rs      ← acquire(scope, mode, holder_id, ttl); fencing CAS
├── error.rs        ← LockError + RetryHint
├── kinds.rs        ← LockScope, LockMode, ResourceKey
├── fence.rs        ← register_pending, clear, wait_for_drain
└── incarnation.rs  ← init_incarnation, daemon-startup recovery
```

Existing `src/locks.rs` is split into the directory above. Public re-exports preserve external visibility from `cairn_store_sqlite::locks::*`.

The deprecated thin wrapper `acquire_exclusive(scope_kind, scope_key, holder_id, ttl)` stays one cycle for the single in-tree caller (`lint --fix-markdown`); marked `#[deprecated]` with a migration note pointing at `acquire(ResourceKey::vault(...), LockMode::Exclusive, ...)`.

---

## 3. Schema — migration `0050_locks_v2.sql`

Additive only, per CLAUDE.md §6.11 ("never mutate a committed migration"):

```sql
-- Migration 0050: epoch-fencing + incarnation columns (brief §5.6).

ALTER TABLE locks
  ADD COLUMN epoch INTEGER NOT NULL DEFAULT 0;

ALTER TABLE lock_holders
  ADD COLUMN acquired_epoch    INTEGER NOT NULL DEFAULT 0;
-- Sentinel '__pre_v2__' marks rows backfilled by this migration.
-- init_incarnation deletes any row whose owner_incarnation does not match
-- the current daemon's ULID, so the sentinel rows are GC'd on first open.
ALTER TABLE lock_holders
  ADD COLUMN owner_incarnation TEXT    NOT NULL DEFAULT '__pre_v2__';

-- View: how many reader fences are still PENDING per resource.
-- Used by wait_for_drain to poll progress without scanning the full table.
CREATE VIEW reader_fence_pending_count AS
  SELECT resource, COUNT(*) AS pending
    FROM reader_fence
   WHERE state = 'PENDING'
   GROUP BY resource;

INSERT INTO schema_migrations (migration_id, name, sql_blake3, applied_at)
  VALUES (50, '0050_locks_v2', '', strftime('%s','now') * 1000);
```

Backfill rationale: any `lock_holders` row surviving migration is from the pre-epoch protocol and cannot be reasoned about under the new fencing rules. `acquired_epoch DEFAULT 0` parks them at the lowest epoch; `owner_incarnation DEFAULT '__pre_v2__'` tags them with a non-ULID sentinel that `init_incarnation` (§8) deletes on first open. `verify.rs` static schema list gains the new column names and the view.

---

## 4. Lock taxonomy

```rust
// kinds.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LockScope { Entity, Session, Vault }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LockMode { Shared, Exclusive }

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResourceKey {
    pub scope: LockScope,
    pub key:   String,   // e.g. "t1:default:rec_123" for Entity
}

impl ResourceKey {
    pub fn entity(tenant: &str, workspace: &str, entity_id: &str) -> Self;
    pub fn session(tenant: &str, workspace: &str, session_id: &str) -> Self;
    pub fn vault(tenant: &str, workspace: &str) -> Self;

    /// Stable serialization stored in `lock_holders.resource`.
    pub fn as_resource_str(&self) -> String;
}
```

Verb-to-lock mapping (documentation; **wiring deferred to follow-up PRs** per scope decision):

| Operation | Locks |
|---|---|
| `search`, `retrieve` (read path) | none — SQLite WAL mode handles reader concurrency |
| `ingest`, `summarize`, `assemble_hot` (writes) | `(Entity, Exclusive)` per affected entity, plus `(Session, Shared)` if the op carries a `session_id` |
| `forget --record` | `(Entity, Exclusive)` |
| `forget --session` | `(Session, Exclusive)` for full Phase A |
| `reindex` | `(Vault, Exclusive)` |
| workflow jobs | `(Vault, Shared)` for compatible jobs; `(Vault, Exclusive)` for migrations |

The mapping table also lives in `mod.rs` doc comments so future verb authors find it without rereading the brief.

---

## 5. Acquisition + fencing CAS

### `acquire(...)`

```rust
pub async fn acquire(
    conn: &Arc<Connection>,
    resource: &ResourceKey,
    mode: LockMode,
    holder_id: &str,
    ttl: Duration,
) -> Result<LockHandle, LockError>;
```

Single SQLite transaction (`BEGIN IMMEDIATE`):

1. **GC stale holders** — `DELETE FROM lock_holders WHERE resource = ?1 AND (expires_at <= :now OR owner_incarnation != :current_incarnation)`.
2. **Recompute live holders** — `SELECT mode, epoch, COUNT(holder_id) FROM locks LEFT JOIN lock_holders USING (resource) WHERE resource = ?1`.
3. **Decide:**
   - No row yet → `INSERT INTO locks(resource, mode, holder_count, epoch=1, updated_at)` + insert holder with `acquired_epoch=1`.
   - `live=0` (all GC'd) → `UPDATE locks SET epoch = epoch + 1, mode = :wanted, holder_count = 1` + insert holder with new epoch. **Epoch bump = reclaim event.**
   - `live>0 AND mode == :wanted AND :wanted = Shared` → insert holder; epoch unchanged.
   - `live>0 AND mode != :wanted` (incompatible) → `LockError::Held { … RetryHint::BackoffJitter { 50ms, 5s } }`.
4. Cache `(holder_id, acquired_epoch, owner_incarnation)` in returned `LockHandle`.

The existing 0004 triggers (`lock_holders_exclusive_only_alone`, `lock_holders_shared_blocked_by_exclusive`) keep enforcing compatibility at the row level — the Rust decision logic and the trigger are **defense in depth**, not redundant: a future direct-INSERT call site cannot bypass the schema rules.

### `with_fencing` — fenced write transactions

A standalone CAS in its own transaction would race the next reclaim — by the time the caller opens its write transaction, the epoch could have advanced again. Per brief §5.6 ("Per-holder fencing — each holder caches its own (holder_id, acquired_epoch). The Rust core caches this pair at acquisition and re-asserts it on every chunk … BEGIN IMMEDIATE; -- Fencing CAS …"), the CAS must execute **inside the same `BEGIN IMMEDIATE`** as the side-effects.

API exposed on `LockHandle`:

```rust
impl LockHandle {
    /// Opens a BEGIN IMMEDIATE transaction, runs the fencing CAS as its first
    /// statement, then invokes `f(&mut Transaction)` if the CAS passes.
    /// COMMIT happens after `f` returns Ok; any error rolls back.
    ///
    /// CAS check (single SELECT inside the txn):
    ///   SELECT
    ///     (SELECT epoch FROM locks WHERE resource = ?1) AS group_epoch,
    ///     EXISTS (SELECT 1 FROM lock_holders
    ///              WHERE resource = ?1 AND holder_id = ?2
    ///                AND acquired_epoch = ?3
    ///                AND owner_incarnation = ?4) AS holder_alive;
    /// Both conditions must hold; otherwise rollback + LockError::Fenced.
    pub async fn with_fencing<F, R>(
        &self,
        f: F,
    ) -> Result<R, LockError>
    where
        F: for<'tx> FnOnce(&mut rusqlite::Transaction<'tx>) -> Result<R, LockError> + Send + 'static,
        R: Send + 'static;
}
```

Stale handle (epoch advanced by reclaim, or daemon restarted) → rollback + `LockError::Fenced { resource, expected_epoch, observed_epoch, retry: RetryHint::BackoffJitter { … } }`. The closure body runs *only* if the CAS passed, and any rollback path (closure returns `Err`, panic, dropped early) leaves no partial side-effects.

WAL `StepRunner` invokes `lock.with_fencing(|tx| { /* upsert records / fts / wal_steps / consent_journal */ })` per brief §5.6 transition table — the "side-effects + COMMIT marker in one txn" rule.

A separate read-only `LockHandle::is_still_held()` is retained as a best-effort diagnostic for tests and admin tooling; it does NOT replace `with_fencing` on any write path.

---

## 6. Reader-fence API

```rust
// fence.rs
pub async fn register_pending(
    conn: &Arc<Connection>,
    resource: &ResourceKey,
    operation_id: &OperationId,
) -> Result<(), LockError>;

pub async fn clear(
    conn: &Arc<Connection>,
    resource: &ResourceKey,
    operation_id: &OperationId,
) -> Result<(), LockError>;

/// Polls reader_fence_pending_count until 0, or `timeout` elapses.
/// Used by forget / reindex commit paths (follow-up PRs) to drain readers.
pub async fn wait_for_drain(
    conn: &Arc<Connection>,
    resource: &ResourceKey,
    timeout: Duration,
) -> Result<(), LockError>;
```

`clear` issues `UPDATE reader_fence SET state = 'CLEARED', cleared_at = :now WHERE …` — relies on the existing `reader_fence_state_transition` trigger. Direct-DELETE remains forbidden until the linked WAL op terminates (`reader_fence_no_direct_delete` trigger from 0004).

**Wiring deferred:** no call sites use these helpers in this PR. Follow-up PRs (#57+ forget, #50+ reindex) will add `register_pending` before tombstoning and `wait_for_drain` before commit.

---

## 7. Errors

```rust
// error.rs
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LockError {
    #[error("lock held: resource={resource} mode={mode:?} held_by={current_holder} {since_ms}ms ago, ttl_remaining={ttl_remaining:?}, retry={retry:?}")]
    Held {
        resource: String,
        mode: LockMode,
        operation: String,        // caller-supplied verb name
        current_holder: String,   // holder_id of incumbent
        ttl_remaining: Duration,  // from incumbent.expires_at - now
        since_ms: i64,            // ms since incumbent acquired
        retry: RetryHint,
    },

    #[error("fencing CAS failed: resource={resource} expected_epoch={expected_epoch} observed={observed_epoch} — holder reclaimed")]
    Fenced {
        resource: String,
        expected_epoch: i64,
        observed_epoch: i64,
        retry: RetryHint,
    },

    #[error("draining timeout: {pending} reader-fence rows still PENDING after {waited:?}")]
    DrainTimeout {
        resource: String,
        pending: i64,
        waited: Duration,
        retry: RetryHint,
    },

    #[error("lock db error")]
    Db(#[source] tokio_rusqlite::Error),

    #[error("system clock pre-epoch")]
    Clock,

    #[error("daemon incarnation not initialized")]
    NoIncarnation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RetryHint {
    /// Caller should retry with exponential backoff between `initial` and `max`.
    BackoffJitter { initial: Duration, max: Duration },
    /// Caller should call `wait_for_drain` on the resource.
    WaitForDrain  { resource: String, suggested_timeout: Duration },
    /// Terminal: no retry will succeed (e.g. validation failure).
    NoRetry,
}
```

Existing `lint --fix-markdown` call site (`crates/cairn-cli/src/verbs/lint.rs:104`) updated to construct `Held` with `operation = "lint --fix-markdown"`.

Default retry hints:
- `Held` → `BackoffJitter { initial: 50ms, max: 5s }`
- `Fenced` → `BackoffJitter { initial: 50ms, max: 5s }` (fencing failure is transient — re-acquire and retry the WAL op)
- `DrainTimeout` → `WaitForDrain { resource, suggested_timeout: 30s }`

CLI/MCP layers can render `RetryHint` to user-facing prose; programmatic callers branch on the variant.

---

## 8. Daemon incarnation

```rust
// incarnation.rs
/// Called once during Store::open, after migrations.
/// Mints a fresh ULID, INSERT OR REPLACE into the daemon_incarnation singleton,
/// then DELETE FROM lock_holders WHERE owner_incarnation != :new
/// (this drops both prior-incarnation rows AND the '__pre_v2__' sentinel
/// rows backfilled by migration 0050), and bumps locks.epoch on every
/// affected resource so any in-flight CAS from a prior incarnation fails closed.
pub async fn init_incarnation(conn: &Arc<Connection>) -> Result<String, LockError>;
```

`Store` (in `open.rs`) gains a single `init_incarnation` call after `run_migrations`. The returned ULID is stored as `Arc<str>` on `Store` and threaded into every `acquire(...)` via a new `Store::lock_context()` helper.

Recovery is bounded — single SQLite transaction over the entire `lock_holders` table; in practice this is dozens of rows at most, far below any latency budget.

---

## 9. Tests

Three new integration test files under `crates/cairn-store-sqlite/tests/`. All use `tempfile::tempdir()` and a real SQLite store — no mocks (CLAUDE.md §6.4: "No mocking the DB").

### 9.1 `locks_concurrent_writers.rs`

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_exclusive_writers_serialize_without_deadlock() {
    // 1. Open store in tempdir.
    // 2. Spawn N=8 tasks, each in a loop K=10 times: acquire(Entity::random, Exclusive, ttl=2s)
    //    → write a sentinel row → release.
    // 3. JoinSet awaits all; total wall time bounded < 60s (no permanent deadlock).
    // 4. Sentinel rows show monotonic acquired_at per resource.
    // 5. Mid-run sampler thread asserts holder_count <= 1 for every Exclusive resource.
}
```

### 9.2 `locks_stale_fence.rs`

```rust
#[tokio::test]
async fn stale_writer_blocked_by_fencing_cas() {
    // 1. Holder A acquires (Entity, Exclusive, ttl=200ms), caches handle.
    // 2. tokio::time::sleep(500ms) past TTL.
    // 3. Holder B acquires same resource — succeeds (reclaim path; epoch bumps).
    // 4. A.fencing_cas() → expects LockError::Fenced { observed_epoch > expected_epoch }.
    // 5. A attempts `BEGIN IMMEDIATE; <consume FencingGuard>; UPDATE records …; COMMIT;`
    //    via real WAL StepRunner shape → expects rollback (no record row written).
    // 6. B.fencing_cas() succeeds; B's write commits.
}

#[tokio::test]
async fn incarnation_restart_invalidates_prior_holders() {
    // Open store, acquire holder, drop store (simulating daemon restart),
    // reopen → init_incarnation runs GC → prior holder's row gone, epoch bumped.
}
```

### 9.3 `locks_compatibility_matrix.rs`

```rust
#[rstest]
#[case(None,                Shared,    Outcome::Ok)]
#[case(None,                Exclusive, Outcome::Ok)]
#[case(Some(Shared),        Shared,    Outcome::Ok)]
#[case(Some(Shared),        Exclusive, Outcome::Held)]
#[case(Some(Exclusive),     Shared,    Outcome::Held)]
#[case(Some(Exclusive),     Exclusive, Outcome::Held)]
#[tokio::test]
async fn compatibility_matrix(#[case] incumbent: Option<LockMode>,
                              #[case] requested: LockMode,
                              #[case] expected: Outcome) { … }

#[tokio::test]
async fn write_with_session_takes_entity_excl_and_session_shared() {
    // Writer A acquires (Session::s1, Shared) — represents an in-flight write
    // carrying session_id=s1; the read path does not take session locks.
    // Writer B acquires (Entity::e1, Exclusive) + (Session::s1, Shared) → both succeed
    //   (two writers can hold the same session in Shared while touching different entities).
    // Concurrent forget_session attempt for s1: (Session::s1, Exclusive) → Held.
}
```

Plus unit tests in `error.rs` for `Display` text, and one `insta` snapshot for a `LockError::Held` rendering to lock the user-facing format.

---

## 10. Files touched

**New (10):**
- `crates/cairn-store-sqlite/src/migrations/sql/0050_locks_v2.sql`
- `crates/cairn-store-sqlite/src/locks/mod.rs`
- `crates/cairn-store-sqlite/src/locks/handle.rs`
- `crates/cairn-store-sqlite/src/locks/acquire.rs`
- `crates/cairn-store-sqlite/src/locks/error.rs`
- `crates/cairn-store-sqlite/src/locks/kinds.rs`
- `crates/cairn-store-sqlite/src/locks/fence.rs`
- `crates/cairn-store-sqlite/src/locks/incarnation.rs`
- `crates/cairn-store-sqlite/tests/locks_concurrent_writers.rs`
- `crates/cairn-store-sqlite/tests/locks_stale_fence.rs`
- `crates/cairn-store-sqlite/tests/locks_compatibility_matrix.rs`

**Removed:**
- `crates/cairn-store-sqlite/src/locks.rs` (split into the directory above)

**Modified:**
- `crates/cairn-store-sqlite/src/lib.rs` — re-export update
- `crates/cairn-store-sqlite/src/open.rs` — `init_incarnation` call after migrations; expose `Store::lock_context()`
- `crates/cairn-store-sqlite/src/verify.rs` — add `epoch`, `acquired_epoch`, `owner_incarnation` columns + `reader_fence_pending_count` view to expected schema list
- `crates/cairn-cli/src/verbs/lint.rs` — switch to typed `acquire(ResourceKey::vault(...), LockMode::Exclusive, …)`; populate new `LockError::Held` fields

**Estimate:** ~900 LOC source + ~400 LOC tests.

---

## 11. Deferred (with brief citations)

These items appear in brief §5.6 but are deliberately out of scope for this PR. Each becomes a follow-up issue.

- **BOOTTIME-ns lease clock** (brief §5.6 "Durable lease clock" paragraph). Wall-clock `expires_at` retained; acceptable at P0 single-process. Revisit when P1+ supervisor lands and zombie processes become reachable.
- **`boot_id` column on `lock_holders`** (same paragraph). Single-host P0 has no cross-reboot lease; daemon-incarnation bump on every open is sufficient.
- **Waiters queue (`locks.waiters` BLOB)** (brief §5.6 acquisition step 3d). Callers retry per `RetryHint::BackoffJitter`. Brief explicitly admits this as a P0 simplification.
- **Reader-fence wiring into forget / reindex commit paths** — separate issues (#57+ forget; #50+ reindex). This PR exposes the API only.
- **Mode conversion (shared → exclusive upgrade)** — no current caller; defer until a verb requests it.
- **Boot-recovery transaction for `daemon_incarnation`** as defined in brief §5.6 lines 1875–1892 (full multi-statement form with `lock_holders_orphaned` materialized DELETE→UPDATE). The simplified form in §8 above is correct under P0 single-process assumptions; the multi-statement form is required only for P1+ supervisor concurrency.

If any of the deferred items proves load-bearing during implementation, the PR will pause and update both this spec and the brief; per CLAUDE.md §11, "Updating the brief is a legitimate outcome of an implementation PR."

---

## 12. Verification (CLAUDE.md §8)

Standard checklist applies. Specific items:

- `cargo nextest run -p cairn-store-sqlite --locked --no-fail-fast` — all three new test files pass.
- `cargo nextest run -p cairn-cli --locked` — `lint --fix-markdown` continues to acquire and release correctly with the new typed API.
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — no new lint exceptions added; `LockError` derives required by `thiserror` only.
- `./scripts/check-core-boundary.sh` — unchanged; locks live in `cairn-store-sqlite`, not core.
- `cargo run -p cairn-cli --bin cairn-docgen -- --check` — no CLI flag changes; should pass without a docgen rerun.

PR description must cite brief §5.6 and §3.0, list invariants touched (specifically: WAL two-phase apply now actually fences against stale holders), and paste verification output.
