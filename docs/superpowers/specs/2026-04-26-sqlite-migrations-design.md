# SQLite Migrations for Records, Indexes, WAL, Replay, Consent, Locks, and Jobs

**Issue:** [#45](https://github.com/windoliver/cairn/issues/45) — P0
**Parent epic:** #6 (SQLite record store with FTS5 + sqlite-vec + local embeddings)
**Design sources:** brief §3.0 (storage topology), §3 (records-in-SQLite), §5.6 (WAL)
**Date:** 2026-04-26

---

## 1. Goal

Land the schema-only DDL for `.cairn/cairn.db`: every P0 table, index, FTS5
virtual table + triggers, and view from brief §3 and §5.6, applied through a
forward-only migration runner that opens cleanly on a fresh vault and
re-opens idempotently on an up-to-date one. **Verb implementations and the
sqlite-vec extension are explicitly out of scope** — they land with the
storage implementation in #46.

This issue does *not* ship `MemoryStore` verb impls, the sqlite-vec C
extension, or any Nexus projection. It only ships the file-on-disk shape and
the open-time pragmas, plus enough Rust to apply them.

## 2. Non-goals

- Real verb behaviour (`MemoryStore` capability flags stay `false`).
- `vec0` virtual tables (deferred to #46 — extension load + Cargo dep
  changes belong with the storage impl).
- Workflow host scheduling logic (only the `workflow_jobs` schema lands).
- Nexus / hub projections.
- Markdown projector or `cairn lint --fix-markdown`.

## 3. Architecture

`cairn-store-sqlite` gains:

- A new `open()` entry point that takes a `&Path` to `.cairn/cairn.db`,
  applies pragmas, runs all pending migrations, and returns a `rusqlite::Connection`.
- A `migrations/` module exposing the embedded `Migrations` value used by
  `open()` and by tests.
- A `migrations/sql/` directory of forward-only `.sql` files split by
  concern.

The existing `SqliteMemoryStore` scaffold and plugin manifest stay as-is —
capability flags remain `false` until #46.

## 4. Components

### 4.1 Cargo additions (this crate only)

```toml
[dependencies]
rusqlite = { workspace = true, features = ["bundled"] }
rusqlite_migration = { workspace = true }
```

`rusqlite` joins `[workspace.dependencies]` with `default-features = false`
and `features = ["bundled"]`; `rusqlite_migration` joins as a new workspace
dep pinned to a 1.x range. The `package.metadata.cargo-machete` ignore for
`thiserror` stays.

### 4.2 Module layout

```
crates/cairn-store-sqlite/
  src/
    lib.rs               -- existing scaffold + re-exports `open`, `StoreError`
    open.rs              -- `pub fn open(path: &Path) -> Result<Connection, StoreError>`
    error.rs             -- `StoreError` enum (thiserror)
    migrations/
      mod.rs             -- `pub fn migrations() -> Migrations<'static>`
      sql/
        0001_records.sql      -- records, partial indexes, records_fts + triggers,
                              -- records_latest view, edges
        0002_wal.sql          -- wal_ops, wal_steps
        0003_replay.sql       -- used, issuer_seq, outstanding_challenges
        0004_locks.sql        -- locks, lock_holders, daemon_incarnation, reader_fence
        0005_consent.sql      -- consent_journal
        0006_jobs.sql         -- workflow_jobs (+ index on (state, scheduled_at))
```

### 4.3 Pragmas applied on open (before migrations)

```
PRAGMA journal_mode = WAL;
PRAGMA synchronous  = NORMAL;
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;
PRAGMA temp_store   = MEMORY;
```

`user_version` is owned by `rusqlite_migration` and advances as migrations
apply.

### 4.4 SQL contents per file

Each file ships exactly the DDL specified by the brief. Verbatim DDL is
pulled from §3 and §5.6 of `docs/design/design-brief.md`.

- **0001_records.sql** — `records` (per §3 lines ~340-365 of the brief),
  partial indexes `records_active_target_idx`, `records_path_idx`,
  `records_kind_idx`, `records_visibility_idx`, `records_scope_idx`;
  `records_fts` virtual table + `records_fts_ai` / `_ad` / `_au` triggers;
  `records_latest` view; `edges` table.
- **0002_wal.sql** — `wal_ops (operation_id PK, state, envelope JSONB, ...)`
  and `wal_steps (operation_id, step_ord, state, PK(operation_id, step_ord))`.
- **0003_replay.sql** — `used (operation_id, nonce, issuer, sequence,
  committed_at, UNIQUE(operation_id, nonce))`, `issuer_seq (issuer PK,
  high_water)`, `outstanding_challenges (issuer, challenge, expires_at,
  PK(issuer, challenge))`.
- **0004_locks.sql** — `locks` (per §5.6 lines ~1820-1830), `lock_holders`
  (per §5.6 lines ~1834-1855) with FK back to `locks`, `daemon_incarnation`
  singleton (CHECK only_one = 1), `reader_fence (scope_kind, scope_key,
  op_id, state, opened_at, PK(scope_kind, scope_key))`.
- **0005_consent.sql** — `consent_journal (row_id INTEGER PK AUTOINCREMENT,
  op_id, actor, kind, payload JSONB, committed_at)`.
- **0006_jobs.sql** — `workflow_jobs (job_id PK, kind, state, payload JSONB,
  scheduled_at, attempts, last_error, created_at, updated_at)` plus
  `CREATE INDEX workflow_jobs_due_idx ON workflow_jobs(state, scheduled_at)`.

The brief is silent on `workflow_jobs`; the schema above is a minimal
placeholder consistent with common job-queue tables (priority/retry/
scheduling) and will be extended via future append-only migrations when
`cairn-workflows` lands.

## 5. Data flow

```
caller (cli / tests)
  │
  ▼
cairn_store_sqlite::open(path: &Path) -> Result<Connection, StoreError>
  │
  ├─ rusqlite::Connection::open(path)
  ├─ apply_pragmas(&conn)              -- best-effort but errored ones surface
  ├─ migrations().to_latest(&mut conn) -- idempotent on up-to-date DB
  └─ Ok(conn)
```

Re-opening an up-to-date DB is a no-op past pragma application. Opening a
DB whose `user_version` is *higher* than `migrations().count()` returns
`StoreError::IncompatibleSchema` — forward-only is enforced.

## 6. Error handling

```rust
// crates/cairn-store-sqlite/src/error.rs
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    #[error("opening sqlite database")]
    Open(#[source] rusqlite::Error),

    #[error("applying pragma {name}")]
    Pragma { name: &'static str, #[source] source: rusqlite::Error },

    #[error("running migrations")]
    Migrate(#[from] rusqlite_migration::Error),

    #[error("schema is at user_version {found}, ahead of compiled head {expected}")]
    IncompatibleSchema { found: usize, expected: usize },
}
```

Per CLAUDE.md §6.2 the lib uses `thiserror` only; binaries map to anyhow at
their boundary. `#[non_exhaustive]` keeps additions non-breaking.

## 7. Testing

### 7.1 In-crate unit tests

In `src/migrations/mod.rs`:

- `migrations_validates` — `Migrations::validate()` succeeds.
- `each_migration_applies_to_memory` — apply on `:memory:`, advance
  `user_version` one step at a time, assert each step succeeds and
  `user_version` advances by one.

### 7.2 Integration tests (`tests/migrations.rs`)

Uses `tempfile::tempdir()` (already a workspace dev-dep via
`cairn-test-fixtures`).

- `fresh_vault_opens_to_head` — call `open()` on a tmp path; query
  `sqlite_master` and assert the full P0 set is present:
  - tables: `records`, `edges`, `wal_ops`, `wal_steps`, `used`,
    `issuer_seq`, `outstanding_challenges`, `locks`, `lock_holders`,
    `daemon_incarnation`, `reader_fence`, `consent_journal`,
    `workflow_jobs`
  - virtual tables: `records_fts`
  - views: `records_latest`
  - triggers: `records_fts_ai`, `records_fts_ad`, `records_fts_au`
  - partial indexes: `records_active_target_idx`, `records_path_idx`,
    `records_kind_idx`, `records_visibility_idx`, `records_scope_idx`,
    `workflow_jobs_due_idx`
- `pragmas_applied` — assert `PRAGMA journal_mode` returns `wal`,
  `foreign_keys` returns 1.
- `idempotent_reopen` — `open()` twice on the same path; both succeed and
  `user_version` is stable between calls.
- `partial_migration_resume` — apply migrations 1..=3 manually via the
  runner, then call `open()`; assert all six are applied.
- `forward_only_rejects_future_schema` — open a fresh DB, manually
  `PRAGMA user_version = 999`; call `open()`; assert `StoreError::IncompatibleSchema`.
- `fts_round_trip` — minimal smoke: `INSERT INTO records (...)` then
  `SELECT body FROM records_fts WHERE records_fts MATCH '...'` returns the
  row, proving the trigger wired up correctly.

### 7.3 Snapshot tests (`insta`)

A single test dumps `sqlite_master` (sorted by `type, name`) after applying
all migrations, and snapshots it. Reviewers see schema deltas in PR diffs.
Snapshot lives at `crates/cairn-store-sqlite/tests/snapshots/migrations__schema.snap`.

## 8. Verification mapping (issue's acceptance criteria)

| AC | How it's verified |
|----|--------------------|
| Fresh vault opens with all P0 tables and pragmas | `fresh_vault_opens_to_head` + `pragmas_applied` |
| Migration history is visible and fails on checksum mismatch | `rusqlite_migration` tracks `user_version`; `forward_only_rejects_future_schema` covers the mismatch direction. (`rusqlite_migration` itself rejects DDL drift between calls.) |
| No P0 authoritative state outside `.cairn/cairn.db` except rebuildable mirrors/caches | Structural — this PR adds nothing outside the SQLite file. Reviewer confirms. |
| Migration tests on empty and pre-migrated fixtures | `fresh_vault_opens_to_head` + `partial_migration_resume` |
| Inspect SQLite schema for required tables and FTS/vector indexes | Snapshot test + explicit `sqlite_master` assertions (vector indexes deferred to #46 per scope) |
| DB open/close smoke tests on macOS/Linux if CI supports both | Existing CI matrix runs on macOS + Ubuntu |

## 9. CLAUDE.md conformance

- §6.2 error handling — `thiserror` lib enum, no `anyhow` in lib.
- §6.7 deps — both new deps justified (rusqlite is the brief-mandated SQLite
  binding; `rusqlite_migration` is the chosen runner). Both join
  `[workspace.dependencies]`. `default-features = false` on rusqlite.
- §6.11 storage — migrations live in `crates/cairn-store-sqlite/src/migrations/sql/`,
  append-only, applied via `rusqlite_migration::Migrations`.
- §7 TDD — failing test (`fresh_vault_opens_to_head` against empty crate)
  written first, then migrations land file-by-file.
- §8 verification — `cargo fmt`, `cargo clippy --workspace --all-targets
  --locked -- -D warnings`, `cargo test --workspace --locked`,
  `./scripts/check-core-boundary.sh` (this PR doesn't touch `cairn-core`,
  so the boundary is untouched).

## 10. Out-of-scope follow-ups

- #46: real `MemoryStore` verb impls + sqlite-vec extension load + `0007_vector.sql`
  (vec0 virtual tables for embeddings).
- Workflow host scheduling logic — extends `workflow_jobs` schema if needed
  via a future migration.
- `cairn admin replay-wal` tooling — reads `wal_ops` / `wal_steps`; tables
  are ready, command lands later.
