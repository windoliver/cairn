# SQLite Migrations for Records, Indexes, WAL, Replay, Consent, and Locks

> Note on title: issue #45 reads "Records, Indexes, WAL, Replay,
> Consent, Locks, and Jobs". This spec drops "and Jobs" from the
> title because `workflow_jobs` is intentionally deferred (see §2 +
> §10). The follow-up issue that lands `workflow_jobs` will be filed
> with the PR.

**Issue:** [#45](https://github.com/windoliver/cairn/issues/45) — P0
**Parent epic:** #6 (SQLite record store with FTS5 + sqlite-vec + local embeddings)
**Design sources:** brief §3.0 (storage topology), §3 (records-in-SQLite), §5.6 (WAL)
**Date:** 2026-04-26

> **Scope of this PR.** This is a design / spec PR only. The
> implementation (Cargo deps, `migrations/sql/*.sql` files,
> `open()`, `StoreError`, integration tests) lands in a follow-up
> PR driven by the writing-plans skill that fires after this spec
> is approved. The branch `feat/sqlite-migrations` is intentionally
> docs-only at this point. Reviewers should evaluate the spec for
> design correctness; "no migration code is shipped here" is
> expected and not a finding.

---

## 1. Goal

Land the schema-only DDL for `.cairn/cairn.db` covering the
**records, WAL, replay, consent, and locks** surface specified in
brief §3 and §5.6 — five of the six checklist items in issue #45.
The sixth item (`workflow_jobs`) is **scoped to a separate sibling
P0 issue** (filed before this PR merges) so each issue's
forward-only DDL can be grounded in pinned-down brief sections.

The result of merging this issue: a vault has every P0 control-plane
table the brief has specified DDL for. The `workflow_jobs` table
joins the head schema in the immediately-following sibling issue,
which lands `0006_jobs.sql` once the workflow host's leasing /
retry / dedupe invariants are specified.

> **Why split?** Brief §3 + §5.6 give exact DDL for records, WAL,
> replay, consent, and locks. The brief is silent on `workflow_jobs`.
> Forward-only migrations make a placeholder schema irreversibly
> persistent on every vault; the cost of guessing is permanent, the
> cost of splitting the issue is one extra PR. The split is the
> conservative choice.

This issue does *not* ship `MemoryStore` verb impls, the sqlite-vec C
extension, or any Nexus projection.

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

### 4.3 Pragmas applied on open

Split into two groups by whether the pragma mutates persistent
on-disk state. The split lets `open()` validate compatibility
**before** any persistent change (§5 flow).

**Read-only pragmas (applied first, before validation):**

```
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;
PRAGMA temp_store   = MEMORY;
```

Connection-only; no writes to the DB.

**Persistent pragmas (applied after validation, before migrations):**

```
PRAGMA journal_mode = WAL;
PRAGMA synchronous  = NORMAL;
```

`journal_mode` lives in the database header and persists across
opens; setting it on a damaged vault could leave `-wal`/`-shm`
files behind. Deferring this pragma until after
`verify_migration_history` + `verify_schema_integrity_at` keeps the
failure boundary clean.

`user_version` is owned by `rusqlite_migration` and advances as
migrations apply.

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
- `records_latest` view: matches the brief's definition exactly —
  `active = 1`, `tombstoned = 0`, and `NOT EXISTS` any `updates` edge
  whose `dst = record_id`:

  ```sql
  CREATE VIEW records_latest AS
    SELECT r.*
      FROM records r
     WHERE r.active = 1
       AND r.tombstoned = 0
       AND NOT EXISTS (
         SELECT 1 FROM edges e
          WHERE e.kind = 'updates' AND e.dst = r.record_id
       );
  ```

  **Supersession is durable**, not revivable: once an `updates` edge
  exists pointing at a record, that record is permanently excluded
  from `records_latest`. This is the brief's intended semantics
  (§3 ~line 418) — the alternative (re-emerging when the supersessor
  dies) creates a stale-read hazard where downstream readers observe
  conflicting "latest" facts depending on later lifecycle changes.

  The dead-source-hides-live-dst concern from earlier review rounds
  is closed at the schema level via the
  `edges_drop_updates_when_src_tombstoned` trigger:

  ```sql
  CREATE TRIGGER edges_drop_updates_when_src_tombstoned
    AFTER UPDATE OF tombstoned ON records
    FOR EACH ROW
    WHEN NEW.tombstoned = 1 AND OLD.tombstoned = 0
  BEGIN
    DELETE FROM edges
      WHERE src = NEW.record_id
        AND kind = 'updates';
  END;
  ```

  When the §5.6 forget pipeline sets `tombstoned = 1` on a source
  row, this trigger atomically removes its outgoing `updates` edges
  in the same SQLite transaction. A crash between the tombstone
  write and the §5.6 Phase B `edges.drain` step is no longer
  observable — the edge is gone before any reader could see the
  inconsistent state. The trigger is keyed on the
  `OLD.tombstoned = 0 → NEW.tombstoned = 1` transition specifically
  so it does not refire on idempotent retries. The trigger only
  removes the *src*'s outgoing `updates` edges; cleanup of edges
  pointing at a tombstoned dst is unnecessary because dst remains
  filtered out of `records_latest` by `tombstoned = 0` regardless.
- `edges` table: columns `src TEXT NOT NULL`, `dst TEXT NOT NULL`,
  `kind TEXT NOT NULL`, `weight REAL`, `PRIMARY KEY (src, dst, kind)`,
  plus referential integrity that the brief leaves implicit:
  - `FOREIGN KEY (src) REFERENCES records(record_id) DEFERRABLE INITIALLY DEFERRED`
  - `FOREIGN KEY (dst) REFERENCES records(record_id) DEFERRABLE INITIALLY DEFERRED`

  Both FKs are deferred so that an `upsert`'s atomic
  `BEGIN IMMEDIATE … COMMIT` can insert the `version=N+1` records row
  alongside its non-`updates` edges (links, backlinks, requires,
  provides, extends, derives) in either order — those edges only need
  the FK to resolve by commit. **`updates` edges are the exception:**
  the supersede invariant must check src/dst liveness at edge-INSERT
  time (see triggers below), so the storage impl must INSERT the
  records row before any `updates` edge that references it. That is a
  caller-side contract, not a deferred-FK guarantee.

  Without these FKs, a stray `('updates', some_record_id,
  some_other_record_id)` row could be inserted with no real successor
  and silently hide the target from `records_latest` — that is a
  read-corruption hole the schema must close, not a caller-side
  concern.

- `edges` integrity triggers — three triggers cover insert, update,
  and the bypass-via-mutation case:

  - `edges_updates_supersede_insert` —
    `BEFORE INSERT ON edges WHEN NEW.kind = 'updates'` raises `ABORT`
    unless **both** `NEW.src` and `NEW.dst` exist in `records` with
    `tombstoned = 0`, and the src row's `target_id` differs from the
    dst row's `target_id` (an `updates` edge is fact-supersession
    across distinct target_ids per brief §3 line ~409). At
    creation time both rows must be present and not yet tombstoned;
    the src does not need to be `active = 1` because supersession is
    durable — once written, the edge keeps excluding `dst` from
    `records_latest` regardless of subsequent src lifecycle. Because
    this check runs at INSERT time (SQLite has no deferred triggers),
    callers must INSERT the new records row before any `updates`
    edge that references it.
  - `edges_updates_supersede_update` — same predicate, but
    `BEFORE UPDATE ON edges WHEN NEW.kind = 'updates'`. Closes the
    `UPDATE edges SET kind = 'updates' WHERE …` bypass.
  - `edges_updates_immutable_after_insert` —
    `BEFORE UPDATE ON edges WHEN OLD.kind = 'updates'` raises `ABORT`
    if any of `src`, `dst`, or `kind` change. An existing `updates`
    edge cannot be mutated to point elsewhere or downgraded to a
    different kind; it must be `DELETE`d first (which the storage impl
    does only via the §5.6 forget pipeline). Combined with the FK on
    `src`/`dst`, this makes the edge's identity immutable for the
    lifetime of the row.

  These three triggers together close the read-corruption path: no
  combination of `INSERT`, `UPDATE`, or kind-flip on `edges` can
  conjure a stray `updates` row that hides a live record from
  `records_latest`.

- This is a deviation from the brief, called out for review: the brief
  shows `edges` with no FK declarations. The deviation strengthens
  invariants without changing semantics; flag in PR. Revert the FKs +
  trigger if the brief intentionally permits dangling edges (e.g., for
  bulk import staging).

#### 0002_wal.sql

Brief §5.6 specifies these as the audit log + replay ledger linearization
point. Concrete DDL:

