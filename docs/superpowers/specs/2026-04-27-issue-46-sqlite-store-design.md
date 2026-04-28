# Issue #46 — SQLite `MemoryStore` CRUD, versioning, and graph edges

**Status:** draft
**Date:** 2026-04-27
**Issue:** [#46](https://github.com/windoliver/cairn/issues/46) (parent: #6)
**Brief sections:** §3.0 Storage topology · §4 MemoryStore contract · §5.1 Read path · §5.2 Write path · §5.6 WAL state machine · brief lines 337-369 (records schema), 1624 (read-path layering), 2030 (forget_record)

## 1. Goal

Land the P0 `MemoryStore` adapter primitives: typed read API, copy-on-write
versioned record writes, graph edge operations, and a synchronous
transaction surface gated by a WAL-only **apply token** — all backed by
`rusqlite` (`bundled`) against `.cairn/cairn.db`. Migrations covering the
P0 schema (records, edges, FTS5, WAL ops/steps, replay ledger, consent
journal, locks, jobs) ship in the same PR; #45 was closed without producing
migrations, so #46 absorbs that scope.

These adapter methods are the **physical-apply primitives** the WAL state
machine (§5.6, lands in #8) will orchestrate. They are not callable
directly from verbs in #46. Tests + the future state machine are the only
callers.

**Out of scope.** WAL state-machine logic (§5.6 — #8). Phase A drain →
Phase B physical purge orchestration (#8). Ranking, semantic embedding
generation, hybrid search orchestration. The `sqlite-vec` virtual table is
not created in #46 — it lands with embeddings in #48. The full ReBAC rule
set + `Principal` resolution lives in `cairn-core::rebac` (separate
issue); #46 ships only the read-trait surface that *takes* a `&Principal`
and the SQL-level filter helpers — the rebac decision function is wired
in by its owning issue. For #46, "rebac evaluation" means: filter rows
by `scope.visibility` ∈ allowed-tiers and `scope.user`/`scope.agent`
match against the principal. Anything richer is a follow-up.

## 2. Context

- Brief §4 row 1 fixes `MemoryStore` as the storage contract; P0 default is
  pure SQLite + FTS5 with `sqlite-vec`.
- **Brief lines 337-369** define the schema: `records` is a single
  copy-on-write table keyed per-version by `record_id = BLAKE3(target_id ||
  '#' || version)`, with stable `target_id`, monotonic `version`, `active`
  flag, and `tombstoned` flag. Exactly one active row per `target_id`
  enforced by partial unique index. There is no separate history table —
  every superseded version remains in `records` with `active=0`.
- **CLAUDE.md invariant 5** — "WAL + two-phase apply for every mutation.
  Every write goes through the WAL state machine. No direct DB mutations."
  #46 ships the *primitives* the state machine will call. To enforce this
  at the type level, write methods take an `ApplyToken` whose constructor
  is `pub(in cairn_core::wal)` and unreachable from outside `cairn-core`'s
  WAL module (which lands in #8).
- **Visibility / rebac is enforced at the `MemoryStore` layer.** Brief
  lines 2557 (`retrieve --scope` "rebac applied per-row at MemoryStore
  layer"), 3287 ("Filter lives at the store layer; caller never sees
  leaked rows"), and 4136 ("results the caller can't read are dropped
  at the MemoryStore layer, never surfaced") fix this. Brief line 1624
  ("the harness never reaches the store directly") describes the
  *invocation flow* through Scope Resolve and Rank & Filter — Scope
  Resolve produces a typed query carrying the principal, the store
  evaluates rebac per row, and Rank & Filter further trims. Store-layer
  enforcement is the dominant signal; read methods therefore take a
  `&Principal` and the store drops non-readable rows pre-return.
- Existing `cairn-store-sqlite` is a stub. No `rusqlite` dep yet
  (intentionally deferred per `Cargo.toml` comment).
- `MemoryRecord`, `RecordId`, `Provenance`, `ActorChain`, `Evidence`,
  `Scope`, taxonomy types already live in `cairn-core::domain`.
- WAL state machine (§5.6) is not in scope; we ship the `wal_ops`/
  `wal_steps` tables only.

## 3. Architecture

```
cairn-core
├── contract::memory_store
│   ├── MemoryStore trait (READ-ONLY public surface; dyn-registered)
│   ├── MemoryStoreCapabilities, ListQuery, ChangeKind, Edge, EdgeKind
│   ├── StoreError (abstract; backend variant boxes adapter errors)
│   └── apply
│       ├── ApplyToken                  (zero-sized; ctor pub(in crate::wal))
│       ├── MemoryStoreApply trait      (sealed; with_apply_tx entrypoint)
│       └── MemoryStoreApplyTx trait    (sealed; sync write methods)
└── wal                                 (#8 — for #46 we only place a
                                          stub mod with the ApplyToken
                                          constructor; the executor lands in #8)

cairn-store-sqlite
├── migrations/                    (embedded via include_str!)
│   ├── 0001_init_pragmas.sql
│   ├── 0002_records.sql           (versioned COW per brief lines 337-369)
│   ├── 0003_edges.sql
│   ├── 0004_fts5.sql
│   ├── 0005_wal_state.sql
│   ├── 0006_replay_consent.sql
│   ├── 0007_locks_jobs.sql
│   └── 0008_meta.sql
└── src/
    ├── lib.rs                     (existing register_plugin! + extended impls)
    ├── error.rs                   (rusqlite-aware SqliteStoreError + From for core)
    ├── schema/
    │   ├── mod.rs                 (Migration struct, &'static [Migration])
    │   └── runner.rs              (apply_pending, checksum verify)
    ├── conn.rs                    (open(), pragma setup)
    ├── store.rs                   (SqliteMemoryStore impl on MemoryStore — reads)
    └── apply.rs                   (SqliteMemoryStoreApply / Tx — writes)
```

**Concurrency.** `rusqlite::Connection` is `!Sync`. The store wraps it in
`tokio::sync::Mutex<Connection>` and dispatches every operation to
`tokio::task::spawn_blocking`. SQLite is set to WAL journal mode so
concurrent readers don't block on writers. P0 single-author model means
contention is minimal; one process owns the file.

## 4. Trait surface

### 4.1 `MemoryStore` (read-only public contract)

```rust
#[async_trait::async_trait]
pub trait MemoryStore: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> &MemoryStoreCapabilities;
    fn supported_contract_versions(&self) -> VersionRange;

    /// Read the active version of a logical record by stable `target_id`,
    /// gated by rebac against `principal`. Returns `None` if no active
    /// version exists, the active row is tombstoned/expired, OR the
    /// principal cannot read it (the three are indistinguishable to the
    /// caller — brief line 4136 mandates hidden rows never surface).
    async fn get(
        &self,
        principal: &Principal,
        target_id: &TargetId,
    ) -> Result<Option<MemoryRecord>, StoreError>;

    /// Range/list query, gated by rebac against `query.principal`. The
    /// query carries pre-resolved scope filters (target_id prefix, kind,
    /// tier, time bounds); the store evaluates each candidate row's
    /// `scope` + `actor_chain` against the principal and drops
    /// non-readable rows before returning (brief line 3287). Returns the
    /// (visible) rows; the count of dropped rows is surfaced via
    /// `ListResult::hidden` per brief line 4136 (`results_hidden: N`).
    async fn list(&self, query: &ListQuery) -> Result<ListResult, StoreError>;

    /// Full lifecycle history for a logical `target_id`, gated by rebac
    /// against `principal`. Returns concrete `Version` entries first
    /// (ordered by `version` ASC, including superseded/tombstoned/expired
    /// rows the principal can read), then any `Purge` markers from
    /// `record_purges`. Used by audit/forensic paths and by the WAL
    /// executor when computing pre-images. The WAL executor passes a
    /// system principal that bypasses scope filtering. See §5.4.
    async fn version_history(
        &self,
        principal: &Principal,
        target_id: &TargetId,
    ) -> Result<Vec<HistoryEntry>, StoreError>;
}
```

`MemoryStore` is the trait that `register_plugin!` stores as
`Box<dyn MemoryStore>`. **It exposes no write methods.** Any in-process
caller holding `dyn MemoryStore` can only read, and only what the
supplied `Principal` is permitted to see.

`get`/`list` return only rows where `active = 1 AND tombstoned = 0 AND
(expired_at IS NULL OR expired_at > now)` AND the principal passes the
rebac predicate. `ListQuery` admin toggles expose
superseded/tombstoned/expired rows for forensic and WAL recovery paths
— but only ever rows the principal can read.

`Principal` (defined in `cairn-core::domain::identity`, already
present) carries the actor identity and resolved scope tuple. A
`Principal::system()` constructor returns a privileged principal that
bypasses rebac — used by the WAL executor and tests. Brief line 1361
flags this kind of unverified read with a `trust: "unverified"` marker
in the response; the store passes the principal mode through unchanged
and lets the verb layer surface the trust marker.

### 4.2 Apply (write) surface — sealed, token-gated

```rust
pub mod apply {
    /// Witness that the caller is the WAL executor. Constructable only
    /// inside `cairn_core::wal`. No public API can produce one.
    pub struct ApplyToken { _private: () }

    impl ApplyToken {
        // Crate-internal — only WAL state-machine code can call this.
        // For #46, the WAL module is a stub that exposes this only to a
        // single private function used by tests; #8 wires it into the
        // executor.
        pub(in crate::wal) fn new() -> Self { Self { _private: () } }
    }

    pub trait MemoryStoreApply: sealed::Sealed + Send + Sync {
        /// Run a synchronous closure inside one rusqlite transaction.
        /// Only callers with an `ApplyToken` (i.e. the WAL executor) can
        /// invoke. The closure is `FnOnce(&mut dyn MemoryStoreApplyTx)
        /// -> Result<T>`; commit on `Ok`, rollback on `Err` or panic.
        fn with_apply_tx<F, T>(
            &self,
            _: ApplyToken,
            f: F,
        ) -> Result<T, StoreError>
        where
            F: FnOnce(&mut dyn MemoryStoreApplyTx) -> Result<T, StoreError>
                + Send + 'static,
            T: Send + 'static;
    }

    pub trait MemoryStoreApplyTx: sealed::Sealed + Send {
        /// Stage a new version (active=0) of a record. The supplied
        /// `MemoryRecord` carries `target_id` and the staged `version`
        /// (caller computes `version = max(existing) + 1`). The deterministic
        /// per-version `record_id = BLAKE3(target_id || '#' || version)`
        /// is computed inside this method.
        fn stage_version(&mut self, record: &MemoryRecord)
            -> Result<RecordId, StoreError>;

        /// Atomically flip `active` so exactly one version of the target
        /// is active. The `primary.activate` step in §5.6.
        fn activate_version(
            &mut self, target_id: &TargetId, version: u64,
        ) -> Result<(), StoreError>;

        /// Set `tombstoned = 1` on **every version** of `target_id`. This
        /// is Phase A of forget_record (brief line 2030). Idempotent.
        fn tombstone_target(
            &mut self, target_id: &TargetId, actor: &ActorRef,
        ) -> Result<(), StoreError>;

        /// Set `expired_at` on the active version. Subsequent reads
        /// filter it out unless `include_expired` is set.
        fn expire_active(
            &mut self, target_id: &TargetId, at: Timestamp,
        ) -> Result<(), StoreError>;

        /// Phase B primitive. In one tx: write a `record_purges` audit
        /// marker keyed by `(target_id, op_id)` (idempotent — repeat with
        /// the same `op_id` is a no-op), then DELETE every row with
        /// `target_id` plus matching `edges`/`edge_versions`/`records_fts`
        /// rows. The brief audit invariant (line 2030) requires no body
        /// or edge survives; the marker carries no body, only audit
        /// metadata. This method does not touch `consent_journal` or
        /// `wal_ops` — those are the executor's job.
        fn purge_target(
            &mut self,
            target_id: &TargetId,
            op_id: &OpId,
            actor: &ActorRef,
        ) -> Result<PurgeOutcome, StoreError>;

        fn add_edge(&mut self, edge: &Edge) -> Result<(), StoreError>;
        fn remove_edge(
            &mut self,
            from: &RecordId, to: &RecordId, kind: EdgeKind,
        ) -> Result<(), StoreError>;
    }

    mod sealed { pub trait Sealed {} }
}
```

`MemoryStoreApply` is **not** registered with `register_plugin!`. The
plugin host has no path to a write-capable view of the store. The WAL
executor in #8 obtains it via a typed accessor (`SqliteMemoryStore::apply`
returning `&dyn MemoryStoreApply`) that requires the executor to already
hold an `ApplyToken` — practically, the executor module is the only
construction site.

For tests, a `cfg(any(test, feature = "test-util"))` shim in
`cairn-core::wal` exposes a `test_apply_token()` constructor. The feature
is dev-only and never default-enabled in production builds.

### 4.3 Tx execution model

`with_apply_tx` runs entirely inside one `tokio::task::spawn_blocking`:

1. Acquire connection mutex.
2. Begin a `rusqlite::Transaction`.
3. Construct `SqliteMemoryStoreApplyTx<'_>` wrapping the tx handle.
4. Call the closure synchronously.
5. Commit on `Ok(t)`; rollback on `Err(e)`.
6. On panic: `rusqlite::Transaction`'s drop guard rolls back automatically;
   `spawn_blocking` surfaces the panic as a `JoinError` mapped to
   `StoreError::Backend`.

Because `MemoryStoreApplyTx` methods are sync and the closure is sync,
the entire tx executes on one thread holding one connection — atomic by
construction.

## 5. Data model

### 5.1 Tables (P0)

| Table | Purpose |
|---|---|
| `records` | **Versioned copy-on-write** table per brief lines 337-369. PK is per-version `record_id`. `target_id` + `version` indexed; partial unique index `WHERE active = 1` enforces one active version per target. Mutable lifecycle columns: `active`, `tombstoned`, `tombstoned_at`, `tombstoned_by`, `expired_at`. JSON columns for provenance, actor_chain, evidence, taxonomy, scope. |
| `record_purges` | **Append-only purge audit log.** One row per `purge_target` invocation. Persists a metadata-only marker (`target_id`, `op_id`, `purged_at`, `purged_by`, `body_hash_salt`) so `version_history(target_id)` can surface a purge after every `records` row is gone. Survives the physical delete; consumed by audit/forensic verbs. |
| `edges` | `from_id`, `to_id`, `kind`, `weight`, `metadata` JSON. Composite PK `(from_id, to_id, kind)`. `from_id`/`to_id` reference per-version `record_id` (so edges carry version semantics like vector/FTS rows do per §5.6). |
| `edge_versions` | Append-only edge history (insert/update/remove markers). |
| `records_fts` | FTS5 contentless table indexing every version (active or not). The read predicate matches `get`/`list` exactly: `active = 1 AND tombstoned = 0 AND (expired_at IS NULL OR expired_at > now)`. Superseded **and expired** rows are filtered identically. |
| `wal_ops`, `wal_steps` | WAL state-machine tables (rows added in #8). |
| `replay_ledger`, `issuer_seq`, `challenges` | Identity/replay surface (rows added in #7). |
| `consent_journal` | Append-only consent log (rows added by the WAL executor in #8 and the consent UX in #17). |
| `locks`, `reader_fence` | Per-vault advisory locks + reader fences for session-scoped forget. |
| `jobs` | Workflow host queue. |
| `schema_migrations` | `(id INTEGER PK, name TEXT, checksum TEXT, applied_at TEXT)`. |

The `records_vec` virtual table is not in #46. It lands with #48.

### 5.2 `records` schema (concrete)

Aligned with brief lines 341-369:

```sql
CREATE TABLE records (
  record_id      TEXT NOT NULL PRIMARY KEY,        -- BLAKE3(target_id || '#' || version)
  target_id      TEXT NOT NULL,                    -- stable logical identity
  version        INTEGER NOT NULL,                 -- monotonic per target_id
  active         INTEGER NOT NULL DEFAULT 0,       -- 1 for currently visible
  tombstoned     INTEGER NOT NULL DEFAULT 0,       -- set on every version during forget
  -- lifecycle audit columns (populated once each, never overwritten)
  created_at     TEXT NOT NULL,                    -- ISO-8601; set on stage_version
  created_by     TEXT NOT NULL,                    -- ActorRef of stage_version caller
  tombstoned_at  TEXT,                             -- ISO-8601; set once on tombstone_target
  tombstoned_by  TEXT,                             -- ActorRef of tombstone_target caller
  expired_at     TEXT,                             -- ISO-8601; set once on expire_active
  -- payload + metadata
  body           TEXT NOT NULL,
  provenance     TEXT NOT NULL,                    -- JSON
  actor_chain    TEXT NOT NULL,                    -- JSON
  evidence       TEXT NOT NULL,                    -- JSON
  scope          TEXT NOT NULL,                    -- JSON
  taxonomy       TEXT NOT NULL,                    -- JSON
  confidence     REAL NOT NULL,
  salience       REAL NOT NULL,
  UNIQUE (target_id, version)
) STRICT;

-- Append-only audit marker table, keyed by (target_id, op_id) for
-- idempotency. Survives every records DELETE issued by purge_target.
CREATE TABLE record_purges (
  target_id        TEXT NOT NULL,
  op_id            TEXT NOT NULL,
  purged_at        TEXT NOT NULL,
  purged_by        TEXT NOT NULL,                  -- ActorRef
  body_hash_salt   TEXT NOT NULL,
  PRIMARY KEY (target_id, op_id)
) STRICT;

CREATE UNIQUE INDEX records_active_target_idx
  ON records(target_id) WHERE active = 1;

CREATE INDEX records_target_idx ON records(target_id);
```

### 5.3 Write semantics (#46 primitives, called by #8 state machine)

- `stage_version(record)` — INSERT a row with `active=0`, version computed
  by caller (typically `max(existing) + 1`). The PK collision on
  `(target_id, version)` is the brief's "second idempotency key" (line
  364): a retry cannot stage version N+1 twice. On `UNIQUE` violation,
  return `StoreError::Conflict { kind: VersionAlreadyStaged }`.
- `activate_version(target_id, version, expected_prior: Option<u64>)`
  — monotonic compare-and-swap:
  ```sql
  -- Step 1: confirm the target/version row exists.
  SELECT 1 FROM records WHERE target_id = ?1 AND version = ?2;
  --   missing → return StoreError::NotFound (no UPDATE issued).
  -- Step 2: monotonicity guard. Read the current active version.
  SELECT version FROM records WHERE target_id = ?1 AND active = 1;
  --   if expected_prior is Some(v) and current != v
  --     → return StoreError::Conflict { kind: ActivationRaced }.
  --   if current >= ?2 (i.e. requested version is not strictly newer)
  --     → return StoreError::Conflict { kind: ActivationRaced }.
  -- Step 3: flip flags atomically.
  UPDATE records SET active = (version = ?2) WHERE target_id = ?1;
  -- Step 4: assert exactly one row is now active for the target.
  SELECT COUNT(*) FROM records WHERE target_id = ?1 AND active = 1;
  --   != 1 → rollback + return StoreError::Invariant("activate_version: no row activated")
  ```
  `expected_prior=None` is allowed only on first activation (no current
  active row). For all subsequent activations, the WAL executor passes
  the version it read at op-stage time; a stale or duplicated apply
  fails closed rather than silently rolling readers back to v2 after v3
  is already active. Total-or-error, monotonic, retry-safe.

  The brief pairs activation with a `consent_journal` insert in the
  same transaction. See §5.6 below for how the store exposes the
  consent-row write to the executor.
- `tombstone_target(target_id, actor)` — sets `tombstoned=1` on every
  version of the target. Idempotent: re-tombstoning is a no-op. The
  `actor` is recorded into `tombstoned_by`/`tombstoned_at` columns
  (added to schema; default NULL pre-tombstone).
- `expire_active(target_id, at)` — sets `expired_at` on the active
  version only. Reads filter on `expired_at IS NULL OR expired_at > now()`.
- `purge_target(target_id, op_id, actor)` — single tx, ordered to keep
  the per-version `record_id` set discoverable until edges/FTS are
  drained:
  1. **Capture** the per-version key set:
     `SELECT record_id FROM records WHERE target_id = ?1` → `Vec<RecordId>`.
     Used by all subsequent deletes; never re-derived after `records` is
     emptied.
  2. INSERT a metadata-only marker into `record_purges` with
     `(target_id, op_id, purged_at=now(), purged_by=actor,
      body_hash_salt=random())`. Idempotency key is `(target_id, op_id)`.
     If the marker already exists for this `op_id`, return
     `PurgeOutcome::AlreadyPurged` and commit a no-op tx.
  3. DELETE from `edges` and `edge_versions` where `from_id` or `to_id`
     is in the captured set.
  4. DELETE from `records_fts` where the row's `record_id` is in the
     captured set.
  5. DELETE from `records` where `target_id = ?1` — the body delete is
     last so prior steps still see the per-version keys.
  6. Return `PurgeOutcome::Purged`.

  Capture-first ordering closes the audit invariant: no edge or FTS
  row referencing a purged `record_id` can survive, even though the
  primary delete needs to happen for the brief's "no body in
  `cairn.db`" invariant. Pre-image zeroing in `wal_ops`/`wal_steps`
  is the executor's job; the store does not know op-step linkage.

### 5.4 `version_history` semantics

Signature:

```rust
async fn version_history(&self, target_id: &TargetId)
    -> Result<Vec<HistoryEntry>, StoreError>;
```

`HistoryEntry` is an enum so a purged target — which has no `record_id`
because every `records` row is gone — has its own variant rather than a
sentinel `RecordVersion`:

```rust
#[non_exhaustive]
pub enum HistoryEntry {
    /// One concrete version of the record. Sourced from a `records` row
    /// that still exists.
    Version(RecordVersion),
    /// A purge audit marker. Sourced from `record_purges`. No body, no
    /// `record_id` — only `target_id`, `op_id`, and the purge event.
    Purge(PurgeMarker),
}

pub struct RecordVersion {
    pub record_id: RecordId,        // per-version, always present
    pub target_id: TargetId,
    pub version: u64,
    pub active: bool,
    pub events: Vec<RecordEvent>,   // ordered by event timestamp ASC
}

pub struct PurgeMarker {
    pub target_id: TargetId,
    pub op_id: OpId,
    pub event: RecordEvent,         // kind = Purge
    pub body_hash_salt: String,
}

pub struct RecordEvent {
    pub kind: ChangeKind,           // Update | Tombstone | Expire | Purge
    pub at: Timestamp,
    pub actor: Option<ActorRef>,
}
```

Order: all `Version` entries first, sorted by `version` ASC; then any
`Purge` entries from `record_purges` for the target, sorted by
`purged_at` ASC.

Per-version lifecycle events are sourced from immutable row columns:

- `events[0]` always exists: `kind=Update, at=created_at,
  actor=Some(created_by)` — the version's birth.
- If `tombstoned_at` is non-NULL: append `kind=Tombstone,
  at=tombstoned_at, actor=Some(tombstoned_by)`. Because `tombstoned_at`
  is immutable (set once on Phase A and never overwritten), tombstoning
  a target with multiple versions does **not** rewrite earlier `Update`
  events — every version still reports its birth `Update` and a later
  `Tombstone` event at the tombstone time.
- If `expired_at` is non-NULL on the active version: append `kind=Expire,
  at=expired_at, actor=None`.

For purged targets, `version_history` returns one or more
`HistoryEntry::Purge` entries (one per `record_purges` row — every retry
under a different `op_id` becomes its own marker; same `op_id` retries
are coalesced by the PK). The body is gone but the audit fact survives.

`ChangeKind::Purge` is an emitted variant in #46 (sourced from
`record_purges` rows), not a reserved-only variant.

### 5.5 Edge semantics

- `add_edge(edge)` — INSERT with conflict-update on `(from_id, to_id,
  kind)`; on update, copy prior to `edge_versions`.
- `remove_edge(from, to, kind)` — copy to `edge_versions` (`change_kind=
  remove`), DELETE row.
- `from_id` and `to_id` are **per-version** `record_id`s. Edges follow
  the same versioning model as vector/FTS rows (brief §5.6 line 2029).
- Tombstoning a record does **not** cascade-delete edges. Read paths
  filter via JOIN against `records.active = 1 AND tombstoned = 0`.
  Physical purge (in `purge_target`) removes them entirely.

### 5.6 Consent journal in the same transaction

Brief §5.6 (line 2029) requires the consent-journal row and the state
change (e.g. `primary.activate`, `primary.mark_tombstone`) to commit
in the **same** SQLite transaction so readers cannot observe a state
change without a matching journal row, and the journal cannot survive
a rolled-back state change.

`MemoryStoreApplyTx` therefore exposes one consent-journal primitive:

```rust
fn append_consent_journal(
    &mut self,
    entry: &ConsentJournalEntry,
) -> Result<ConsentJournalRowId, StoreError>;
```

The WAL executor calls this inside the same `with_apply_tx` closure that
performs the state change. The store does not write consent rows on its
own — every entry is the executor's choice — but it does provide the
in-tx insert path so the brief's atomicity invariant holds.

`ConsentJournalEntry` is a typed payload (op_id, kind, target_id,
actor, payload-hash, timestamp) defined in `cairn-core::domain`. The
store treats the JSON-serialized form as opaque: it round-trips bytes
through the table.

`MemoryStoreApplyTx`'s revised method list:

```rust
fn stage_version(&mut self, record: &MemoryRecord) -> Result<RecordId, StoreError>;
fn activate_version(
    &mut self,
    target_id: &TargetId,
    version: u64,
    expected_prior: Option<u64>,
) -> Result<(), StoreError>;
fn tombstone_target(
    &mut self, target_id: &TargetId, actor: &ActorRef,
) -> Result<(), StoreError>;
fn expire_active(
    &mut self, target_id: &TargetId, at: Timestamp,
) -> Result<(), StoreError>;
fn purge_target(
    &mut self, target_id: &TargetId, op_id: &OpId, actor: &ActorRef,
) -> Result<PurgeOutcome, StoreError>;
fn add_edge(&mut self, edge: &Edge) -> Result<(), StoreError>;
fn remove_edge(
    &mut self, from: &RecordId, to: &RecordId, kind: EdgeKind,
) -> Result<(), StoreError>;
fn append_consent_journal(
    &mut self, entry: &ConsentJournalEntry,
) -> Result<ConsentJournalRowId, StoreError>;
```

## 6. Error handling

`cairn-core::contract::memory_store::StoreError` is **abstract** (no
rusqlite dependency in core):

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    #[error("record not found: target_id={0}")]
    NotFound(TargetId),
    #[error("conflict: {kind:?}")]
    Conflict { kind: ConflictKind },
    #[error("invariant violated: {0}")]
    Invariant(&'static str),
    #[error("backend error")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("serialization: {0}")]
    Serde(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConflictKind {
    VersionAlreadyStaged,    // stage_version idempotency key violation
    UniqueViolation,         // generic SQLite UNIQUE
    ForeignKey,              // generic SQLite FK
}
```

`cairn-store-sqlite::error::SqliteStoreError` wraps `rusqlite::Error` and
implements `From<SqliteStoreError> for cairn_core::StoreError` via the
`Backend` variant. `Conflict` variants are detected from
`rusqlite::ErrorCode` and surfaced as the typed variant rather than
`Backend`.

## 7. Migrations

### 7.1 Runner

Hand-rolled (no `sqlx::migrate!` since driver is `rusqlite`). Embeds each
migration via `include_str!`. List is a `&'static [Migration]` ordered by
id and **identical across all build flavors** — no feature gates can
change which numbered migrations exist.

```rust
struct Migration {
    id: u32,
    name: &'static str,
    sql: &'static str,
    checksum: &'static str, // sha256 of sql, computed at build via build.rs
}
```

`apply_pending(conn)`:
1. Ensure `schema_migrations` table exists (run `0008_meta.sql`'s
   create-only prefix idempotently — bootstrap).
2. Read applied `(id, checksum)` set.
3. For each migration in list: if applied, verify checksum match → abort
   on mismatch; if not applied, execute in single sqlite tx, insert
   ledger row.
4. Forward-only — no down migrations.

### 7.2 `open()` flow

```rust
pub async fn open(path: &Path) -> Result<SqliteMemoryStore, StoreError> {
    // 1. tokio::task::spawn_blocking
    // 2. Connection::open(path)
    // 3. apply pragmas (WAL, foreign_keys, busy_timeout=5000, synchronous=NORMAL)
    // 4. apply_pending migrations
    // 5. wrap in tokio::sync::Mutex, return SqliteMemoryStore
}
```

No extension loading and no optional schema in #46. Basic CRUD has zero
dependency on `sqlite-vec`. The vec story is entirely deferred to #48.

### 7.3 Capability advertisement

Post-#46:
```rust
MemoryStoreCapabilities { fts: true, vector: false, graph_edges: true, transactions: true }
```

`vector` flips in #48 when the `records_vec` migration + embedding pipeline
land.

## 8. Testing

### 8.1 Unit (`cairn-store-sqlite/src/`)

- `schema::tests::checksum_stable` — checksums match across builds.
- `schema::tests::list_invariant` — same migration ids and ordering for
  any feature combination of the crate.
- `error::tests::rusqlite_unique_to_conflict` — `rusqlite::ErrorCode::ConstraintUnique`
  maps to `StoreError::Conflict { kind: UniqueViolation }`.
- `conn::tests::pragmas_applied` — open writes WAL, FK pragmas; verify
  via `PRAGMA` reads.

### 8.2 Integration (`cairn-store-sqlite/tests/`)

Tests use `cairn_core::wal::test_apply_token()` (test-util feature) to
mint an `ApplyToken` and exercise `MemoryStoreApply`.

| File | Coverage |
|---|---|
| `crud_roundtrip.rs` | `stage_version` → `activate_version` → `get` returns byte-equal `MemoryRecord` (full provenance, actor_chain, evidence, scope, confidence, salience). |
| `cow_versioning.rs` | Stage v1 → activate v1 → stage v2 → activate v2 → assert: (a) `version_history(target)` returns 2 rows; (b) `get(target)` returns v2's body; (c) partial unique index rejects a second active row; (d) FTS5 returns only v2 because read filter joins on `active=1`. |
| `tombstone_preserves_history.rs` | Stage v1 → activate → stage v2 → activate → tombstone target. Assert `version_history` returns 2 `HistoryEntry::Version` entries; v1's events are `[Update(at=v1.created_at), Tombstone(at=ts)]`; v2's events are `[Update(at=v2.created_at), Tombstone(at=ts)]`. **The earlier Update events are not rewritten.** `get` returns `None`. |
| `expire_active.rs` | Expire active version → `get` returns `None` after the expiry; `include_expired=true` surfaces it; FTS5 search for the body returns zero hits **without** `include_expired` (covers expiry fence in §5.1 records_fts row). |
| `activate_validates_existence.rs` | `activate_version(target, version=999)` for a non-existent version → returns `StoreError::NotFound`, no row's `active` flag changes (assert by reading post-error). Re-issue with the correct version → succeeds; exactly one active row. |
| `purge_audit_marker.rs` | Stage 3 versions + edges + FTS rows, then `purge_target(target, op_id, actor)` → assert zero rows in `records`/`edges`/`edge_versions`/`records_fts`; **assert one row in `record_purges`** with `purged_by=actor`; `version_history(target)` returns one `HistoryEntry::Purge` with `event.kind=Purge`, `event.at=purged_at`, `event.actor=Some(actor)`. Re-invoking with the same `op_id` returns `PurgeOutcome::AlreadyPurged` and writes nothing (PK on `(target_id, op_id)` enforces idempotency). |
| `activate_monotonicity.rs` | Stage v1+v2+v3, activate v3, then attempt `activate_version(target, version=2, expected_prior=Some(3))` → `StoreError::Conflict { kind: ActivationRaced }`. Same with `expected_prior=None`. v3 stays active; reads see v3. |
| `consent_journal_atomicity.rs` | Inside one `with_apply_tx`, call `activate_version` then `append_consent_journal`, then return `Err`. Assert post-rollback: no records change AND no consent_journal row. Same closure but returning `Ok` → both writes visible. Proves the brief's atomicity invariant. |
| `rebac_visibility.rs` | Stage three records: one `private` for principal `alice`, one `private` for principal `bob`, one `team` shared. Call `list` with alice's principal → only alice's private + the team record; `bob`'s private is dropped pre-return; `ListResult.hidden` = 1. Same for `get` against bob's record with alice's principal: returns `None`. `Principal::system()` sees all three. Proves brief lines 2557/3287/4136 — non-readable rows never surface, hidden count is reported. |
| `tx_rollback.rs` | Closure returning `Err`: partial writes invisible. `panic!` inside closure: tx rolled back (rusqlite drop guard), connection still usable for the next `with_apply_tx`. |
| `edges.rs` | `add_edge` upsert semantics; `remove_edge` history; backlinks query (manual SQL inside test) returns expected set; tombstoning a record leaves edges intact. |
| `migrations.rs` | Apply on empty DB; re-apply is no-op; tampered ledger checksum aborts open; migration ids/checksums match across `cargo test --no-default-features` and `cargo test --all-features`. |
| `capabilities.rs` | Caps: `{fts: true, graph_edges: true, transactions: true, vector: false}`. |
| `apply_token_gate.rs` | Compile-fail test (via `trybuild`): a function that imports `MemoryStoreApply` and tries to call `with_apply_tx` without a token from `cairn_core::wal` fails to build. Establishes the token-gate as type-enforced. |
| `fts_visibility_predicate.rs` | Index three records: one active+visible, one tombstoned, one expired. FTS search with the same query against all three bodies returns only the active+visible record. Confirms FTS read joins the full `active=1 AND tombstoned=0 AND (expired_at IS NULL OR expired_at > now)` predicate. |

### 8.3 Conformance

A `MemoryStoreConformance` test suite lives in `cairn-test-fixtures`
(dev-only). Future stores re-run the suite. #46 ships the suite + the
SQLite invocation; no second store impl yet.

### 8.4 Property tests

`proptest` for `stage_version + activate_version → get` round-trip over
arbitrary `MemoryRecord` (uses existing `MemoryRecord` strategy from
`cairn-test-fixtures`).

## 9. Verification (run before pushing)

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" \
  cargo doc --workspace --no-deps --document-private-items --locked
cargo deny check
cargo audit --deny warnings
cargo machete
```

`rusqlite` license review: MIT — already on `deny.toml` allowlist.

## 10. Commit plan (single PR)

1. `feat(core): MemoryStore read trait + apply token + sealed write trait` —
   `MemoryStore` (read), `MemoryStoreApply`/`MemoryStoreApplyTx` (sealed),
   `ApplyToken` with `pub(in crate::wal)` constructor, `cairn_core::wal`
   stub module, `StoreError`, `ConflictKind`, `ListQuery`, `ChangeKind`,
   `Edge`/`EdgeKind`. No new workspace deps.
2. `feat(store): rusqlite, migrations runner, schema_migrations` —
   migrations 0001–0008 (records uses brief-aligned versioned-COW schema),
   runner, `open()`, pragma setup. `rusqlite` features `bundled`,
   `serde_json`. No `sqlite-vec` dep.
3. `feat(store-sqlite): impl MemoryStore (read) + MemoryStoreApply (write)` —
   `store.rs` for reads, `apply.rs` for writes, error wrapping, capability
   flags flipped to `{fts, graph_edges, transactions} = true`,
   `vector = false`.
4. `test(store-sqlite): COW versioning + tombstone + purge + token gate` —
   integration tests, property tests, conformance suite, `trybuild`
   compile-fail test for the apply-token gate, change_kind=purge decode
   test.

## 11. Risks & open questions

- **Brief reading: rebac at MemoryStore layer.** Brief line 1624 ("the
  harness never reaches the store directly") describes invocation
  flow, while lines 2557, 3287, 4136 specify enforcement *location*:
  rebac is per-row at the store. This spec follows the latter — read
  methods take a `&Principal`, the store drops non-readable rows
  pre-return. The full ReBAC rule set lives in `cairn-core::rebac`
  (separate issue); #46 ships only the principal-bearing trait surface
  + the SQL filter helpers (visibility tier check + actor scope match).

- **`ApplyToken` is a discipline mechanism, not a security boundary.**
  Anyone with mutable repo access can mint a constructor. The token
  exists to make non-WAL writes a *compile error in well-behaved code*,
  not to thwart adversaries. Acceptable for a single-author P0 vault.
- **Splitting `MemoryStore` (read) from `MemoryStoreApply` (write)** is a
  brief-level shape choice. Brief §4 row 1 calls `MemoryStore` "the
  storage contract" and lists CRUD as part of it; #46 splits CRUD into
  read-trait + token-gated apply-trait. This is a refinement of the
  brief, not a contradiction — both surfaces are still "the storage
  contract", and the WAL-only invariant from CLAUDE.md §4 invariant 5 is
  honored at the type level. **Flag for maintainer review** before
  implementation lands.
- **`sqlite-vec` deferral.** Vec table creation is fully deferred to #48.
  When #48 adds its migration, the migration must be ledger-stable —
  applying on every build and using `CREATE VIRTUAL TABLE IF NOT EXISTS`
  guarded by extension presence, with a fallback shim that records the
  migration as applied without creating the virtual table. #48 owns
  this; #46 just leaves the slot.
- **`async_trait` vs RPITIT.** Existing trait uses `async_trait` for
  `dyn` compatibility (the registry stores `Box<dyn MemoryStore>`).
  RPITIT is not yet `dyn`-compatible without extra ceremony.
- **Edge cascade.** Choosing not to cascade-delete edges on tombstone is
  a semantic call. Brief is silent. Documented in §5.5; revisit if §10
  workflows complain.
- **Schema migrations are forward-only.** Any change to a committed
  `*.sql` file fails the checksum check.

## 12. Acceptance criteria mapping

| AC (issue #46) | Where satisfied |
|---|---|
| CRUD operations round-trip complete `MemoryRecord` values. | §8.2 `crud_roundtrip.rs` + property test |
| Version history distinguishes update, tombstone, expire, **purge** states. | `ChangeKind` enum declares all four variants; #46 emits all four — `Update` from `created_at`, `Tombstone` from `tombstoned_at`, `Expire` from `expired_at`, `Purge` from the `record_purges` audit row. Each `RecordVersion` carries an immutable per-version event sequence (§5.4) so a later tombstone never rewrites earlier update history. Tested by `tombstone_preserves_history.rs` and `purge_audit_marker.rs` (§8.2). |
| Store capabilities correctly advertise FTS5 and local vector support when compiled/enabled. | §7.3 + §8.2 `capabilities.rs` |
| Run `MemoryStore` conformance tests. | §8.3 |
| Run transaction rollback tests for failing writes. | §8.2 `tx_rollback.rs` |
| Run edge/backlink tests against fixture graphs. | §8.2 `edges.rs` |
