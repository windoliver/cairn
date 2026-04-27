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
- **`workflow_jobs` table** — the brief is silent on its schema and the
  workflow host (`cairn-workflows`) has not yet pinned its leasing /
  retry / dedupe invariants. Forward-only migrations make a placeholder
  irreversibly persistent on every vault. This PR therefore does **not**
  ship `workflow_jobs`; it lands with the workflow host in a follow-up
  issue (filed as the PR's first follow-up). The issue #45 checklist line
  for "workflow jobs" is consciously deferred — call this out in the PR
  description.
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

Each file ships the DDL specified by the brief. The brief uses elided
column lists (`…`) for control-plane tables in §5.6; this section pins
each elision to concrete columns, types, NOT NULL flags, CHECK
constraints, and recovery-supporting indexes. Migration tests assert all
of these (see §7).

#### 0001_records.sql

Verbatim from brief §3 (lines ~340-426):

- `records` table — columns exactly as in the brief: `record_id TEXT PK`,
  `target_id TEXT NOT NULL`, `version INTEGER NOT NULL`, `path TEXT NOT NULL`,
  `kind TEXT NOT NULL`, `class TEXT NOT NULL`, `visibility TEXT NOT NULL`,
  `scope TEXT NOT NULL`, `actor_chain TEXT NOT NULL`, `body TEXT NOT NULL`,
  `body_hash TEXT NOT NULL`, `created_at INTEGER NOT NULL`,
  `updated_at INTEGER NOT NULL`, `active INTEGER NOT NULL DEFAULT 0`,
  `tombstoned INTEGER NOT NULL DEFAULT 0`, `is_static INTEGER NOT NULL DEFAULT 0`,
  `UNIQUE(target_id, version)`.
- Partial unique index `records_active_target_idx ON records(target_id) WHERE active = 1`.
- Partial indexes `records_path_idx`, `records_kind_idx`,
  `records_visibility_idx`, `records_scope_idx` —
  `WHERE active = 1 AND tombstoned = 0`.
- `records_fts` virtual table (`fts5(body, content='records',
  content_rowid='rowid', tokenize='porter unicode61')`).
- Triggers `records_fts_ai`, `records_fts_ad`, `records_fts_au` keeping
  FTS5 in sync.
- `records_latest` view: filters `active = 1 AND tombstoned = 0` and
  `NOT EXISTS` an `updates` edge pointing to the row.
- `edges` table: `src TEXT NOT NULL`, `dst TEXT NOT NULL`,
  `kind TEXT NOT NULL`, `weight REAL`, `PRIMARY KEY (src, dst, kind)`.

#### 0002_wal.sql

Brief §5.6 specifies these as the audit log + replay ledger linearization
point. Concrete DDL:

```sql
CREATE TABLE wal_ops (
  operation_id   TEXT NOT NULL PRIMARY KEY,                 -- ULID, idempotency key
  kind           TEXT NOT NULL CHECK (kind IN (              -- closed set per §5.6 envelope
                   'upsert','delete','promote','expire',
                   'forget_session','forget_record','evolve')),
  state          TEXT NOT NULL CHECK (state IN (             -- FSM states, §5.6
                   'ISSUED','PREPARED','COMMITTED','ABORTED','REJECTED')),
  envelope       TEXT NOT NULL,                             -- JSON blob (full §5.6 envelope)
  issuer         TEXT NOT NULL,
  principal      TEXT,                                      -- nullable per §5.6
  target_hash    TEXT NOT NULL,
  scope_json     TEXT NOT NULL,                             -- JSON tuple
  plan_ref       TEXT,                                      -- optional path to staged plan
  expires_at     INTEGER NOT NULL,                          -- UTC ms; recovery ignores TTL
  signature      TEXT NOT NULL,
  issued_at      INTEGER NOT NULL,
  updated_at     INTEGER NOT NULL,                          -- last state transition time
  reason         TEXT                                       -- populated for REJECTED/ABORTED
);

-- Recovery scans for non-terminal ops on boot (§5.6 step 1).
CREATE INDEX wal_ops_open_idx
  ON wal_ops(state, issued_at)
  WHERE state IN ('ISSUED','PREPARED');

-- Dependency lookup for the recovery DAG.
CREATE TABLE wal_op_deps (
  operation_id     TEXT NOT NULL,
  depends_on_op_id TEXT NOT NULL,
  PRIMARY KEY (operation_id, depends_on_op_id),
  FOREIGN KEY (operation_id)     REFERENCES wal_ops(operation_id) ON DELETE CASCADE,
  FOREIGN KEY (depends_on_op_id) REFERENCES wal_ops(operation_id)
);
CREATE INDEX wal_op_deps_reverse_idx ON wal_op_deps(depends_on_op_id);

CREATE TABLE wal_steps (
  operation_id  TEXT NOT NULL,
  step_ord      INTEGER NOT NULL CHECK (step_ord >= 0),
  step_kind     TEXT NOT NULL,                              -- e.g. snapshot.stage, primary.upsert_cow
  state         TEXT NOT NULL CHECK (state IN (
                   'PENDING','DONE','FAILED','COMPENSATED')),
  attempts      INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
  last_error    TEXT,
  pre_image     BLOB,                                       -- staged snapshot per §5.6
  started_at    INTEGER,
  finished_at   INTEGER,
  PRIMARY KEY (operation_id, step_ord),
  FOREIGN KEY (operation_id) REFERENCES wal_ops(operation_id) ON DELETE CASCADE
);

-- Recovery resumes at the lowest non-DONE step per op (§5.6 step 4).
CREATE INDEX wal_steps_resume_idx
  ON wal_steps(operation_id, state, step_ord);
```

#### 0003_replay.sql

```sql
-- Replay-attack ledger (§4.2). UNIQUE(operation_id, nonce) is the
-- collision check; lookup by issuer+sequence drives anti-rewind.
CREATE TABLE used (
  operation_id  TEXT NOT NULL,
  nonce         BLOB NOT NULL,
  issuer        TEXT NOT NULL,
  sequence      INTEGER NOT NULL CHECK (sequence >= 0),
  committed_at  INTEGER NOT NULL,
  UNIQUE (operation_id, nonce)
);
CREATE INDEX used_issuer_seq_idx ON used(issuer, sequence);

CREATE TABLE issuer_seq (
  issuer      TEXT NOT NULL PRIMARY KEY,
  high_water  INTEGER NOT NULL CHECK (high_water >= 0)
);

CREATE TABLE outstanding_challenges (
  issuer      TEXT NOT NULL,
  challenge   BLOB NOT NULL,
  expires_at  INTEGER NOT NULL,
  PRIMARY KEY (issuer, challenge)
);
CREATE INDEX outstanding_challenges_exp_idx ON outstanding_challenges(expires_at);
```

#### 0004_locks.sql

Verbatim from brief §5.6 lock-table block (lines ~1820-1865):

- `locks` — columns: `scope_kind`, `scope_key`, `mode CHECK IN
  ('shared','exclusive','free')`, `holder_count INTEGER NOT NULL
  CHECK (holder_count >= 0)`, `epoch INTEGER NOT NULL CHECK (epoch >= 0)`,
  `waiters BLOB`, `last_heartbeat_at INTEGER`,
  `PRIMARY KEY (scope_kind, scope_key)`.
- `lock_holders` — every column from the brief block (`holder_id`,
  `acquired_epoch`, `owner_incarnation`, `boot_id`, `reclaim_deadline`),
  `PRIMARY KEY (scope_kind, scope_key, holder_id)`,
  `FOREIGN KEY (scope_kind, scope_key) REFERENCES locks(scope_kind, scope_key)`.
- Index `lock_holders_reclaim_idx ON lock_holders(reclaim_deadline)` —
  GC step in the acquisition transaction filters by deadline (§5.6
  acquisition protocol, step 1).
- `daemon_incarnation` singleton — `only_one INTEGER PRIMARY KEY CHECK
  (only_one = 1)`, `incarnation TEXT NOT NULL`, `boot_id TEXT NOT NULL`,
  `started_at INTEGER NOT NULL`.
- `reader_fence (scope_kind TEXT NOT NULL, scope_key TEXT NOT NULL,
  op_id TEXT NOT NULL, state TEXT NOT NULL CHECK (state IN
  ('tombstoning','closed')), opened_at INTEGER NOT NULL,
  PRIMARY KEY (scope_kind, scope_key))`.

#### 0005_consent.sql

```sql
CREATE TABLE consent_journal (
  row_id        INTEGER PRIMARY KEY AUTOINCREMENT,
  op_id         TEXT NOT NULL,
  actor         TEXT NOT NULL,
  kind          TEXT NOT NULL,                              -- mirrors wal_ops.kind plus 'abort'
  payload       TEXT NOT NULL,                              -- JSON
  committed_at  INTEGER NOT NULL,
  FOREIGN KEY (op_id) REFERENCES wal_ops(operation_id)
);

-- The async consent_log_materializer tails this table by row_id
-- (§5.6 upsert step 7). An index on op_id supports lint cross-checks.
CREATE INDEX consent_journal_op_idx ON consent_journal(op_id);
```

> The closed-set CHECK constraints (`wal_ops.kind`, `wal_ops.state`,
> `wal_steps.state`, `locks.mode`, `reader_fence.state`) are deliberate:
> they are the structural invariants that make recovery / replay
> reasoning safe, and they prevent malformed rows even if a future verb
> impl forgets to validate.

## 5. Data flow

```
caller (cli / tests)
  │
  ▼
cairn_store_sqlite::open(path: &Path) -> Result<Connection, StoreError>
  │
  ├─ rusqlite::Connection::open(path)
  ├─ apply_pragmas(&conn)                -- errored pragmas surface as StoreError::Pragma
  ├─ migrations().to_latest(&mut conn)   -- idempotent on up-to-date DB
  ├─ verify_schema_fingerprint(&conn)?   -- same-version drift detector (see below)
  └─ Ok(conn)
```

Re-opening an up-to-date DB is a no-op past pragma application. Opening a
DB whose `user_version` is *higher* than `migrations().count()` returns
`StoreError::IncompatibleSchema` — forward-only is enforced.

### 5.1 Schema fingerprint (same-version drift detection)

`rusqlite_migration` only checks `user_version`; it does not detect a
manually-tampered schema at the same version (e.g., a dropped FTS
trigger or a missing partial index would silently leave the vault
limping along). To close that gap, `open()` runs a fingerprint check
**after** migrations apply:

```rust
fn verify_schema_fingerprint(conn: &Connection) -> Result<(), StoreError> {
    // Canonical dump: every row of sqlite_master for tables / indexes /
    // triggers / views, ordered by (type, name), `sql` column normalized
    // (whitespace-collapsed, trailing-`;` stripped). Hash with BLAKE3.
    let actual = compute_fingerprint(conn)?;
    if actual != EXPECTED_FINGERPRINT_AT_HEAD {
        return Err(StoreError::SchemaDrift {
            expected: EXPECTED_FINGERPRINT_AT_HEAD,
            actual,
        });
    }
    Ok(())
}
```

`EXPECTED_FINGERPRINT_AT_HEAD` is a `&'static str` (hex-encoded BLAKE3)
generated by a unit test that prints the canonical dump on first run and
asserts equality afterwards. Updating the constant when adding a new
migration is part of the migration's PR — same-version drift in the
field is then a pure `SchemaDrift` error and the daemon fails closed.
This is the same primitive that catches "the user manually deleted
`records_fts_au` to silence a confused error message."

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

    #[error("schema fingerprint mismatch (drift at user_version head): expected {expected}, got {actual}")]
    SchemaDrift { expected: &'static str, actual: String },
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
  - tables: `records`, `edges`, `wal_ops`, `wal_op_deps`, `wal_steps`,
    `used`, `issuer_seq`, `outstanding_challenges`, `locks`,
    `lock_holders`, `daemon_incarnation`, `reader_fence`,
    `consent_journal`
  - virtual tables: `records_fts`
  - views: `records_latest`
  - triggers: `records_fts_ai`, `records_fts_ad`, `records_fts_au`
  - partial indexes: `records_active_target_idx`, `records_path_idx`,
    `records_kind_idx`, `records_visibility_idx`, `records_scope_idx`
  - control-plane indexes: `wal_ops_open_idx`, `wal_op_deps_reverse_idx`,
    `wal_steps_resume_idx`, `used_issuer_seq_idx`,
    `outstanding_challenges_exp_idx`, `lock_holders_reclaim_idx`,
    `consent_journal_op_idx`
- `pragmas_applied` — assert `PRAGMA journal_mode` returns `wal`,
  `foreign_keys` returns 1.
- `idempotent_reopen` — `open()` twice on the same path; both succeed and
  `user_version` is stable between calls.
- `partial_migration_resume` — apply migrations 1..=3 manually via the
  runner, then call `open()`; assert all six are applied.
- `forward_only_rejects_future_schema` — open a fresh DB, manually
  `PRAGMA user_version = 999`; call `open()`; assert `StoreError::IncompatibleSchema`.
- `same_version_drift_is_rejected` — call `open()` to bring the DB to
  head; manually `DROP TRIGGER records_fts_au`; call `open()` again on
  the same path; assert `StoreError::SchemaDrift`. A second variant
  drops `records_path_idx` and asserts the same error. This is the
  drift case `user_version` alone does not catch.
- `fts_round_trip` — minimal smoke: `INSERT INTO records (...)` then
  `SELECT body FROM records_fts WHERE records_fts MATCH '...'` returns the
  row, proving the trigger wired up correctly.
- `wal_op_state_rejects_invalid` — attempt
  `INSERT INTO wal_ops (..., state='WHATEVER', ...)`; assert SQLite
  returns a CHECK constraint failure. Repeats for `wal_steps.state`,
  `locks.mode`, `reader_fence.state`. Proves the closed-set CHECKs are
  load-bearing.

### 7.3 Snapshot tests (`insta`)

A single test dumps `sqlite_master` (sorted by `type, name`) after applying
all migrations, and snapshots it. Reviewers see schema deltas in PR diffs.
Snapshot lives at `crates/cairn-store-sqlite/tests/snapshots/migrations__schema.snap`.

## 8. Verification mapping (issue's acceptance criteria)

| AC | How it's verified |
|----|--------------------|
| Fresh vault opens with all P0 tables and pragmas | `fresh_vault_opens_to_head` + `pragmas_applied` |
| Migration history is visible and fails on checksum mismatch | `rusqlite_migration` tracks `user_version`; `forward_only_rejects_future_schema` covers future-version skew; `same_version_drift_is_rejected` covers the same-version tampered-schema case (closes the dropped-trigger / dropped-index gap that `user_version` alone misses). |
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
- `workflow_jobs` table + indexes — lands with the workflow host once
  its leasing / retry / dedupe invariants are pinned in the brief.
- `cairn admin replay-wal` tooling — reads `wal_ops` / `wal_steps`; tables
  are ready, command lands later.