```sql
CREATE TABLE wal_ops (
  operation_id   TEXT NOT NULL PRIMARY KEY,                 -- ULID, idempotency key
  issued_seq     INTEGER NOT NULL UNIQUE,                   -- AUTOINCREMENT-like monotonic order;
                                                            -- the authoritative happens-before for the
                                                            -- wal_op_deps DAG. Strictly increasing on
                                                            -- every INSERT; never reused or reordered.
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
-- issued_seq is filled by the storage impl as
--   COALESCE((SELECT MAX(issued_seq) FROM wal_ops), 0) + 1
-- inside the same BEGIN IMMEDIATE that inserts the row. AUTOINCREMENT is
-- INTEGER PRIMARY KEY-only in SQLite, so we hand-roll the monotonic
-- counter; UNIQUE NOT NULL prevents duplicates and a BEFORE INSERT
-- trigger rejects values that don't strictly advance MAX().
CREATE TRIGGER wal_ops_issued_seq_must_advance
  BEFORE INSERT ON wal_ops
  FOR EACH ROW
  WHEN NEW.issued_seq <= COALESCE((SELECT MAX(issued_seq) FROM wal_ops), 0)
BEGIN
  SELECT RAISE(ABORT,
    'wal_ops.issued_seq must strictly advance MAX(issued_seq)');
END;

-- Once an op enters a terminal state (COMMITTED, ABORTED, REJECTED)
-- every column is frozen forever — including updated_at and reason
-- — so the audit ledger cannot be retroactively rewritten. The
-- envelope-immutable trigger above already pins identity columns;
-- this trigger pins the FSM execution columns once we hit terminal.
CREATE TRIGGER wal_ops_terminal_immutable
  BEFORE UPDATE ON wal_ops
  FOR EACH ROW
  WHEN OLD.state IN ('COMMITTED', 'ABORTED', 'REJECTED')
BEGIN
  SELECT RAISE(ABORT,
    'wal_ops rows in terminal states (COMMITTED/ABORTED/REJECTED) are fully immutable');
END;

-- FSM enforcement: state transitions must follow the §5.6 graph.
-- Allowed edges:
--   ISSUED   -> PREPARED | REJECTED
--   PREPARED -> COMMITTED | ABORTED
--   COMMITTED, ABORTED, REJECTED -> (terminal, no outgoing edges)
-- The CHECK on the column already restricts state to the closed
-- enum; this trigger restricts allowed transitions between values.
CREATE TRIGGER wal_ops_state_transition
  BEFORE UPDATE OF state ON wal_ops
  FOR EACH ROW
  WHEN NEW.state IS NOT OLD.state
   AND NOT (
        (OLD.state = 'ISSUED'   AND NEW.state IN ('PREPARED','REJECTED'))
     OR (OLD.state = 'PREPARED' AND NEW.state IN ('COMMITTED','ABORTED'))
   )
BEGIN
  SELECT RAISE(ABORT,
    'wal_ops.state transition not allowed by §5.6 FSM');
END;

-- Treat the issued envelope as append-only. Every column that
-- describes WHAT operation was authorized — identity, ordering, and
-- the signed envelope material — is frozen post-insert. Only the FSM
-- execution columns mutate as the op progresses through
-- ISSUED → PREPARED → COMMITTED / ABORTED / REJECTED:
--   * state         — driven by the FSM
--   * updated_at    — bumped on every state transition
--   * reason        — populated for REJECTED / ABORTED
-- All other columns (operation_id, issued_seq, kind, envelope, issuer,
-- principal, target_hash, scope_json, plan_ref, expires_at, signature,
-- issued_at) are immutable. This closes "operator UPDATEs target_hash
-- mid-op" and similar control-plane integrity holes.
-- The WAL is the authoritative linearization + audit ledger. Block
-- DELETE so terminal-state cleanup cannot erase recovery / replay
-- history. Cascading deletes (wal_steps and wal_op_deps both have
-- ON DELETE CASCADE on operation_id) make a single DELETE
-- catastrophic without this guard. Archival, when needed, must move
-- rows to a separate audit table with its own retention policy, not
-- erase the ledger.
CREATE TRIGGER wal_ops_no_delete
  BEFORE DELETE ON wal_ops
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'wal_ops is append-only; DELETE not permitted');
END;

CREATE TRIGGER wal_ops_envelope_immutable
  BEFORE UPDATE ON wal_ops
  FOR EACH ROW
  WHEN NEW.operation_id IS NOT OLD.operation_id
    OR NEW.issued_seq   IS NOT OLD.issued_seq
    OR NEW.kind         IS NOT OLD.kind
    OR NEW.envelope     IS NOT OLD.envelope
    OR NEW.issuer       IS NOT OLD.issuer
    OR NEW.principal    IS NOT OLD.principal
    OR NEW.target_hash  IS NOT OLD.target_hash
    OR NEW.scope_json   IS NOT OLD.scope_json
    OR NEW.plan_ref     IS NOT OLD.plan_ref
    OR NEW.expires_at   IS NOT OLD.expires_at
    OR NEW.signature    IS NOT OLD.signature
    OR NEW.issued_at    IS NOT OLD.issued_at
BEGIN
  SELECT RAISE(ABORT,
    'wal_ops envelope columns are immutable; only state/updated_at/reason may change');
END;

-- Recovery scans for non-terminal ops on boot (§5.6 step 1).
CREATE INDEX wal_ops_open_idx
  ON wal_ops(state, issued_at)
  WHERE state IN ('ISSUED','PREPARED');

-- Dependency lookup for the recovery DAG. Acyclicity is enforced
-- against wal_ops.issued_seq, the authoritative monotonic ordering — NOT
-- against ULID lexicographic order, which is unreliable within the
-- same millisecond.
CREATE TABLE wal_op_deps (
  operation_id     TEXT NOT NULL,
  depends_on_op_id TEXT NOT NULL,
  PRIMARY KEY (operation_id, depends_on_op_id),
  CHECK (operation_id != depends_on_op_id),    -- no self-edge
  FOREIGN KEY (operation_id)     REFERENCES wal_ops(operation_id) ON DELETE CASCADE,
  FOREIGN KEY (depends_on_op_id) REFERENCES wal_ops(operation_id)
);
CREATE INDEX wal_op_deps_reverse_idx ON wal_op_deps(depends_on_op_id);

-- DAG enforcement: dependency must point to an op with a strictly
-- smaller issued_seq. CHECK can't run subqueries in SQLite, so a
-- BEFORE INSERT trigger does the lookup. Combined with the FK on
-- both sides and the strictly-monotonic issued_seq, this makes
-- self-edges and arbitrary cycles unrepresentable.
CREATE TRIGGER wal_op_deps_must_be_acyclic
  BEFORE INSERT ON wal_op_deps
  FOR EACH ROW
  WHEN (SELECT issued_seq FROM wal_ops WHERE operation_id = NEW.depends_on_op_id)
       >= (SELECT issued_seq FROM wal_ops WHERE operation_id = NEW.operation_id)
BEGIN
  SELECT RAISE(ABORT,
    'wal_op_deps.depends_on_op_id must have a strictly smaller issued_seq');
END;

CREATE TRIGGER wal_op_deps_immutable
  BEFORE UPDATE ON wal_op_deps
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'wal_op_deps rows are immutable');
END;

-- Dependency edges are part of the durable recovery DAG. DELETE would
-- silently weaken ordering guarantees: the boot-time replay walk
-- could then schedule a child op as if it were independent. Block
-- DELETE outright; the only way wal_op_deps rows disappear is via
-- the ON DELETE CASCADE from wal_ops — which is itself blocked by
-- wal_ops_no_delete.
CREATE TRIGGER wal_op_deps_no_delete
  BEFORE DELETE ON wal_op_deps
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'wal_op_deps is append-only; DELETE not permitted');
END;

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

-- A step's identity (which op it belongs to, its position in the
-- step graph, and what it does) is fixed at creation. Recovery walks
-- the step graph from step_ord = 0 upward; renumbering or re-keying
-- a step would silently rewrite the recovery order. Only execution
-- state mutates: state, attempts, last_error, started_at,
-- finished_at, pre_image (which is staged once and may be
-- subsequently cleared on COMPENSATED).
-- Step FSM enforcement. Allowed edges:
--   PENDING -> DONE | FAILED
--   FAILED  -> PENDING (retry)        | COMPENSATED
--   DONE    -> COMPENSATED            (rollback after a downstream abort)
--   COMPENSATED -> (terminal)
CREATE TRIGGER wal_steps_state_transition
  BEFORE UPDATE OF state ON wal_steps
  FOR EACH ROW
  WHEN NEW.state IS NOT OLD.state
   AND NOT (
        (OLD.state = 'PENDING'     AND NEW.state IN ('DONE','FAILED'))
     OR (OLD.state = 'FAILED'      AND NEW.state IN ('PENDING','COMPENSATED'))
     OR (OLD.state = 'DONE'        AND NEW.state = 'COMPENSATED')
   )
BEGIN
  SELECT RAISE(ABORT,
    'wal_steps.state transition not allowed');
END;

CREATE TRIGGER wal_steps_identity_immutable
  BEFORE UPDATE ON wal_steps
  FOR EACH ROW
  WHEN NEW.operation_id IS NOT OLD.operation_id
    OR NEW.step_ord     IS NOT OLD.step_ord
    OR NEW.step_kind    IS NOT OLD.step_kind
BEGIN
  SELECT RAISE(ABORT,
    'wal_steps identity (operation_id, step_ord, step_kind) is immutable');
END;

-- Recovery treats wal_steps as the authoritative cursor (resume at
-- the lowest non-DONE step). DELETE would erase that cursor and
-- make recovery infer an incomplete graph. The only valid removal
-- path is the ON DELETE CASCADE from wal_ops, which is itself
-- blocked by wal_ops_no_delete — so this trigger blocks the direct
-- DELETE path and closes the loop.
CREATE TRIGGER wal_steps_no_delete
  BEFORE DELETE ON wal_steps
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'wal_steps is append-only; DELETE not permitted');
END;
```

#### 0003_replay.sql

> **Brief deviation, called out for review.** The brief shows `used (...,
> UNIQUE(operation_id, nonce))`. This spec strengthens it to
> `operation_id PRIMARY KEY` + `UNIQUE(issuer, sequence)` +
> `UNIQUE(issuer, nonce)` + a deferred FK to `wal_ops`, plus the two
> sequence-monotonicity triggers above so SQLite — not caller code —
> rejects rewinds and keeps `issuer_seq.high_water` in lock-step with
> the ledger. This is consistent with §5.6's claim that "operation_id
> is the idempotency key" and with `wal_ops.operation_id` being a PK.
> Flag as a brief refinement; revert if the brief intentionally permits
> non-advancing sequences.

