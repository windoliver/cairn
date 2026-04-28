# Issue #46 — SQLite `MemoryStore` CRUD, versioning, and graph edges

**Status:** draft
**Date:** 2026-04-27
**Issue:** [#46](https://github.com/windoliver/cairn/issues/46) (parent: #6)
**Brief sections:** §4 MemoryStore contract · §5.1 Read path · §5.2 Write path · §3.0 Storage topology

## 1. Goal

Land the P0 `MemoryStore` implementation: typed CRUD over `MemoryRecord`, version
history with distinct `update`/`tombstone`/`expire`/`purge` states, graph edge
operations, and closure-based transactions — all backed by `rusqlite`
(`bundled`) against `.cairn/cairn.db`. Migrations for the full P0 schema
(records, edges, FTS5, vec virtual table, WAL ops/steps, replay ledger,
consent journal, locks, jobs) ship in the same PR; #45 was closed without
producing migrations, so #46 absorbs that scope.

Out of scope (per issue): ranking, semantic embedding generation, hybrid search
orchestration. FTS5 and `sqlite-vec` tables are *created* but not exercised —
their query paths land in #47/#48/#49.

## 2. Context

- Brief §4 row 1 fixes `MemoryStore` as the storage contract; P0 default is
  pure SQLite + FTS5 with `sqlite-vec`.
- Existing `cairn-store-sqlite` is a stub: only `name`/`capabilities`/
  `supported_contract_versions`. No `rusqlite` dep yet (intentionally deferred
  per `Cargo.toml` comment).
- `cairn-core::contract::memory_store::MemoryStore` is surface-only. CRUD
  methods do not exist. Adding them is brief-level surface change but
  pre-authorized by the scaffold doc which states CRUD lands in #46.
- `MemoryRecord`, `RecordId`, `Provenance`, `ActorChain`, `Evidence`,
  `Scope`, taxonomy types already live in `cairn-core::domain`.
- WAL state machine (§5.6) is **not** in scope — #46 ships the migration
  tables (`wal_ops`, `wal_steps`); the state machine wires up in #8.

## 3. Architecture

```
cairn-core
└── contract::memory_store
    ├── MemoryStore trait (extended: get/list/version_history/with_tx + existing)
    ├── MemoryStoreTx trait (sealed; upsert/tombstone/expire/purge/add_edge/remove_edge)
    ├── StoreError (abstract; backend variant boxes adapter errors)
    ├── ListQuery, RecordVersion, ChangeKind, Edge, EdgeKind, ConflictKind
    └── ConsentJournalRef, ActorRef (re-exported from domain)

cairn-store-sqlite
├── migrations/                    (embedded via include_str!)
│   ├── 0001_init_pragmas.sql
│   ├── 0002_records.sql
│   ├── 0003_edges.sql
│   ├── 0004_fts5.sql
│   ├── 0005_vec.sql
│   ├── 0006_wal_state.sql
│   ├── 0007_replay_consent.sql
│   ├── 0008_locks_jobs.sql
│   └── 0009_meta.sql
└── src/
    ├── lib.rs                     (existing register_plugin! + extended impl)
    ├── error.rs                   (rusqlite-aware StoreError + From for core variant)
    ├── schema/
    │   ├── mod.rs                 (Migration struct, list of (id, name, sql, checksum))
    │   └── runner.rs              (apply_pending, checksum verify)
    ├── conn.rs                    (open(), pragma setup, sqlite-vec extension load)
    ├── store.rs                   (SqliteMemoryStore impl on MemoryStore)
    └── tx.rs                      (SqliteMemoryStoreTx impl on MemoryStoreTx)
```

**Concurrency.** `rusqlite::Connection` is `!Sync`. The store wraps it in
`tokio::sync::Mutex<Connection>` and dispatches every query to
`tokio::task::spawn_blocking`. SQLite is set to WAL journal mode so concurrent
readers don't block on writers. P0 single-author model means contention is
minimal; one process owns the file.

**Sync boundary.** All `rusqlite` calls happen inside `spawn_blocking`. The
async trait surface awaits the join. No `block_on` inside async, no
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

    // write path (single tx)
    async fn with_tx<'a, F, T>(&'a self, f: F) -> Result<T, StoreError>
    where
        F: for<'tx> FnOnce(&'tx mut dyn MemoryStoreTx)
                -> futures::future::BoxFuture<'tx, Result<T, StoreError>>
            + Send + 'a,
        T: Send + 'a;
}
```

### 4.2 `MemoryStoreTx` (sealed)

```rust
#[async_trait::async_trait]
pub trait MemoryStoreTx: Send {
    async fn upsert(&mut self, record: &MemoryRecord) -> Result<(), StoreError>;
    async fn tombstone(&mut self, id: &RecordId, actor: &ActorRef)
        -> Result<(), StoreError>;
    async fn expire(&mut self, id: &RecordId, at: Timestamp)
        -> Result<(), StoreError>;
    async fn purge(&mut self, id: &RecordId, journal: &ConsentJournalRef)
        -> Result<(), StoreError>;
    async fn add_edge(&mut self, edge: &Edge) -> Result<(), StoreError>;
    async fn remove_edge(
        &mut self, from: &RecordId, to: &RecordId, kind: EdgeKind,
    ) -> Result<(), StoreError>;
}
```

Sealed via private supertrait (`mod sealed { pub trait Sealed {} }`) — no
third-party impls. Justification: tx semantics couple tightly to backend
internals; opening this for external impl invites correctness footguns.

### 4.3 Why closure-based tx

- **Compile-time guarantee** the tx is committed or rolled back — no leaked
  open transactions if the caller panics or returns early.
- Mirrors `sqlx::Pool::begin` / `tokio::task::spawn_blocking` ergonomics.
- Lets `&mut dyn MemoryStoreTx` be `dyn`-compatible without leaking
  connection lifetimes through the public type.

The `BoxFuture` indirection is the price for `dyn`-compatibility +
`async fn` in trait + closure args. Native RPITIT can't express
`for<'tx> FnOnce(&'tx mut dyn Trait) -> impl Future` cleanly until
async-closures stabilize.

## 5. Data model

### 5.1 Tables (P0)

| Table | Purpose |
|---|---|
| `records` | Active row per `record_id`. JSON columns for provenance, actor_chain, evidence, taxonomy, scope. |
| `record_versions` | Append-only history. `change_kind` ∈ {`update`,`tombstone`,`expire`,`purge`}. Full prior body for `update`/`tombstone`/`expire`; `purge` keeps only metadata (body redacted). |
| `edges` | `from_id`, `to_id`, `kind`, `weight`, `metadata` JSON. Composite PK `(from_id, to_id, kind)`. |
| `edge_versions` | Append-only edge history (mirrors `record_versions`). |
| `records_fts` | FTS5 contentless table over `body`, `title`, `tags`. Triggers on `records` keep it in sync. |
| `records_vec` | `sqlite-vec` virtual table (`vec0`). Empty in #46; populated in #48. |
| `wal_ops`, `wal_steps` | WAL state-machine tables (rows added in #8). |
| `replay_ledger`, `issuer_seq`, `challenges` | Identity/replay surface (rows added in #7). |
| `consent_journal` | Append-only consent log (rows added in #17). |
| `locks` | Per-vault advisory locks. |
| `jobs` | Workflow host queue. |
| `schema_migrations` | `(id INTEGER PK, name TEXT, checksum TEXT, applied_at TEXT)`. |

### 5.2 Version state semantics

- `upsert(record)` — if row exists, copy current row to `record_versions` with
  `change_kind=update`, then write new row to `records`. If new, insert only
  (no version row).
- `tombstone(id, actor)` — copy current row to `record_versions` with
  `change_kind=tombstone`, set `records.tombstoned_at`/`tombstoned_by`. Row
  remains in `records` but flagged.
- `expire(id, at)` — copy current row to `record_versions` with
  `change_kind=expire`, set `records.expired_at`. Row remains.
- `purge(id, journal)` — physical deletion. In a single tx:
  1. Write the supplied consent entry to `consent_journal` (raw row insert —
     the table ships in #46 even though the consent UX/lint surface lands in
     #17). Open `consent_journal_ref` is the row's PK; supplying a stale or
     non-matching ref → `StoreError::Invariant`.
  2. DELETE every row from `record_versions` where `record_id = id` (no
     redaction-by-update — full row delete; bodies are not recoverable from
     the history table).
  3. DELETE from `records` where `id = id`.
  4. INSERT one final marker row into `record_versions` with
     `change_kind=purge`, body NULL, and `consent_journal_id` foreign-keying
     the just-written journal row. This row is the only post-purge artefact
     and contains no payload — only metadata + audit pointer.
  5. DELETE matching rows from `records_fts` and (when present) `records_vec`
     so derived indexes carry no residual content.

  Acceptance: after `purge`, querying `record_versions` for the id returns
  exactly one row (the marker) with `body IS NULL`; full-text search over the
  body returns zero hits; `consent_journal` contains a permanent audit row.

`get`/`list` filter out tombstoned and expired rows by default. `ListQuery`
exposes `include_tombstoned: bool` / `include_expired: bool` toggles for
admin-style queries.

### 5.3 Edge semantics

- `add_edge` — insert with conflict-update on `(from_id, to_id, kind)`; copy
  prior to `edge_versions` if updating.
- `remove_edge` — copy to `edge_versions` (`change_kind=remove`), DELETE row.
- Tombstoning a record does **not** cascade-delete edges; consumers filter via
  joining against active records. Rationale: edges are evidence; orphan edges
  are diagnostic, not corrupt.

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
migration via `include_str!`. List is a `&'static [Migration]` ordered by id.

