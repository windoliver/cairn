# Issue #46 — SQLite `MemoryStore` CRUD, versioning, and graph edges

**Status:** draft
**Date:** 2026-04-27
**Issue:** [#46](https://github.com/windoliver/cairn/issues/46) (parent: #6)
**Brief sections:** §4 MemoryStore contract · §5.1 Read path · §5.2 Write path · §5.6 WAL state machine · §3.0 Storage topology

## 1. Goal

Land the P0 `MemoryStore` adapter primitives: typed CRUD over `MemoryRecord`,
logical-deletion version history (`update`/`tombstone`/`expire`), graph edge
operations, and a closure-based synchronous transaction surface — all backed
by `rusqlite` (`bundled`) against `.cairn/cairn.db`. Migrations covering the
full P0 schema (records, edges, FTS5, WAL ops/steps, replay ledger, consent
journal, locks, jobs) ship in the same PR; #45 was closed without producing
migrations, so #46 absorbs that scope.

These adapter methods are the **physical-apply primitives** the WAL state
machine (§5.6, lands in #8) will orchestrate. They are not callable directly
from verbs in #46. Tests + the future state machine are the only callers.

**Out of scope.** Physical purge/forget (the WAL state machine in #8 owns
Phase A drain → Phase B physical delete; #46 only ships the schema for it).
Ranking, semantic embedding generation, hybrid search orchestration. The
`sqlite-vec` virtual table is **not** created in #46 — it lands with
embeddings in #48.

## 2. Context

- Brief §4 row 1 fixes `MemoryStore` as the storage contract; P0 default is
  pure SQLite + FTS5 with `sqlite-vec`.
- **CLAUDE.md invariant 5** — "WAL + two-phase apply for every mutation.
  Every write goes through the WAL state machine. No direct DB mutations."
  #46 ships the *primitives* the state machine will call. Verb-layer
  mutations only flow through #8.
- Existing `cairn-store-sqlite` is a stub: only `name`/`capabilities`/
  `supported_contract_versions`. No `rusqlite` dep yet (intentionally deferred
  per `Cargo.toml` comment).
- `cairn-core::contract::memory_store::MemoryStore` is surface-only. CRUD
  methods do not exist. Adding them is brief-level surface change but
  pre-authorized by the scaffold doc which states CRUD lands in #46.
- `MemoryRecord`, `RecordId`, `Provenance`, `ActorChain`, `Evidence`,
  `Scope`, taxonomy types already live in `cairn-core::domain`.
- WAL state machine (§5.6) is not implemented in #46; we ship the
  `wal_ops`/`wal_steps` tables only.

## 3. Architecture

```
cairn-core
└── contract::memory_store
    ├── MemoryStore trait (extended: get/list/version_history/with_tx + existing)
    ├── MemoryStoreTx trait (sealed; upsert/tombstone/expire/add_edge/remove_edge)
    ├── StoreError (abstract; backend variant boxes adapter errors)
    ├── ChangeKind enum (Update | Tombstone | Expire | Purge — Purge variant
    │   exists for forward-compat with #8; no #46 method emits it)
    ├── ListQuery, RecordVersion, Edge, EdgeKind, ConflictKind
    └── ActorRef, Timestamp (re-exported from domain)

cairn-store-sqlite
├── migrations/                    (embedded via include_str!)
│   ├── 0001_init_pragmas.sql
│   ├── 0002_records.sql
│   ├── 0003_edges.sql
│   ├── 0004_fts5.sql
│   ├── 0005_wal_state.sql
│   ├── 0006_replay_consent.sql
│   ├── 0007_locks_jobs.sql
│   └── 0008_meta.sql
└── src/
    ├── lib.rs                     (existing register_plugin! + extended impl)
    ├── error.rs                   (rusqlite-aware SqliteStoreError + From for core)
    ├── schema/
    │   ├── mod.rs                 (Migration struct, &'static [Migration])
    │   └── runner.rs              (apply_pending, checksum verify)
    ├── conn.rs                    (open(), pragma setup)
    ├── store.rs                   (SqliteMemoryStore impl on MemoryStore)
    └── tx.rs                      (SqliteMemoryStoreTx impl on MemoryStoreTx)
```

The `sqlite-vec` virtual table and any vec-related code are **not present in
#46**. Migrations 0001–0008 are unconditional; the migration ledger is
identical for every build flavor in #46. #48 introduces a new numbered
migration for the vec table.

**Concurrency.** `rusqlite::Connection` is `!Sync`. The store wraps it in
`tokio::sync::Mutex<Connection>` and dispatches every operation to
`tokio::task::spawn_blocking`. SQLite is set to WAL journal mode so concurrent
readers don't block on writers. P0 single-author model means contention is
minimal; one process owns the file.

**Sync boundary.** All `rusqlite` calls happen inside `spawn_blocking`. The
async outer trait surface awaits the join. No `block_on` inside async, no
`std::sync::Mutex` held across `.await` (CLAUDE.md §6.3).

## 4. Trait surface

### 4.1 `MemoryStore` (extended)

```rust
#[async_trait::async_trait]
pub trait MemoryStore: Send + Sync {
    // existing surface — unchanged
    fn name(&self) -> &str;
    fn capabilities(&self) -> &MemoryStoreCapabilities;
    fn supported_contract_versions(&self) -> VersionRange;

    // read path (auto-tx, read-only)
    async fn get(&self, id: &RecordId) -> Result<Option<MemoryRecord>, StoreError>;
    async fn list(&self, query: &ListQuery) -> Result<Vec<MemoryRecord>, StoreError>;
    async fn version_history(&self, id: &RecordId)
        -> Result<Vec<RecordVersion>, StoreError>;

    // write path: single sync closure runs entirely inside one
    // spawn_blocking holding one rusqlite::Transaction.
    async fn with_tx<F, T>(&self, f: F) -> Result<T, StoreError>
    where
        F: FnOnce(&mut dyn MemoryStoreTx) -> Result<T, StoreError>
            + Send + 'static,
        T: Send + 'static;
}
```

There is exactly one transaction signature. No `BoxFuture`. No `futures`
crate dep. The closure is synchronous, owned, and `'static`-bounded.

### 4.2 `MemoryStoreTx` (sealed, sync methods)

```rust
pub trait MemoryStoreTx: sealed::Sealed + Send {
    fn upsert(&mut self, record: &MemoryRecord) -> Result<(), StoreError>;
    fn tombstone(&mut self, id: &RecordId, actor: &ActorRef)
        -> Result<(), StoreError>;
    fn expire(&mut self, id: &RecordId, at: Timestamp)
        -> Result<(), StoreError>;
    fn add_edge(&mut self, edge: &Edge) -> Result<(), StoreError>;
    fn remove_edge(
        &mut self, from: &RecordId, to: &RecordId, kind: EdgeKind,
    ) -> Result<(), StoreError>;
}
```

**No `purge` method in #46.** The WAL state machine in #8 owns the Phase A
drain → Phase B physical delete sequence required for auditable forget; the
adapter primitive will land in that PR alongside the state-machine fence.
Until then, the data model reserves `ChangeKind::Purge` so version-history
readers compile against the final enum, but no #46 code path emits it.

`MemoryStoreTx` is sync because `rusqlite::Transaction<'_>` borrows the
connection on a single thread; async-per-method plus `spawn_blocking`-per-call
cannot preserve a single SQLite tx across hops without `unsafe` or runtime
blocking.

### 4.3 Tx execution model

`with_tx` does a single `tokio::task::spawn_blocking` that:

1. Acquires the connection mutex.
2. Begins a `rusqlite::Transaction`.
3. Constructs a `SqliteMemoryStoreTx<'_>` wrapping the tx handle.
4. Calls the closure synchronously.
5. Commits on `Ok`, rolls back on `Err`.
6. On panic inside the closure: `rusqlite::Transaction`'s drop guard rolls
   back automatically; `spawn_blocking` surfaces the panic as a `JoinError`
   that maps to `StoreError::Backend`.
7. Returns `T` to the caller.

Compile-time guarantees: callers cannot leak `&mut dyn MemoryStoreTx` beyond
the closure, and the trait being sealed prevents stashing an owned tx
elsewhere.

`async_trait` is unnecessary on `MemoryStoreTx` (no async methods).
`MemoryStore` itself keeps `async_trait` for `dyn` compatibility on the
read-path methods and `with_tx`.

Sealed via private supertrait (`mod sealed { pub trait Sealed {} }`).

## 5. Data model

### 5.1 Tables (P0)

| Table | Purpose |
|---|---|
| `records` | Active row per `record_id`. JSON columns for provenance, actor_chain, evidence, taxonomy, scope. |
| `record_versions` | Append-only history. `change_kind` ∈ {`update`,`tombstone`,`expire`,`purge`}. #46 emits the first three; `purge` rows are written only by #8's state-machine path. |
| `edges` | `from_id`, `to_id`, `kind`, `weight`, `metadata` JSON. Composite PK `(from_id, to_id, kind)`. |
| `edge_versions` | Append-only edge history (mirrors `record_versions`). |
| `records_fts` | FTS5 contentless table over `body`, `title`, `tags`. Triggers on `records` keep it in sync. |
| `wal_ops`, `wal_steps` | WAL state-machine tables (rows added in #8). |
| `replay_ledger`, `issuer_seq`, `challenges` | Identity/replay surface (rows added in #7). |
| `consent_journal` | Append-only consent log (rows added by #8's purge path and the consent UX in #17). |
| `locks` | Per-vault advisory locks. |
| `jobs` | Workflow host queue. |
| `schema_migrations` | `(id INTEGER PK, name TEXT, checksum TEXT, applied_at TEXT)`. |

The `records_vec` virtual table is **not** in #46. It lands with #48.

### 5.2 Version state semantics (#46-emitted)

- `upsert(record)` — if row exists, copy current row to `record_versions`
  with `change_kind=update`, then write new row to `records`. If new, insert
  only (no version row).
- `tombstone(id, actor)` — copy current row to `record_versions` with
  `change_kind=tombstone`, set `records.tombstoned_at`/`tombstoned_by`. Row
  remains in `records` but flagged.
- `expire(id, at)` — copy current row to `record_versions` with
  `change_kind=expire`, set `records.expired_at`. Row remains.

`get`/`list` filter out tombstoned and expired rows by default. `ListQuery`
exposes `include_tombstoned: bool` / `include_expired: bool` toggles for
admin-style queries.

`change_kind=purge` is a reserved enum variant that #8's two-phase
forget implementation will emit. #46 has no code path that produces it; tests
that need a purge marker construct one via raw SQL inside the test body.

### 5.3 Edge semantics

- `add_edge` — insert with conflict-update on `(from_id, to_id, kind)`; copy
  prior to `edge_versions` if updating.
- `remove_edge` — copy to `edge_versions` (`change_kind=remove`), DELETE row.
- Tombstoning a record does **not** cascade-delete edges; consumers filter via
  joining against active records. Rationale: edges are evidence; orphan edges
  are diagnostic, not corrupt. Edge cleanup on physical purge is owned by #8.

## 6. Error handling

`cairn-core::contract::memory_store::StoreError` is **abstract** (no rusqlite
dependency in core):

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    #[error("record not found: {0}")]
    NotFound(RecordId),
    #[error("conflict: {kind:?}")]
    Conflict { kind: ConflictKind },
    #[error("invariant violated: {0}")]
    Invariant(&'static str),
    #[error("backend error")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("serialization: {0}")]
    Serde(#[from] serde_json::Error),
}
```

`cairn-store-sqlite::error::SqliteStoreError` wraps `rusqlite::Error` and
implements `From<SqliteStoreError> for cairn_core::StoreError` via the
`Backend` variant. `Conflict { kind: UniqueViolation | ForeignKey }` is
detected from `rusqlite::ErrorCode` and surfaced as the typed variant rather
than `Backend`.

## 7. Migrations

### 7.1 Runner

Hand-rolled (no `sqlx::migrate!` since driver is `rusqlite`). Embeds each
migration via `include_str!`. List is a `&'static [Migration]` ordered by id
and **identical across all build flavors** — no feature gates can change
which numbered migrations exist.

```rust
struct Migration {
    id: u32,
    name: &'static str,
    sql: &'static str,
    checksum: &'static str, // sha256 of sql, computed at build via build.rs
}
```

`apply_pending(conn)`:
1. Ensure `schema_migrations` table exists (run `0008_meta.sql`'s create-only
   prefix idempotently — bootstrap).
2. Read applied `(id, checksum)` set.
3. For each migration in list: if applied, verify checksum match → abort on
   mismatch; if not applied, execute in single sqlite tx, insert ledger row.
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

There is no extension loading and no optional schema in #46. Basic CRUD has
zero dependency on `sqlite-vec`. The vec story is entirely deferred to #48,
which will add a new numbered migration and feature-gate only the **runtime
path** (extension load + queries), never the migration ledger.

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
- `schema::tests::list_invariant` — same migration ids and ordering for any
  feature combination of the crate (sanity guard against future additions).
- `error::tests::rusqlite_unique_to_conflict` — `rusqlite::ErrorCode::ConstraintUnique`
  maps to `StoreError::Conflict`.
- `conn::tests::pragmas_applied` — open writes WAL, FK pragmas; verify via
  `PRAGMA` reads.

### 8.2 Integration (`cairn-store-sqlite/tests/`)

| File | Coverage |
|---|---|
| `crud_roundtrip.rs` | `upsert` → `get` returns byte-equal `MemoryRecord` (full provenance, actor_chain, evidence, scope, confidence, salience). |
| `versioning.rs` | `upsert` → `upsert` → `tombstone` → `expire` sequence. `version_history` returns 4 rows with `change_kind ∈ {update, update, tombstone, expire}` in order. Default `list`/`get` filters tombstoned/expired; `include_tombstoned`/`include_expired` flags surface them. |
| `purge_state_reserved.rs` | Insert a synthetic `change_kind=purge` row via raw SQL; assert `version_history` decodes the variant correctly. Documents the forward-compat contract with #8. |
| `tx_rollback.rs` | Closure returning `Err`: partial writes invisible. `panic!` inside closure: tx rolled back (rusqlite drop guard), connection still usable for the next `with_tx`. |
| `edges.rs` | `add_edge` upsert semantics; `remove_edge` history; backlinks query (manual SQL inside test) returns expected set; tombstoning a record leaves edges intact. |
| `migrations.rs` | Apply on empty DB; re-apply is no-op; tampered ledger checksum aborts open; migration id list and checksums match across `cargo test --no-default-features` and `cargo test --all-features`. |
| `capabilities.rs` | Caps flip to `{fts: true, graph_edges: true, transactions: true, vector: false}`. |

### 8.3 Conformance

A `MemoryStoreConformance` test suite lives in `cairn-test-fixtures` (dev-only).
Future stores re-run the suite. #46 ships the suite + the SQLite invocation;
no second store impl yet.

### 8.4 Property tests

`proptest` for `upsert → get` round-trip over arbitrary `MemoryRecord` (uses
existing `MemoryRecord` strategy from `cairn-test-fixtures`).

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

`rusqlite` license review: MIT — already on `deny.toml` allowlist. No
`cargo deny` change expected.

## 10. Commit plan (single PR)

1. `feat(core): extend MemoryStore trait with CRUD + tx + version history` —
   trait surface, abstract `StoreError`, supporting types
   (`ListQuery`, `RecordVersion`, `ChangeKind` (incl. reserved `Purge`),
   `Edge`, `EdgeKind`, `ConflictKind`, `MemoryStoreTx` sealed trait).
   No new workspace deps.
2. `feat(store): add rusqlite, migrations runner, schema_migrations table` —
   migrations 0001–0008, runner, `open()`, pragma setup. `rusqlite` features
   `bundled`, `serde_json`. No `sqlite-vec` dep.
3. `feat(store-sqlite): implement MemoryStore + MemoryStoreTx` — `store.rs`,
   `tx.rs`, error wrapping (`SqliteStoreError → StoreError`), capability
   flags flipped to `{fts, graph_edges, transactions} = true`, `vector = false`.
4. `test(store-sqlite): conformance + rollback + edges + versioning` — all
   integration tests + property tests + conformance suite + the
   `purge_state_reserved` forward-compat test.

## 11. Risks & open questions

- **`sqlite-vec` deferral.** Vec table creation is fully deferred to #48, so
  #46 never touches the extension. Risk: the eventual #48 migration adds a
  numbered entry that older binaries (no `vec` feature) will still **apply**
  — by design, the migration must use `CREATE VIRTUAL TABLE IF NOT EXISTS
  records_vec USING vec0(...)` only when the extension loads, with a fallback
  no-op `CREATE TABLE records_vec_pending(...)` shim that the runtime ignores.
  This keeps the migration ledger identical across builds. #48 owns this
  detail; #46 just leaves the slot.
- **`async_trait` vs RPITIT.** Existing trait uses `async_trait` for
  `dyn` compatibility (the registry stores `Box<dyn MemoryStore>`).
  RPITIT is not yet `dyn`-compatible without extra ceremony. Keeping
  `async_trait` is the correct call for #46.
- **WAL invariant 5.** CLAUDE.md mandates "every write goes through the WAL
  state machine". #46 ships the *primitives* (`upsert`/`tombstone`/`expire`/
  `add_edge`/`remove_edge`); the state machine in #8 wraps them. Verbs do
  not call these primitives directly until #8 lands. Tests are the only
  caller in #46.
- **Edge cascade.** Choosing not to cascade-delete edges on tombstone is a
  semantic call. Brief is silent. Documented in §5.3; revisit if §10
  workflows complain.
- **Schema migrations are forward-only.** A bug in a shipped migration must
  be fixed by a *new* migration. CI lock-step: any change to a committed
  `*.sql` file fails review.

## 12. Acceptance criteria mapping

| AC (issue #46) | Where satisfied |
|---|---|
| CRUD operations round-trip complete `MemoryRecord` values. | §8.2 `crud_roundtrip.rs` + property test |
| Version history distinguishes update, tombstone, expire, **purge** states. | `ChangeKind` enum (§3, §5.2) declares all four variants; #46 emits the first three; `purge_state_reserved.rs` (§8.2) verifies decode of a synthetic purge row, satisfying the *distinguishability* AC. The emitting code path lands with #8. |
| Store capabilities correctly advertise FTS5 and local vector support when compiled/enabled. | §7.3 + §8.2 `capabilities.rs` (vector stays false in #46; the AC is satisfied by reporting the truthful state for the compiled crate). |
| Run `MemoryStore` conformance tests. | §8.3 |
| Run transaction rollback tests for failing writes. | §8.2 `tx_rollback.rs` |
| Run edge/backlink tests against fixture graphs. | §8.2 `edges.rs` |