```sql
-- Replay-attack ledger (§4.2). All three tables are created before
-- any triggers, since `CREATE TRIGGER ... ON <name>` requires the
-- table to exist. Triggers cross-reference all three tables (e.g.,
-- used_sequence_must_advance reads issuer_seq), but trigger-body
-- references are bound at fire time, not at creation.

CREATE TABLE used (
  operation_id  TEXT NOT NULL PRIMARY KEY,                  -- one ledger row per op_id;
                                                            -- coheres with wal_ops.operation_id PK
  nonce         BLOB NOT NULL,
  issuer        TEXT NOT NULL,
  sequence      INTEGER NOT NULL CHECK (sequence >= 0),
  committed_at  INTEGER NOT NULL,
  UNIQUE (issuer, sequence),    -- per-issuer sequence uniqueness (no duplicate values)
  UNIQUE (issuer, nonce),       -- nonce uniqueness scoped to the issuer's trust boundary
  FOREIGN KEY (operation_id) REFERENCES wal_ops(operation_id)
    DEFERRABLE INITIALLY DEFERRED
);

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

-- ===== triggers (declared after all tables) =====

-- Anti-rewind: an insert into `used` must strictly advance
-- issuer_seq.high_water. UNIQUE alone prevents reusing a sequence
-- value but not rewinding to an unused-but-lower one (e.g., commit
-- seq 5 after seq 10). This trigger rejects non-advancing inserts.
-- Cross-table consistency: used.issuer MUST equal the issuer of the
-- wal_ops row referenced by operation_id. Without this, a malformed
-- insert could attribute one issuer's operation_id to another
-- issuer's nonce/sequence space and burn or advance the wrong
-- issuer's high-water mark.
CREATE TRIGGER used_issuer_matches_wal
  BEFORE INSERT ON used
  FOR EACH ROW
  WHEN NEW.issuer IS NOT (
    SELECT issuer FROM wal_ops WHERE operation_id = NEW.operation_id
  )
BEGIN
  SELECT RAISE(ABORT,
    'used.issuer must match wal_ops.issuer for the referenced operation_id');
END;

CREATE TRIGGER used_sequence_must_advance
  BEFORE INSERT ON used
  FOR EACH ROW
  WHEN EXISTS (
    SELECT 1 FROM issuer_seq
     WHERE issuer = NEW.issuer
       AND high_water >= NEW.sequence
  )
BEGIN
  SELECT RAISE(ABORT,
    'used.sequence must strictly advance issuer_seq.high_water');
END;

-- Atomically advance issuer_seq.high_water on every successful `used`
-- INSERT. The matching `used` row is already visible to the BEFORE
-- INSERT trigger on issuer_seq below, so this passes its match check.
CREATE TRIGGER used_advance_high_water
  AFTER INSERT ON used
  FOR EACH ROW
BEGIN
  INSERT INTO issuer_seq (issuer, high_water)
    VALUES (NEW.issuer, NEW.sequence)
    ON CONFLICT(issuer) DO UPDATE
      SET high_water = excluded.high_water
      WHERE excluded.high_water > issuer_seq.high_water;
END;

-- The replay ledger is append-only. UPDATE would re-open the rewind /
-- divergence cases the INSERT path is hardened against. DELETE would
-- free the (issuer, nonce) slot while issuer_seq.high_water stays
-- put, opening the same nonce/sequence to be replayed under a
-- different operation_id. Repair must record state in a separate
-- audit table, not mutate the ledger.
CREATE TRIGGER used_immutable
  BEFORE UPDATE ON used
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'used rows are append-only; UPDATE not permitted');
END;

CREATE TRIGGER used_no_delete
  BEFORE DELETE ON used
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'used rows are append-only; DELETE not permitted');
END;

-- issuer_seq is the rewind guard for used_sequence_must_advance.
-- A DELETE wipes the high-water for an issuer, after which a fresh
-- `used` insert at any sequence (including a previously-rewound one)
-- would pass the rewind check and used_advance_high_water would
-- recreate issuer_seq at that lower value. Block DELETE outright.
CREATE TRIGGER issuer_seq_no_delete
  BEFORE DELETE ON issuer_seq
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'issuer_seq is append-only; DELETE not permitted');
END;

-- Direct INSERT INTO issuer_seq is rejected unless the (issuer,
-- high_water) tuple exists in `used`. The only legitimate inserter
-- is used_advance_high_water (AFTER INSERT on used), which runs
-- after the matching `used` row is visible.
CREATE TRIGGER issuer_seq_insert_must_match_ledger
  BEFORE INSERT ON issuer_seq
  FOR EACH ROW
  WHEN NOT EXISTS (
    SELECT 1 FROM used
      WHERE issuer = NEW.issuer
        AND sequence = NEW.high_water
  )
BEGIN
  SELECT RAISE(ABORT,
    'issuer_seq INSERT must correspond to a row in `used`');
END;

-- issuer_seq mirrors `used`: every advance must correspond to a real
-- ledger row. Cannot rewind, cannot leap ahead.
CREATE TRIGGER issuer_seq_only_via_ledger
  BEFORE UPDATE ON issuer_seq
  FOR EACH ROW
  WHEN NEW.high_water <= OLD.high_water
    OR NOT EXISTS (
      SELECT 1 FROM used
        WHERE issuer = NEW.issuer
          AND sequence = NEW.high_water
    )
BEGIN
  SELECT RAISE(ABORT,
    'issuer_seq.high_water can only advance to a sequence already in `used`');
END;

```

#### 0004_locks.sql

Verbatim from brief §5.6 lock-table block (lines ~1820-1865):

- `locks` — columns: `scope_kind`, `scope_key`, `mode CHECK IN
  ('shared','exclusive','free')`, `holder_count INTEGER NOT NULL
  CHECK (holder_count >= 0)`, `epoch INTEGER NOT NULL CHECK (epoch >= 0)`,
  `waiters BLOB`, `last_heartbeat_at INTEGER`,
  `PRIMARY KEY (scope_kind, scope_key)`. A row-level CHECK ties the
  two state columns together so contradictory states (`free` with
  holders, `exclusive` with > 1, `shared` with 0) are
  unrepresentable:

  ```sql
  CHECK (
    (mode = 'free'      AND holder_count = 0) OR
    (mode = 'exclusive' AND holder_count = 1) OR
    (mode = 'shared'    AND holder_count >= 1)
  )
  ```

  **Both `mode` and `holder_count` are derived from `lock_holders`
  via triggers** so caller statement ordering does not matter. The
  schema reduces to: callers manage `lock_holders` rows; the
  database derives `locks.(mode, holder_count)` automatically.

  To do this, `lock_holders` carries a `mode_requested TEXT NOT NULL
  CHECK (mode_requested IN ('shared', 'exclusive'))` column (one
  per holder, captured at acquisition). The sync triggers compute:
  - `holder_count = (SELECT COUNT(*) FROM lock_holders WHERE
    scope_kind = ? AND scope_key = ?)`,
  - `mode = CASE
      WHEN holder_count = 0 THEN 'free'
      WHEN EXISTS (SELECT 1 FROM lock_holders WHERE ... AND
                   mode_requested = 'exclusive') THEN 'exclusive'
      ELSE 'shared'
    END`.

  An additional CHECK on `lock_holders` rejects an exclusive
  request when another holder already exists for the same scope
  (regardless of mode), enforcing exclusivity at write time:
  ```sql
  CREATE TRIGGER lock_holders_exclusive_only_alone
    BEFORE INSERT ON lock_holders
    FOR EACH ROW
    WHEN NEW.mode_requested = 'exclusive'
     AND EXISTS (
       SELECT 1 FROM lock_holders
        WHERE scope_kind = NEW.scope_kind
          AND scope_key  = NEW.scope_key
     )
  BEGIN
    SELECT RAISE(ABORT,
      'exclusive lock cannot coexist with any other holder');
  END;
  ```

  And reject a shared request when an exclusive holder already
  exists:
  ```sql
  CREATE TRIGGER lock_holders_shared_blocked_by_exclusive
    BEFORE INSERT ON lock_holders
    FOR EACH ROW
    WHEN NEW.mode_requested = 'shared'
     AND EXISTS (
       SELECT 1 FROM lock_holders
        WHERE scope_kind = NEW.scope_kind
          AND scope_key  = NEW.scope_key
          AND mode_requested = 'exclusive'
     )
  BEGIN
    SELECT RAISE(ABORT,
      'shared lock cannot be acquired while exclusive holder exists');
  END;
  ```

  With these triggers `locks.(mode, holder_count)` is fully derived
  and **the joint CHECK is automatically maintained**. The §5.6
  acquisition protocol simplifies: the storage impl only inserts /
  deletes `lock_holders` rows (with the appropriate
  `mode_requested`), and the database manages the summary row. The
  CHECK on `locks` is retained as an extra layer that must always
  hold; the §15 concurrency invariant test asserts it does.
- `lock_holders` — every column from the brief block (`holder_id`,
  `acquired_epoch`, `owner_incarnation`, `boot_id`, `reclaim_deadline`)
  **plus** `mode_requested TEXT NOT NULL CHECK (mode_requested IN
  ('shared','exclusive'))` for trigger-driven mode derivation,
  `PRIMARY KEY (scope_kind, scope_key, holder_id)`,
  `FOREIGN KEY (scope_kind, scope_key) REFERENCES locks(scope_kind, scope_key)`.
  This is a brief deviation called out in §10.