```rust
struct Migration {
    id: u32,
    name: &'static str,
    sql: &'static str,
    checksum: &'static str, // sha256 of sql, computed at build via build.rs
}
```

`apply_pending(conn)`:
1. Ensure `schema_migrations` table exists (run `0009_meta.sql`'s create-only
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
    // 5. (vec feature only) attempt sqlite-vec extension load + create vec
    //    virtual table; on failure log warn + leave vec capability off — DO
    //    NOT fail open(). CRUD must work without the extension.
    // 6. wrap in tokio::sync::Mutex, return SqliteMemoryStore
}
```

**`sqlite-vec` is opt-in.** The `vec` cargo feature defaults to **off** in #46.
Migration `0005_vec.sql` is conditionally executed only when the feature is
enabled and the extension loads successfully; otherwise the table is not
created and `capabilities().vector` stays `false`. Basic CRUD has zero
dependency on `sqlite-vec`. The feature flips default-on in #48 once the
extension is statically linked and the embedding pipeline lands.

### 7.3 Capability advertisement

Post-#46:
```rust
MemoryStoreCapabilities { fts: true, vector: false, graph_edges: true, transactions: true }
```

`vector` flips in #48 when embeddings + `sqlite-vec` querying lands.

## 8. Testing

### 8.1 Unit (`cairn-store-sqlite/src/`)

- `schema::tests::checksum_stable` — checksums match across builds (build.rs
  output committed to ensure reproducibility).
- `error::tests::rusqlite_unique_to_conflict` — `rusqlite::ErrorCode::ConstraintUnique`
  maps to `StoreError::Conflict`.
- `conn::tests::pragmas_applied` — open writes WAL, FK pragmas; verify via
  `PRAGMA` reads.

### 8.2 Integration (`cairn-store-sqlite/tests/`)

| File | Coverage |
|---|---|
| `crud_roundtrip.rs` | `upsert` → `get` returns byte-equal `MemoryRecord` (full provenance, actor_chain, evidence, scope, confidence, salience). |
| `versioning.rs` | `upsert` × 2 → `tombstone` → `expire` → `purge` sequence. Pre-purge: `version_history` returns 4 rows. Post-purge: `version_history` returns exactly 1 row (the purge marker) with `body IS NULL`; FTS query for any prior body content returns zero hits; `consent_journal` contains the audit row referenced by the marker. |
| `tx_rollback.rs` | Closure returning `Err`: partial writes invisible. `panic!` inside closure: tx rolled back, connection still usable. |
| `edges.rs` | `add_edge` upsert semantics; `remove_edge` history; backlinks query (manual SQL inside test) returns expected set; tombstoning a record leaves edges intact. |
| `migrations.rs` | Apply on empty DB; re-apply is no-op; tampered ledger checksum aborts open. |
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
   (`ListQuery`, `RecordVersion`, `ChangeKind`, `Edge`, `EdgeKind`).
   Adds `futures = "0.3"` (default-features-off, `std` only) to workspace
   deps for `BoxFuture` in the `with_tx` signature.
2. `feat(store): add rusqlite, migrations runner, schema_migrations table` —
   migrations 0001–0009, runner, `open()`, pragma setup. `rusqlite` features
   `bundled`, `serde_json`.
3. `feat(store-sqlite): implement MemoryStore + MemoryStoreTx` — `store.rs`,
   `tx.rs`, error wrapping (`SqliteStoreError → StoreError`), capability
   flags flipped.
4. `test(store-sqlite): conformance + rollback + edges + versioning` — all
   integration tests + property tests + conformance suite.

## 11. Risks & open questions

- **`sqlite-vec` extension loading.** P0 binary will eventually need the
  extension statically linked, but #46 keeps the `vec` cargo feature
  **default-off** so basic CRUD can never fail to open due to a missing
  extension. When the feature is on and the extension loads, the
  `records_vec` virtual table is created; otherwise it is skipped and
  `capabilities().vector` stays `false`. The default flips in #48.
- **`async_trait` vs RPITIT.** Existing trait uses `async_trait` for
  `dyn` compatibility (the registry stores `Box<dyn MemoryStore>`).
  RPITIT is not yet `dyn`-compatible without extra ceremony. Keeping
  `async_trait` is the correct call for #46; revisit when the dyn-RPITIT
  story matures.
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
| Version history distinguishes update, tombstone, expire, purge states. | §5.2 + §8.2 `versioning.rs` |
| Store capabilities correctly advertise FTS5 and local vector support when compiled/enabled. | §7.3 + §8.2 `capabilities.rs` |
| Run `MemoryStore` conformance tests. | §8.3 |
| Run transaction rollback tests for failing writes. | §8.2 `tx_rollback.rs` |
| Run edge/backlink tests against fixture graphs. | §8.2 `edges.rs` |