- Index `lock_holders_reclaim_idx ON lock_holders(reclaim_deadline)` —
  GC step in the acquisition transaction filters by deadline (§5.6
  acquisition protocol, step 1).
- **Sync triggers** keep `locks.holder_count` consistent with the
  number of `lock_holders` rows. The brief stores the count
  redundantly because the §5.6 acquisition protocol reads it inside
  the same transaction as the holder rows; without sync triggers the
  two can diverge across crash recovery, retry bugs, or hand repair,
  and the lock manager would make decisions from contradictory state.
  - `lock_holders_count_after_insert` — `AFTER INSERT ON lock_holders`
    sets `locks.holder_count = (SELECT COUNT(*) FROM lock_holders
    WHERE scope_kind = NEW.scope_kind AND scope_key = NEW.scope_key)`.
    The storage impl is responsible for setting `locks.mode` to the
    correct value (`'shared'` or `'exclusive'`) in the same
    transaction; the lock CHECK rejects mismatches at COMMIT.
  - `lock_holders_count_after_delete` — `AFTER DELETE ON lock_holders`
    recomputes the count against `OLD` and **flips `mode` to `'free'`
    when the count drops to 0**. This keeps the
    `(mode, holder_count)` invariant on the release path without
    requiring the storage impl to remember to update `mode`. When the
    count drops from N≥2 to N≥1 the mode stays as it was (the
    remaining shared holders keep `mode = 'shared'`).
  - `lock_holders_keys_immutable` — `BEFORE UPDATE ON lock_holders`
    raises ABORT if any of `scope_kind`, `scope_key`, `holder_id`, or
    `acquired_epoch` changes. The §5.6 acquisition protocol never
    re-keys a holder; reclaim is DELETE + INSERT. Blocking key
    mutations forecloses the "operator UPDATEs scope_kind to point a
    holder elsewhere, count drifts on both old and new scopes" path.
  - `lock_holders_count_after_update` — `AFTER UPDATE ON lock_holders`
    recomputes the count for both `OLD.(scope_kind, scope_key)` and
    `NEW.(scope_kind, scope_key)` (defense-in-depth: even though the
    keys-immutable trigger means OLD == NEW for the key fields, this
    keeps the count synced if the keys-immutable trigger is ever
    relaxed for a future migration).
  - The triggers reject negative counts via the table's
    `CHECK (holder_count >= 0)`.

  Result: any insert or delete on `lock_holders` is paired in the same
  SQLite write with `locks.holder_count` reflecting reality. Drift is
  detected at the `verify_schema_integrity_at` allowlist (the triggers
  are app-owned objects that must be present).
- `daemon_incarnation` singleton — `only_one INTEGER PRIMARY KEY CHECK
  (only_one = 1)`, `incarnation TEXT NOT NULL`, `boot_id TEXT NOT NULL`,
  `started_at INTEGER NOT NULL`.
- `reader_fence`:
  ```sql
  CREATE TABLE reader_fence (
    scope_kind  TEXT NOT NULL,
    scope_key   TEXT NOT NULL,
    op_id       TEXT NOT NULL,
    state       TEXT NOT NULL CHECK (state IN ('tombstoning','closed')),
    opened_at   INTEGER NOT NULL,
    PRIMARY KEY (scope_kind, scope_key, op_id),
    FOREIGN KEY (op_id) REFERENCES wal_ops(operation_id)
  );

  -- At most one live (tombstoning) fence per scope, but multiple
  -- closed fences per scope are allowed as audit history. A second
  -- forget on the same scope inserts a new (..., op_id_2,
  -- 'tombstoning') row alongside the prior closed fence.
  CREATE UNIQUE INDEX reader_fence_one_live_per_scope
    ON reader_fence(scope_kind, scope_key)
    WHERE state = 'tombstoning';
  ```
  Keying by `(scope_kind, scope_key, op_id)` lets a closed fence
  remain as audit history while a fresh forget on the same scope
  inserts a new `tombstoning` row. The partial unique index ensures
  at most one concurrent forget per scope (which §5.6 enforces via
  the exclusive session lock anyway, but the schema makes it
  unrepresentable independently).

  Read plans join on `reader_fence WHERE state='tombstoning'` —
  closed fences never affect reader visibility.

  The FK ties every fence row to a real WAL op so a malformed insert
  cannot block a scope behind a nonexistent operation.

  **Stale-fence cleanup** is enforced by a trigger that auto-clears
  fence rows whose owning op reaches a terminal failure state
  (`ABORTED` or `REJECTED`). Without this, a forget op that aborts
  in Phase A would leave a permanent read barrier on the scope:

  ```sql
  CREATE TRIGGER reader_fence_clear_on_terminal_abort
    AFTER UPDATE OF state ON wal_ops
    FOR EACH ROW
    WHEN NEW.state IN ('ABORTED', 'REJECTED')
     AND OLD.state NOT IN ('ABORTED', 'REJECTED')
  BEGIN
    DELETE FROM reader_fence
      WHERE op_id = NEW.operation_id;
  END;
  ```

  A successfully completed forget op leaves its fence in `state =
  'closed'` (set by the §5.6 Phase A fence-close chunk); a
  reopen-time recovery walker only re-evaluates `'tombstoning'`
  fences, so closed fences are inert and safe to leave behind.
  COMMITTED forget ops therefore do not auto-delete their fence
  rows — only failures do.

  **Fence integrity triggers** keep the fence trustworthy as a
  read barrier:

  ```sql
  -- Identity columns and the owning op are frozen post-insert.
  CREATE TRIGGER reader_fence_identity_immutable
    BEFORE UPDATE ON reader_fence
    FOR EACH ROW
    WHEN NEW.scope_kind IS NOT OLD.scope_kind
      OR NEW.scope_key  IS NOT OLD.scope_key
      OR NEW.op_id      IS NOT OLD.op_id
      OR NEW.opened_at  IS NOT OLD.opened_at
  BEGIN
    SELECT RAISE(ABORT,
      'reader_fence identity (scope_kind, scope_key, op_id, opened_at) is immutable');
  END;

  -- The state FSM is one-way: tombstoning -> closed only.
  CREATE TRIGGER reader_fence_state_transition
    BEFORE UPDATE OF state ON reader_fence
    FOR EACH ROW
    WHEN NEW.state IS NOT OLD.state
     AND NOT (OLD.state = 'tombstoning' AND NEW.state = 'closed')
  BEGIN
    SELECT RAISE(ABORT,
      'reader_fence.state transition not allowed (only tombstoning -> closed)');
  END;

  -- Direct DELETE is rejected. The legitimate deletion paths are:
  --   (a) ON DELETE CASCADE from wal_ops -- already blocked because
  --       wal_ops_no_delete prevents wal_ops deletes;
  --   (b) reader_fence_clear_on_terminal_abort, which DELETEs from
  --       within a trigger context (see exemption note below).
  CREATE TRIGGER reader_fence_no_direct_delete
    BEFORE DELETE ON reader_fence
    FOR EACH ROW
    -- The exemption: when the deletion is driven by
    -- reader_fence_clear_on_terminal_abort, the wal_ops row whose
    -- terminal-state UPDATE caused the cascade is already at state
    -- IN ('ABORTED','REJECTED'). A direct caller-initiated DELETE
    -- has no such row, so the predicate distinguishes the two.
    WHEN NOT EXISTS (
      SELECT 1 FROM wal_ops
       WHERE operation_id = OLD.op_id
         AND state IN ('ABORTED', 'REJECTED')
    )
  BEGIN
    SELECT RAISE(ABORT,
      'reader_fence DELETE is permitted only via terminal-abort cleanup');
  END;
  ```

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

-- The consent journal is the durable audit surface; the materializer
-- tails it by row_id and trusts that prior rows are stable. Block
-- UPDATE so a tailed row cannot be retroactively rewritten. Block
-- DELETE so a row cannot be removed before the materializer (or an
-- auditor) reads it. Source-of-truth append-only, not just convention.
CREATE TRIGGER consent_journal_immutable
  BEFORE UPDATE ON consent_journal
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'consent_journal rows are immutable');
END;

CREATE TRIGGER consent_journal_no_delete
  BEFORE DELETE ON consent_journal
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'consent_journal is append-only; DELETE not permitted');
END;
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
  │
  ├─ apply_read_only_pragmas(&conn)      -- foreign_keys=ON, busy_timeout=5000,
  │                                      -- temp_store=MEMORY. None of these
  │                                      -- mutate persistent on-disk state.
  │
  ├─ verify_migration_history(&conn)?    -- §5.2 ledger checksum + contiguity check.
  │                                      -- Runs BEFORE any persistent state change
  │                                      -- so a diverged vault is rejected before
  │                                      -- journal mode flips or DDL applies.
  │
  ├─ verify_schema_integrity_at(&conn,   -- §5.1 shape check at the CURRENT
  │     current_user_version)?           -- user_version. Drift rejected before
  │                                      -- to_latest mutates.
  │
  ├─ apply_persistent_pragmas(&conn)     -- journal_mode=WAL, synchronous=NORMAL.
  │                                      -- These persist across opens and may
  │                                      -- create -wal/-shm side files; only run
  │                                      -- after the vault is proved compatible.
  │
  ├─ migrations().to_latest(&mut conn)   -- idempotent on up-to-date DB
  ├─ verify_schema_integrity_at(&conn,   -- §5.1 shape check at HEAD after migration.
  │     head_user_version)?
  ├─ verify_replay_ledger_consistency(   -- §5.3 data-integrity check tying
  │     &conn)?                          -- issuer_seq to used. Only runs once
  │                                      -- the schema has reached version >= 3.
  └─ Ok(conn)
```

Re-opening an up-to-date DB is a no-op past pragma application. Opening a
DB whose `user_version` is *higher* than `migrations().count()` returns
`StoreError::IncompatibleSchema` — forward-only is enforced.

### 5.1 Schema integrity check (same-version drift detection)

`rusqlite_migration` only checks `user_version`; it does not detect a
manually-tampered schema at the same version (e.g., a dropped FTS
trigger or a missing partial index would silently leave the vault
limping along). To close that gap, `open()` runs a **targeted-allowlist**
integrity check after migrations apply.

Hashing every `sqlite_master` row would sweep up engine-owned objects
(`sqlite_*` autoindexes, FTS5 shadow tables `records_fts_data`,
`records_fts_idx`, `records_fts_content`, `records_fts_docsize`,
`records_fts_config`, future SQLite-version internals) — a benign
SQLite upgrade could then fail-closed an otherwise healthy vault. So
the check operates on a static, app-owned **allowlist** instead:

```rust
struct ExpectedObject {
    object_type: &'static str,   // 'table' | 'index' | 'trigger' | 'view'
    name:        &'static str,
    sql_hash:    &'static str,   // BLAKE3 of normalized `sql` (whitespace-collapsed,
                                 // trailing-`;` stripped, lowercased keywords)
}

// One expected-object set per migration step. EXPECTED_BY_VERSION[N] is the
// schema after migration N has applied (and before N+1 has). The pre-migration
// check picks the entry matching the DB's current user_version; the
// post-migration check picks the entry for the head version. A vault that
// drifted at any earlier-than-head version is rejected before we mutate it.
const EXPECTED_BY_VERSION: &[&[ExpectedObject]] = &[
    &[],                             // version 0: empty DB, no objects
    EXPECTED_AFTER_0001,             // version 1: records / fts / edges / triggers / view / schema_migrations
    EXPECTED_AFTER_0002,             // version 2: + wal_ops, wal_op_deps, wal_steps + their indexes
    EXPECTED_AFTER_0003,             // version 3: + replay tables + sequence triggers
    EXPECTED_AFTER_0004,             // version 4: + locks, lock_holders, daemon_incarnation, reader_fence + sync triggers
    EXPECTED_AFTER_0005,             // version 5 (= head): + consent_journal
];

// Each EXPECTED_AFTER_NNNN is a hand-listed slice with one row per
// app-owned object the migration adds, e.g.:
//   const EXPECTED_AFTER_0001: &[ExpectedObject] = &[
//       ExpectedObject { object_type: "table",   name: "schema_migrations", sql_hash: "..." },
//       ExpectedObject { object_type: "table",   name: "records",           sql_hash: "..." },
//       ExpectedObject { object_type: "index",   name: "records_active_target_idx", sql_hash: "..." },
//       ExpectedObject { object_type: "view",    name: "records_latest",    sql_hash: "..." },
//       ExpectedObject { object_type: "trigger", name: "records_fts_ai",    sql_hash: "..." },
//       // ... all 0001 objects ...
//   ];
// records_fts is a virtual-table declaration only — its `sql` column is
// the CREATE VIRTUAL TABLE statement, which is stable. Its shadow tables
// (records_fts_data etc.) are NOT in this list.

fn verify_schema_integrity_at(conn: &Connection, version: usize)
    -> Result<(), StoreError>
{
    let expected = EXPECTED_BY_VERSION
        .get(version)
        .ok_or(StoreError::IncompatibleSchema { found: version, expected: HEAD })?;
    for obj in expected.iter() {
        let actual_sql_hash = lookup_and_hash(conn, obj.object_type, obj.name)?;
        match actual_sql_hash {
            None => return Err(StoreError::SchemaDrift {
                object: obj.name.into(),
                kind: SchemaDriftKind::Missing,
            }),
            Some(h) if h != obj.sql_hash => return Err(StoreError::SchemaDrift {
                object: obj.name.into(),
                kind: SchemaDriftKind::SqlMismatch {
                    expected: obj.sql_hash, actual: h,
                },
            }),
            _ => {}
        }
    }
    // Deny-by-default sweep over executable objects on app tables.
    deny_unexpected_objects_on_app_tables(conn, expected)?;
    Ok(())
}
```

Properties:

- **Engine-owned objects ignored.** `sqlite_*` autoindexes and the FTS5
  shadow set (`records_fts_data`, `_idx`, `_content`, `_docsize`,
  `_config`) are never queried; SQLite version upgrades that change them
  do not fail the check.
- **Drift cases caught.** Dropped trigger, dropped partial index,
  altered view body, hand-edited `records` schema all surface as
  `SchemaDrift`.
- **Executable objects on app tables are deny-by-default.** After the
  allowlist pass, `verify_schema_integrity_at` runs a second sweep
  that enumerates every `index`, `trigger`, and `view` in
  `sqlite_master` whose `tbl_name` is an app-owned table (`records`,
  `edges`, `wal_*`, `used`, `issuer_seq`, `outstanding_challenges`,
  `locks`, `lock_holders`, `daemon_incarnation`, `reader_fence`,
  `consent_journal`, `schema_migrations`) and rejects any not in the
  per-version expected slice. This closes the "operator added an
  `AFTER UPDATE` trigger on `records` that silently mutates rows" hole.
  Engine-owned `sqlite_autoindex_*` rows are still excluded by name
  prefix.
- **Standalone user objects are allowed.** A user-installed view, index,
  or trigger whose `tbl_name` is *not* an app-owned table (e.g., an
  audit table the operator created in the same DB) does not trip the
  check.

The `sql_hash` constants for every `EXPECTED_AFTER_NNNN` slice are
generated and asserted by a unit test (`expected_objects_match_each_version`)
that walks `EXPECTED_BY_VERSION`, applies migrations 0..=N to a fresh
`:memory:` DB, and recomputes the hashes. Adding a migration appends a
new `EXPECTED_AFTER_NNNN` slice in the same commit; CI gates on the
unit test.

> Note on residual risk: this is a **shape** check, not a **history**
> check. It catches drift at head but does not detect that an earlier
> historical migration was edited and re-applied to produce the same
> head. That history-level guarantee is provided separately by §5.2's
> migration checksum table.

### 5.3 Replay-ledger data consistency

`issuer_seq.high_water` is the cached rewind guard for
`used_sequence_must_advance`. Its in-process integrity is enforced
by the trigger set (no DELETE, no UPDATE that doesn't match a `used`
row, no INSERT without a backing ledger row). But a vault produced
by an older binary, manual repair, or pre-trigger corruption could
arrive with `issuer_seq.high_water` stale relative to
`MAX(used.sequence)`. If lower unused sequences then start passing
the rewind check, the replay window the design closes is reopened.

`open()` runs a one-pass consistency check after migrations:

The check requires **exact** agreement between
`issuer_seq.high_water` and `MAX(used.sequence)` for every issuer,
and it rejects orphaned `issuer_seq` rows that have no backing
`used` history — both stale (`high_water < used_max`) and
over-advanced (`high_water > used_max`) caches are corruption
modes. An over-advanced cache is the more dangerous one: it
permanently rejects every legitimate replay-ledger insert at or
below the inflated value, a durable denial-of-service.

```rust
fn verify_replay_ledger_consistency(conn: &Connection) -> Result<(), StoreError> {
    // FULL OUTER JOIN via UNION ALL: SQLite has no FULL OUTER, but a
    // LEFT JOIN from `used` plus a separate "orphan in issuer_seq" pass
    // covers every issuer present in either table.
    let mut stmt = conn.prepare("
        SELECT u.issuer                   AS issuer,
               MAX(u.sequence)            AS used_max,
               s.high_water               AS seq_high_water,   -- NULL if missing
               1                          AS source_used
          FROM used u
          LEFT JOIN issuer_seq s ON s.issuer = u.issuer
         GROUP BY u.issuer
        UNION ALL
        SELECT s.issuer, NULL, s.high_water, 0
          FROM issuer_seq s
         WHERE NOT EXISTS (
             SELECT 1 FROM used u WHERE u.issuer = s.issuer
         )
    ")?;
    for row in stmt.query_map([], /* ... */)? {
        let (issuer, used_max, seq_hw, source_used) = row?;
        match (used_max, seq_hw) {
            // Orphaned cache row: no backing ledger.
            (None, Some(hw)) => return Err(StoreError::ReplayLedgerInconsistent {
                issuer, kind: ReplayLedgerKind::OrphanedHighWater { high_water: hw },
            }),
            // Missing cache row: ledger has rows but no high_water.
            (Some(used_max), None) => return Err(StoreError::ReplayLedgerInconsistent {
                issuer, kind: ReplayLedgerKind::MissingHighWater { used_max },
            }),
            // Mismatch in either direction.
            (Some(used_max), Some(hw)) if hw != used_max => {
                return Err(StoreError::ReplayLedgerInconsistent {
                    issuer,
                    kind: if hw < used_max {
                        ReplayLedgerKind::Stale { used_max, high_water: hw }
                    } else {
                        ReplayLedgerKind::OverAdvanced { used_max, high_water: hw }
                    },
                });
            }
            _ => {}
        }
    }
    Ok(())
}
```

**Same-release repair path.** Hard-failing every corruption case
without a recovery path would turn cache divergence into a
service-blocking outage. `used` is the authoritative ledger;
`issuer_seq` is a derived cache. `open()` therefore auto-repairs
in one transaction when the schema-level triggers prove `used` is
internally consistent (which they do — UNIQUE(issuer, sequence)
and the no-DELETE / no-UPDATE triggers make `used` tamper-resistant
by construction):

```rust
fn repair_replay_ledger(conn: &Connection) -> Result<(), StoreError> {
    let txn = conn.transaction()?;
    txn.execute_batch("
        DELETE FROM issuer_seq;
        INSERT INTO issuer_seq (issuer, high_water)
          SELECT issuer, MAX(sequence) FROM used GROUP BY issuer;
    ")?;
    txn.commit()?;
    Ok(())
}
```

The repair is gated behind a `repair_inconsistent_caches: bool`
flag on `open()` (default `true` for embedded vaults; tests pass
`false` to assert the inconsistency without auto-fixing). The
fixture-bypass path that is the only way to *create* an
inconsistency is itself test-only, so production opens never
trigger the repair branch unless the underlying corruption was
introduced by an older binary or out-of-band tool.

The DELETE+INSERT happens inside a single transaction with foreign
keys + the `issuer_seq_*` triggers temporarily disabled (via a
session-scoped pragma flip the storage impl owns); the rebuild
restores invariants before the transaction commits, so any
concurrent reader sees the old or the new cache, never partial
state. A repair event surfaces in `metrics.jsonl` with the
divergence kind so operators can investigate the root cause.

If `repair_inconsistent_caches` is `false`, `open()` returns
`StoreError::ReplayLedgerInconsistent` with one of `Stale`,
`OverAdvanced`, `OrphanedHighWater`, or `MissingHighWater`.

### 5.2 Migration checksum ledger (history-level guarantee)

`rusqlite_migration` only tracks `user_version`; on its own it cannot
detect that migration `0003_replay.sql` was edited after a vault
applied an older version of it. To make the acceptance criterion
"fails on checksum mismatch" load-bearing, the first migration creates
a checksum ledger and every migration appends to it inside the same
transaction that applies its DDL:

```sql
-- Created by 0001_records.sql, populated by every migration.
CREATE TABLE schema_migrations (
  migration_id  INTEGER NOT NULL PRIMARY KEY,            -- 1, 2, 3, ...
  name          TEXT    NOT NULL,                         -- e.g. '0001_records'
  sql_blake3    TEXT    NOT NULL,                         -- 64-hex BLAKE3 of the .sql file bytes
  applied_at    INTEGER NOT NULL                          -- unix ms
);

-- The ledger must be append-only and contiguous. Block DELETE and
-- UPDATE so deleting row 3 or rewriting row 1's hash cannot bypass
-- verify_migration_history's contiguity + checksum check.
CREATE TRIGGER schema_migrations_no_delete
  BEFORE DELETE ON schema_migrations
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'schema_migrations is append-only; DELETE not permitted');
END;

CREATE TRIGGER schema_migrations_immutable
  BEFORE UPDATE ON schema_migrations
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'schema_migrations rows are immutable');
END;
```

Each migration's last statement is:

```sql
INSERT INTO schema_migrations (migration_id, name, sql_blake3, applied_at)
  VALUES (:id, :name, :hash, :now);
```

`open()` calls `verify_migration_history(&conn)` **before**
`to_latest()` so a known-diverged vault is rejected before any new
forward-only DDL is applied. The check:

1. If `schema_migrations` does not exist (fresh vault, no migrations
   yet applied), skip — `to_latest()` will create the table via
   migration `0001`.
2. Read `PRAGMA user_version` and `SELECT migration_id, name, sql_blake3
   FROM schema_migrations ORDER BY migration_id`. Reject as
   `MigrationHistoryMismatch` if **any** of these hold:
   - row count != `user_version` (a row was deleted or duplicated),
   - the `migration_id` sequence is not exactly `1..=user_version` (gap
     or out-of-order),
   - any row's `(name, sql_blake3)` does not match the compiled manifest
     entry at the same position,
   - `user_version` exceeds the compiled manifest length (treated as
     future-version `IncompatibleSchema`).
3. Only after the ledger is contiguous and every entry matches does
   `to_latest()` run. Any pending migration is applied on top of
   validated history.

Per-vault tampering protection is layered with structural protection:
the migration in `0001_records.sql` adds two triggers on
`schema_migrations` (`schema_migrations_no_delete` and
`schema_migrations_immutable`) that block both `DELETE` and `UPDATE` on
the ledger, so even an in-process bug cannot silently break the
contiguity invariant the open-time check enforces.

The compiled-in manifest lives in a `MIGRATION_MANIFEST: &[(&str, &str)]`
array in `migrations/mod.rs`, generated at build time by a `build.rs`
script that BLAKE3-hashes each `migrations/sql/*.sql` and emits the
manifest into `OUT_DIR`. No proc-macro, no runtime read of the SQL
files in production binaries.

Mismatch → `StoreError::MigrationHistoryMismatch { migration_id, name,
expected, actual }`. This catches:

- An operator hand-edited `0003_replay.sql` and re-installed cairn
  expecting the change to apply on existing vaults.
- A vault applied a buggy older migration that has since been
  superseded by a fix; the binary refuses to open it instead of
  silently running on the diverged history.

The checksum ledger plus the §5.1 shape check together cover both
*history* drift (what was applied) and *current state* drift (what is
installed now).

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

    #[error("schema drift: object {object} {kind}")]
    SchemaDrift { object: String, kind: SchemaDriftKind },

    #[error("migration history mismatch at id {migration_id} ({name}): expected {expected}, got {actual}")]
    MigrationHistoryMismatch {
        migration_id: i64,
        name:         String,
        expected:     &'static str,
        actual:       String,
    },

    #[error("replay ledger inconsistency for issuer {issuer}: {kind:?}")]
    ReplayLedgerInconsistent { issuer: String, kind: ReplayLedgerKind },
}

#[derive(Debug)]
#[non_exhaustive]
pub enum ReplayLedgerKind {
    /// `issuer_seq.high_water < MAX(used.sequence)`: cache lags ledger.
    Stale          { used_max: i64, high_water: i64 },
    /// `issuer_seq.high_water > MAX(used.sequence)`: cache has been
    /// inflated past any committed value, blocking future writes.
    OverAdvanced   { used_max: i64, high_water: i64 },
    /// `issuer_seq` row exists with no backing `used` history.
    OrphanedHighWater { high_water: i64 },
    /// `used` rows exist for this issuer but no `issuer_seq` row.
    MissingHighWater  { used_max: i64 },
}

#[derive(Debug)]
#[non_exhaustive]
pub enum SchemaDriftKind {
    Missing,
    SqlMismatch { expected: &'static str, actual: String },
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
  - tables: `schema_migrations`, `records`, `edges`, `wal_ops`,
    `wal_op_deps`, `wal_steps`, `used`, `issuer_seq`,
    `outstanding_challenges`, `locks`, `lock_holders`,
    `daemon_incarnation`, `reader_fence`, `consent_journal`
  - virtual tables: `records_fts`
  - views: `records_latest`
  - triggers: `records_fts_ai`, `records_fts_ad`, `records_fts_au`,
    `schema_migrations_no_delete`, `schema_migrations_immutable`,
    `edges_updates_supersede_insert`, `edges_updates_supersede_update`,
    `edges_updates_immutable_after_insert`,
    `edges_drop_updates_when_src_tombstoned`,
    `wal_ops_issued_seq_must_advance`, `wal_ops_state_transition`,
    `wal_ops_envelope_immutable`, `wal_ops_terminal_immutable`,
    `wal_ops_no_delete`, `reader_fence_clear_on_terminal_abort`,
    `wal_steps_state_transition`, `wal_steps_identity_immutable`,
    `wal_steps_no_delete`,
    `reader_fence_identity_immutable`,
    `reader_fence_state_transition`,
    `reader_fence_no_direct_delete`,
    `wal_op_deps_must_be_acyclic`, `wal_op_deps_immutable`,
    `wal_op_deps_no_delete`,
    `consent_journal_immutable`, `consent_journal_no_delete`,
    `used_issuer_matches_wal`, `used_sequence_must_advance`,
    `used_advance_high_water`, `used_immutable`, `used_no_delete`,
    `issuer_seq_no_delete`,
    `issuer_seq_insert_must_match_ledger`, `issuer_seq_only_via_ledger`,
    `lock_holders_count_after_insert`, `lock_holders_count_after_delete`,
    `lock_holders_count_after_update`, `lock_holders_keys_immutable`,
    `lock_holders_exclusive_only_alone`,
    `lock_holders_shared_blocked_by_exclusive`
  - partial indexes: `records_active_target_idx`, `records_path_idx`,
    `records_kind_idx`, `records_visibility_idx`, `records_scope_idx`
  - control-plane indexes: `wal_ops_open_idx`, `wal_op_deps_reverse_idx`,
    `wal_steps_resume_idx`, `outstanding_challenges_exp_idx`,
    `lock_holders_reclaim_idx`, `consent_journal_op_idx`
    (the `used` table's `(issuer, sequence)` and `(issuer, nonce)`
    lookup paths are served by the table's own UNIQUE constraints —
    no explicit secondary index needed)
- `pragmas_applied` — assert `PRAGMA journal_mode` returns `wal`,
  `foreign_keys` returns 1.
- `idempotent_reopen` — `open()` twice on the same path; both succeed and
  `user_version` is stable between calls.
- `partial_migration_resume` — apply migrations 1..=3 manually via the
  runner, then call `open()`; assert all are applied.
- `each_migration_applies_in_isolation` — for each `N` in `1..=HEAD`,
  apply migrations `1..=N-1` to a fresh `:memory:` DB, then apply
  exactly migration `N`. Each step succeeds and `user_version`
  advances by exactly one. Catches DDL ordering bugs like
  "trigger created before its target table" specifically for the
  step at which the failure would manifest.
- `forward_only_rejects_future_schema` — open a fresh DB, manually
  `PRAGMA user_version = 999`; call `open()`; assert `StoreError::IncompatibleSchema`.
- `same_version_drift_is_rejected` — call `open()` to bring the DB to
  head; manually `DROP TRIGGER records_fts_au`; call `open()` again on
  the same path; assert `StoreError::SchemaDrift` with kind `Missing`.
  Second variant drops `records_path_idx` (same expected error). Third
  variant rewrites a view's `sql` via `UPDATE sqlite_master` (or
  recreates with different text); expect `SqlMismatch`. This is the
  drift case `user_version` alone does not catch.
- `engine_owned_schema_changes_ok` — after migrations apply, run
  `INSERT INTO records (...)` to force FTS5 to populate its shadow
  tables; reopen; expect success. Confirms FTS5 internals are not in
  the allowlist.
- `extra_object_on_app_table_rejected` — after migrations, `CREATE
  TRIGGER bad_audit AFTER UPDATE ON records BEGIN INSERT INTO ...; END;`;
  reopen; expect `StoreError::SchemaDrift` (deny-by-default for
  executable objects on app tables). Repeats with an extra index on
  `consent_journal`, an extra trigger on `wal_ops`.
- `extra_object_on_user_table_ok` — after migrations, `CREATE TABLE
  user_audit (...); CREATE INDEX user_audit_ts_idx ON
  user_audit(ts);`; reopen; expect success. Confirms standalone
  user-owned tables are not policed.
- `migration_history_mismatch_rejected` — apply migrations to head;
  manually `UPDATE schema_migrations SET sql_blake3 = '00...00' WHERE
  migration_id = 3`; reopen; expect
  `StoreError::MigrationHistoryMismatch { migration_id: 3, .. }`.
- `fts_round_trip` — minimal smoke: `INSERT INTO records (...)` then
  `SELECT body FROM records_fts WHERE records_fts MATCH '...'` returns the
  row, proving the trigger wired up correctly.
- `wal_op_state_rejects_invalid` — attempt
  `INSERT INTO wal_ops (..., state='WHATEVER', ...)`; assert SQLite
  returns a CHECK constraint failure. Repeats for `wal_steps.state`,
  `locks.mode`, `reader_fence.state`. Proves the closed-set CHECKs are
  load-bearing.
- `orphan_replay_row_rejected` — `PRAGMA foreign_keys = ON`; attempt
  `INSERT INTO used (operation_id, nonce, issuer, sequence,
  committed_at) VALUES ('orphan', ...)` with no matching `wal_ops` row;
  assert FK violation at COMMIT (deferred). Then insert the wal_ops row
  first, retry, expect success.
- `dangling_edge_rejected` — `PRAGMA foreign_keys = ON`; attempt
  `INSERT INTO edges VALUES ('missing-src', 'missing-dst', 'link',
  NULL)`; assert FK violation at COMMIT.
- `updates_edge_must_supersede` — insert two records with the *same*
  `target_id` and different versions; insert `edges (src, dst, kind)`
  with `kind = 'updates'`; assert the trigger ABORTs (an `updates`
  edge is fact-supersession across distinct target_ids, not a
  version-bump pointer). Then insert two records with *different*
  target_ids, retry, expect success.
- `updates_edge_update_path_blocked` — insert a non-`updates` edge,
  then attempt `UPDATE edges SET kind = 'updates' WHERE …`; assert the
  `BEFORE UPDATE` trigger ABORTs unless the predicates are met. Repeat
  with src/dst rotation showing the trigger fires on every UPDATE.
- `updates_edge_immutable` — insert a valid `updates` edge; attempt
  `UPDATE edges SET dst = ... WHERE kind = 'updates'`; assert ABORT
  via `edges_updates_immutable_after_insert`. Repeat for `src` and
  `kind` changes.
- `wal_op_deps_rejects_self` — insert two valid `wal_ops` rows; attempt
  `INSERT INTO wal_op_deps (operation_id, depends_on_op_id) VALUES
  ('A', 'A')`; assert CHECK violation. Cycle rejection is covered by
  `wal_op_deps_uses_issued_seq` below.
- `migration_history_check_runs_before_apply` — generate a fixture
  vault by applying migrations 1..=2 only; manually update
  `schema_migrations` to set a wrong `sql_blake3` for migration 1;
  call `open()` (which would otherwise apply migrations 3..=N); assert
  `StoreError::MigrationHistoryMismatch` AND assert that
  `user_version` is still 2 (i.e., no further migrations were applied
  past the diverged history).
- `shape_check_runs_before_apply` — apply migrations 1..=2; manually
  `DROP TRIGGER records_fts_au`; call `open()`. Assert
  `StoreError::SchemaDrift` and that `user_version` is still 2 — the
  pre-migration shape check rejects drift before `to_latest` mutates
  the vault.
- `replay_sequence_rewind_rejected` — insert into `wal_ops` then
  `used` with `(issuer='alice', sequence=10)`; expect success and
  `issuer_seq.high_water = 10`. Insert another wal_ops + used row with
  `(issuer='alice', sequence=5)`; expect ABORT via
  `used_sequence_must_advance`. Insert with `sequence=11`; expect
  success and `high_water = 11`.
- `lock_holder_count_stays_in_sync` — insert two `lock_holders` rows
  for the same scope; assert `locks.holder_count = 2`. Delete one;
  assert `holder_count = 1`. Delete the other; assert `holder_count = 0`.
- `replay_ledger_update_blocked` — insert into `used`; attempt
  `UPDATE used SET sequence = sequence + 1` and `UPDATE used SET
  issuer = 'mallory'`; both ABORT via `used_immutable`. Attempt
  `UPDATE issuer_seq SET high_water = high_water - 1`; ABORT via
  `issuer_seq_only_via_ledger` (rewind branch). Attempt
  `UPDATE issuer_seq SET high_water = 999999` for an issuer with no
  `used` row at sequence 999999; ABORT via `issuer_seq_only_via_ledger`
  (no-matching-ledger branch). Attempt `DELETE FROM used WHERE
  operation_id = '…'`; ABORT via `used_no_delete`. Attempt
  `DELETE FROM issuer_seq WHERE issuer = '…'`; ABORT via
  `issuer_seq_no_delete`.
- `wal_ops_envelope_locked` — insert a `wal_ops` row; UPDATE the
  `state` column from `'ISSUED'` to `'PREPARED'`; expect success
  (state mutable). UPDATE `updated_at`; expect success. UPDATE
  `reason`; expect success. UPDATE `target_hash`, `scope_json`,
  `signature`, `principal`, `expires_at`, `plan_ref`, `envelope`,
  `operation_id`, `issued_seq`, `kind`, `issuer`, `issued_at`; each
  one ABORTs via `wal_ops_envelope_immutable`.
- `wal_ops_fsm_transitions` — insert at `state='ISSUED'`. Legal:
  ISSUED→PREPARED, PREPARED→COMMITTED, ISSUED→REJECTED,
  PREPARED→ABORTED. Illegal (each ABORTs via
  `wal_ops_state_transition`): COMMITTED→ISSUED,
  COMMITTED→PREPARED, ABORTED→COMMITTED, REJECTED→PREPARED,
  PREPARED→ISSUED, COMMITTED→ABORTED.
- `wal_ops_terminal_fully_locked` — drive a row to `'COMMITTED'`;
  attempt `UPDATE wal_ops SET reason = 'note'`; expect ABORT via
  `wal_ops_terminal_immutable`. Repeat for `updated_at`. Repeat for
  rows in `'ABORTED'` and `'REJECTED'`. Confirms terminal rows are
  fully append-only; the audit ledger cannot be rewritten after
  the fact.
- `used_issuer_must_match_wal_ops` — insert a `wal_ops` row with
  `issuer = 'alice'`; attempt `INSERT INTO used (operation_id = …,
  issuer = 'mallory', …)` referencing alice's op; expect ABORT via
  `used_issuer_matches_wal`. Then re-attempt with
  `issuer = 'alice'`; expect success.
- `reader_fence_orphan_rejected` — attempt `INSERT INTO reader_fence
  (..., op_id = 'no-such-op', ...)`; expect FK violation at COMMIT.
- `reader_fence_supports_repeat_forget` — open a fence
  `(scope='session:S', op_id=A, state='tombstoning')`; close it.
  Insert a second fence `(scope='session:S', op_id=B,
  state='tombstoning')`; expect success (PK now includes op_id).
  Confirm read plans see only the one live tombstoning row via the
  partial unique index. Closes the previous round's
  single-use-per-scope failure mode.
- `reader_fence_clears_on_op_abort` — insert a `wal_ops` row with
  `state = 'PREPARED'`; insert a `reader_fence` row referencing it
  with `state = 'tombstoning'`. UPDATE `wal_ops.state = 'ABORTED'`;
  confirm the AFTER UPDATE trigger DELETE'd the fence row. Repeat
  for `'REJECTED'`. With `state = 'COMMITTED'` and a `closed` fence,
  confirm the fence is preserved (only failure paths auto-clear).
- `reader_fence_integrity_locked` — insert a fence in
  `'tombstoning'`. Attempt UPDATE on `scope_kind`, `scope_key`,
  `op_id`, `opened_at`; each ABORTs. Attempt UPDATE state from
  `'closed'` back to `'tombstoning'`; ABORT. Attempt direct
  `DELETE FROM reader_fence WHERE …` while the owning op is still
  PREPARED; ABORT via `reader_fence_no_direct_delete`. Then UPDATE
  the wal_ops row to `'ABORTED'`; the cascade trigger DELETEs the
  fence successfully (the predicate exemption).
- `wal_steps_delete_blocked` — insert a `wal_steps` row; attempt
  `DELETE FROM wal_steps WHERE …`; ABORT via `wal_steps_no_delete`.
- `replay_ledger_consistency_check` — exercise all four corruption
  branches:
  - **Stale:** insert wal_ops + used at `(alice, seq=42)`; manually
    set `issuer_seq.high_water = 5`; reopen; expect
    `ReplayLedgerInconsistent { kind: Stale { used_max: 42,
    high_water: 5 } }`.
  - **OverAdvanced:** same setup but set
    `issuer_seq.high_water = 9999`; reopen; expect
    `ReplayLedgerInconsistent { kind: OverAdvanced { used_max: 42,
    high_water: 9999 } }`.
  - **OrphanedHighWater:** insert just `issuer_seq (mallory, 100)`
    via fixture helper (bypassing the match-ledger trigger), no
    `used` rows for mallory; reopen; expect
    `ReplayLedgerInconsistent { kind: OrphanedHighWater {
    high_water: 100 } }`.
  - **MissingHighWater:** insert wal_ops + used for `(bob, 7)`,
    then DELETE the `issuer_seq` row for bob via fixture (bypassing
    the no-delete trigger); reopen; expect
    `ReplayLedgerInconsistent { kind: MissingHighWater { used_max: 7 } }`.
- `open_does_not_mutate_invalid_vault` — generate a fixture vault
  whose `schema_migrations` row 1 has a wrong `sql_blake3` (set via
  the test fixture helper that bypasses the immutability trigger);
  capture the file's mtime. Call `open()`; expect
  `MigrationHistoryMismatch`. Confirm the file's mtime is unchanged
  AND no `-wal`/`-shm` side files exist next to it. Verifies the
  read-only-pragmas-first ordering.
- `wal_steps_fsm_transitions` — insert at `state='PENDING'`. Legal:
  PENDING→DONE, PENDING→FAILED, FAILED→PENDING (retry),
  FAILED→COMPENSATED, DONE→COMPENSATED. Illegal: DONE→PENDING,
  COMPENSATED→PENDING, COMPENSATED→DONE, PENDING→COMPENSATED.
- `wal_steps_identity_locked` — insert a `wal_steps` row; UPDATE
  `state` from `'PENDING'` to `'DONE'`; expect success. UPDATE
  `attempts`, `last_error`, `started_at`, `finished_at`, `pre_image`;
  each succeeds. UPDATE `operation_id`, `step_ord`, or `step_kind`;
  each ABORTs.
- `schema_migrations_tamper_blocked` — apply migrations to head;
  attempt `DELETE FROM schema_migrations WHERE migration_id = 3`;
  expect ABORT via `schema_migrations_no_delete`. Attempt
  `UPDATE schema_migrations SET sql_blake3 = '00…' WHERE migration_id
  = 3`; expect ABORT via `schema_migrations_immutable`.
- `migration_history_rejects_gap` — apply migrations to head; bypass
  the trigger by temporarily disabling it via a fixture helper
  (test-only) to simulate a tampered vault from another binary;
  delete row 3; reopen via `open()`; expect
  `StoreError::MigrationHistoryMismatch` (contiguity branch). Repeat
  with a duplicated migration_id row.
- `supersession_is_durable_until_src_tombstoned` — insert R1
  (target T1, active=1, tombstoned=0) and R2 (target T2, active=1,
  tombstoned=0); insert valid `updates` edge `(src=R1, dst=R2)`.
  Confirm R2 is excluded from `records_latest`. Set R1.active=0;
  R2 stays excluded (the supersede edge is still live). Now
  `UPDATE records SET tombstoned = 1 WHERE record_id = R1.record_id`;
  the AFTER UPDATE trigger atomically deletes the
  `(R1, R2, 'updates')` edge in the same statement, and R2 reappears
  in `records_latest`. Confirms (a) supersession persists across
  ordinary lifecycle changes (active flip), (b) tombstoning the
  source atomically clears the supersession, (c) the read view
  cannot observe the inconsistent state.
- `updates_edge_requires_non_tombstoned_endpoints` — insert R1 with
  `tombstoned=1`, R2 live; attempt `INSERT INTO edges (src=R1, dst=R2,
  kind='updates')`; expect ABORT via the supersede trigger
  (tombstoned-src predicate). Same with tombstoned dst.
- `issuer_seq_direct_insert_rejected` — attempt `INSERT INTO
  issuer_seq (issuer='mallory', high_water=99999999)` with no matching
  `used` row; expect ABORT via `issuer_seq_insert_must_match_ledger`.
  Insert a wal_ops + used row with `(issuer='alice', sequence=42)`;
  confirm `issuer_seq` now has `(alice, 42)` (auto-populated by the
  AFTER INSERT trigger on `used`, which is the legitimate path).
- `wal_ops_delete_blocked` — insert a `wal_ops` row in COMMITTED
  state; attempt `DELETE FROM wal_ops WHERE operation_id = '…'`;
  expect ABORT via `wal_ops_no_delete`. Confirm child rows in
  `wal_steps` and `wal_op_deps` are still present (the cascade never
  fired because the parent delete was rejected).
- `updates_edge_records_first_contract` — within one
  `BEGIN IMMEDIATE … COMMIT`, attempt `INSERT INTO edges (..., kind =
  'updates')` BEFORE inserting the `records` row referenced by
  `NEW.src`; expect ABORT via `edges_updates_supersede_insert`. Then
  retry with records-first ordering inside the same transaction
  pattern; expect success. Documents the records-before-edges contract
  for `updates` edges.
- `wal_op_deps_delete_blocked` — insert two `wal_ops` rows and a
  valid `wal_op_deps` edge between them. Attempt
  `DELETE FROM wal_op_deps WHERE …`; expect ABORT via
  `wal_op_deps_no_delete`.
- `consent_journal_append_only` — insert a `consent_journal` row;
  attempt `UPDATE consent_journal SET payload = '{}' WHERE row_id = …`;
  expect ABORT via `consent_journal_immutable`. Attempt
  `DELETE FROM consent_journal WHERE row_id = …`; expect ABORT via
  `consent_journal_no_delete`.
- `lock_acquisition_round_trip` — start with no `locks` row;
  insert a `locks` row in `mode='free', holder_count=0`. Insert
  one `lock_holders` row with `mode_requested='exclusive'`; the
  sync triggers drive `locks` to `(exclusive, 1)`. Insert a second
  exclusive holder for the same scope; expect ABORT via
  `lock_holders_exclusive_only_alone`. Release the first holder;
  `locks` returns to `(free, 0)`. Insert two `mode_requested='shared'`
  holders; `locks` becomes `(shared, 2)`. Try inserting an exclusive
  while shared holders exist; ABORT. Release both shared; `(free, 0)`.
- `lock_state_invariants_enforced` — attempt
  `UPDATE locks SET holder_count = 5 WHERE mode = 'exclusive'`;
  expect CHECK violation. Attempt `UPDATE locks SET mode = 'free'
  WHERE holder_count > 0`; expect CHECK violation. Confirms the
  CHECK still rejects manual tampering even though the normal write
  path is trigger-driven.
- `lock_exclusivity_enforced_by_schema` — insert a shared holder;
  then attempt to INSERT an exclusive holder for the same scope;
  ABORT via `lock_holders_exclusive_only_alone`. Insert an
  exclusive holder; attempt to INSERT a shared holder for the same
  scope; ABORT via `lock_holders_shared_blocked_by_exclusive`.
  Schema enforces mutual exclusion independently of caller logic.
- `lock_holders_keys_locked` — insert a holder; attempt
  `UPDATE lock_holders SET scope_key = '…' WHERE …`; ABORT.
  Repeats for `scope_kind`, `holder_id`, `acquired_epoch`.
- `wal_op_deps_uses_issued_seq` — insert two `wal_ops` rows in two
  separate transactions; the second has a strictly larger
  `issued_seq`. Insert the legitimate dep `(later, earlier)`; expect
  success. Attempt `(earlier, later)`; expect ABORT (not based on
  ULID order — based on issued_seq). Attempt
  `INSERT INTO wal_ops (..., issued_seq=1, ...)` after a row with
  `issued_seq=5` exists; expect ABORT via
  `wal_ops_issued_seq_must_advance`. Attempt to UPDATE an existing
  `wal_op_deps` row; expect ABORT via `wal_op_deps_immutable`.

### 7.3 Snapshot tests (`insta`)

A single test dumps `sqlite_master` (sorted by `type, name`) after applying
all migrations, and snapshots it. Reviewers see schema deltas in PR diffs.
Snapshot lives at `crates/cairn-store-sqlite/tests/snapshots/migrations__schema.snap`.

## 8. Verification mapping (issue's acceptance criteria)

| AC | How it's verified |
|----|--------------------|
| Fresh vault opens with all P0 tables and pragmas | `fresh_vault_opens_to_head` + `pragmas_applied` |
| Migration history is visible and fails on checksum mismatch | `schema_migrations` ledger records each applied migration's BLAKE3 hash; `verify_migration_history()` runs at every open and `migration_history_mismatch_rejected` proves it rejects edited history. `forward_only_rejects_future_schema` catches future-version skew, `same_version_drift_is_rejected` catches same-version state drift. |
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
