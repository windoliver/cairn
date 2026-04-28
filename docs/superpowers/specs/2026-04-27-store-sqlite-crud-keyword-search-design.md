# Design: SQLite store CRUD + FTS5 keyword search

**Date:** 2026-04-27
**Issues:** [#46] (CRUD/versioning/edges), [#47] (FTS5 keyword search). Parent: [#6].
**Brief sources:** §3 (Records-in-SQLite), §3.0 (Storage topology), §4 (MemoryStore contract), §4.1 (capability advertisement), §5.1 (Read path / Rank & Filter), §5.2 (Write path), §5.6 (WAL state machine — out of scope for these PRs but boundary defined), §6.5 (record fields), §8.0.d (search filter DSL).

[#6]: https://github.com/windoliver/cairn/issues/6
[#46]: https://github.com/windoliver/cairn/issues/46
[#47]: https://github.com/windoliver/cairn/issues/47

---

## 1. Scope

This spec covers two of the four open sub-issues under epic [#6]:

- **#46** — `MemoryStore` CRUD, versioning, graph edges, transactions, against the existing P0 SQLite schema.
- **#47** — FTS5-backed keyword search returning ranking-input candidates, with the metadata-filter DSL.

Out of scope (separate spec to follow): **#48** (candle local embeddings + sqlite-vec ANN), **#49** (hybrid orchestration + `cairn reindex --from-db`).

The two PRs in this spec leave `MemoryStoreCapabilities` advertising:

```rust
MemoryStoreCapabilities {
    fts: true,            // PR-B
    vector: false,        // #48
    graph_edges: true,    // PR-A
    transactions: true,   // PR-A
}
```

Brief invariants exercised: §4 #5 (WAL boundary — store mutates `records` directly; WAL FSM lives in `cairn-core` and wires later in #8), §4 #6 (fail-closed capability), §4 #8 (no `unwrap`/`expect` in core), §6.11 (WAL FSM is a pure function in core; adapter persists outputs).

---

## 2. Pre-existing state (no rework)

Already merged on `main`:

- Migrations `0001_records.sql` … `0006_drift_hardening.sql`. Schema covers `records`, `records_fts`, `edges`, `records_latest` view, `wal_ops`, `wal_op_deps`, `wal_steps`, replay ledger, locks, consent journal.
- `cairn-store-sqlite::open()` / `open_in_memory()` apply migrations idempotently and verify the manifest.
- `MemoryStore` trait surface in `cairn-core::contract::memory_store` is the thin scaffold (`name`, `capabilities`, `supported_contract_versions`).
- `MemoryStoreCapabilities { fts, vector, graph_edges, transactions }`.
- `cairn-core::domain::filter::{validate_filter, compile_filter, ValidatedFilter, CompiledFilter}` — full DSL validator + parameterized SQL compiler.
- Generated IDL types `SearchArgs`, `SearchArgsMode`, `SearchArgsFilters` (recursive AND/OR/NOT/Leaf), `SearchArgsCitations`.
- `MemoryRecord` domain type with `id, kind, class, visibility, scope, body, provenance, evidence, salience, confidence, signature, tags`.
- Trait already uses `#[async_trait::async_trait]` and is `dyn`-compatible. `MemoryStorePlugin` companion trait carries the static consts.

What is *missing* and provided by this work:

- Trait-level CRUD/edge/search/tx methods.
- Storage of `record_json`, `confidence`, `salience`, `target_id`, `tags_json`, `tombstone_reason` on the `records` table.
- FTS5 index over `body` + `path` (currently body-only).
- Async wrapper over `rusqlite` via `tokio_rusqlite`.
- Test coverage for CRUD/edge/tx invariants and FTS search end-to-end including injection and FTS-syntax errors.

---

## 3. Trait surface — extend `MemoryStore`

The existing trait is widened in place (one trait, all verbs). Cap flags continue to gate optional methods. `CONTRACT_VERSION` bumps `0.1.0` → `0.2.0`; `SqliteMemoryStore::ACCEPTED_RANGE` widens to `0.1.0..0.3.0`.

```rust
#[async_trait]
pub trait MemoryStore: Send + Sync {
    // unchanged
    fn name(&self) -> &str;
    fn capabilities(&self) -> &MemoryStoreCapabilities;
    fn supported_contract_versions(&self) -> VersionRange;

    // CRUD (#46)
    async fn upsert(&self, record: &MemoryRecord) -> Result<UpsertOutcome, StoreError>;
    async fn get(&self, id: &RecordId) -> Result<Option<MemoryRecord>, StoreError>;
    async fn list(&self, args: &ListArgs) -> Result<ListPage, StoreError>;
    async fn tombstone(&self, id: &RecordId, reason: TombstoneReason) -> Result<(), StoreError>;
    async fn versions(&self, target: &TargetId) -> Result<Vec<RecordVersion>, StoreError>;

    // Edges (#46)
    async fn put_edge(&self, edge: &Edge) -> Result<(), StoreError>;
    async fn remove_edge(&self, key: &EdgeKey) -> Result<bool, StoreError>;
    async fn neighbours(&self, id: &RecordId, dir: EdgeDir) -> Result<Vec<Edge>, StoreError>;

    // Keyword search (#47)
    async fn search_keyword(&self, args: &KeywordSearchArgs)
        -> Result<KeywordSearchPage, StoreError>;

    // Tx (#46)
    async fn with_tx<F, T>(&self, f: F) -> Result<T, StoreError>
    where
        F: FnOnce(&mut StoreTx<'_>) -> Result<T, StoreError> + Send + 'static,
        T: Send + 'static;
}
```

Supporting types (in `cairn-core::contract::memory_store`):

```rust
pub enum TombstoneReason { Update, Expire, Forget, Purge }

pub struct UpsertOutcome {
    pub record_id: RecordId,
    pub target_id: TargetId,
    pub version: u32,
    pub content_changed: bool,
    pub prior_hash: Option<BodyHash>,
}

pub struct ListArgs {
    pub scope: Option<ScopeTuple>,
    pub kind: Option<MemoryKind>,
    pub class: Option<MemoryClass>,
    pub limit: usize,
    pub cursor: Option<ListCursor>,
}

pub struct ListPage {
    pub records: Vec<MemoryRecord>,
    pub next_cursor: Option<ListCursor>,
}

pub struct RecordVersion {
    pub record_id: RecordId,
    pub version: u32,
    pub created_at: i64,
    pub updated_at: i64,
    pub active: bool,
    pub tombstoned: bool,
    pub tombstone_reason: Option<TombstoneReason>,
    pub body_hash: BodyHash,
}

pub struct Edge {
    pub src: RecordId,
    pub dst: RecordId,
    pub kind: EdgeKind,        // updates, mentions, supports, …
    pub weight: Option<f32>,
}
pub struct EdgeKey { pub src: RecordId, pub dst: RecordId, pub kind: EdgeKind }
pub enum EdgeDir { Out, In, Both }

pub struct KeywordSearchArgs {
    pub query: String,                          // raw FTS5 expression
    pub filter: Option<ValidatedFilter>,        // pre-validated by core
    pub visibility_allowlist: Vec<MemoryVisibility>,
    pub limit: usize,                           // hard-cap MAX_LIMIT = 200
    pub cursor: Option<KeywordCursor>,
}

pub struct KeywordSearchPage {
    pub candidates: Vec<SearchCandidate>,
    pub next_cursor: Option<KeywordCursor>,
}

pub struct SearchCandidate {
    pub record_id: RecordId,
    pub target_id: TargetId,
    pub scope: ScopeTuple,
    pub kind: MemoryKind,
    pub class: MemoryClass,
    pub visibility: MemoryVisibility,
    pub bm25: f64,
    pub recency_seconds: i64,
    pub confidence: f32,
    pub salience: f32,
    pub staleness_seconds: i64,
    pub snippet: String,
}

pub struct KeywordCursor { pub bm25: f64, pub record_id: RecordId } // base64-json wire form
```

`StoreTx<'a>` is opaque, exposes the same CRUD + edge methods as `MemoryStore`, all sync because the closure already runs on the dedicated DB thread.

`MemoryRecord` extension: add `pub target_id: TargetId`. For new facts the verb layer sets `target_id = id`; on supersession it carries the prior `target_id`. `target_id` is a typed newtype over `String` (ULID-shaped).

`BodyHash` is a typed newtype around the existing `body_hash` text representation (blake3, 64 hex chars).

### 3.1 Async strategy

The store wraps a single `tokio_rusqlite::Connection`. Each method is one `conn.call(|c| { ... })` round-trip. Reads and writes serialize through the dedicated DB thread. Trade-off accepted for P0: single-vault embedded use, no agent storms; switch to a connection pool (`deadpool-rusqlite`) at P1+ when the workflow host and Nexus need concurrency. Caller-visible API stays `async fn` either way.

`with_tx` calls `conn.call(|c| { let tx = c.transaction()?; … })`. The closure receives `&mut StoreTx`. On `Err` rollback (drop), on `Ok` commit.

### 3.2 Capability advertisement

Construction-time, evaluated once in `open()`. The flag flips reflect what the store impl actually wires, not what the schema can support:

| After PR | `fts` | `vector` | `graph_edges` | `transactions` |
|---|---|---|---|---|
| PR-A merged | `false` | `false` | `true` | `true` |
| PR-B merged | `true`  | `false` | `true` | `true` |
| #48 merged  | `true`  | `true`  | `true` | `true` |

`fts` advertises only when `search_keyword` is wired to use FTS5 — that ships in PR-B. The `records_fts` virtual table and triggers exist after `0001`, but the store does not advertise the capability until the search method exists. This keeps "advertised cap → method works" honest at every commit boundary.

Defensive guard at every cap-gated method entry returns `StoreError::CapabilityUnavailable { what: "fts" }` when the cap is off. Verb layer also checks before dispatch — defense in depth (brief §4 #6).

---

## 4. Persistence shape

### 4.1 JSON blob + denormalized hot columns

`MemoryRecord` is too wide to fully denormalize. Strategy:

- **Source of truth:** `record_json TEXT NOT NULL` — `serde_json::to_string(&record)`. Hydration reads only this column.
- **Denormalized hot columns** (already in schema or added below) used for filtering, FTS join, ranking inputs, and human-readable inspection:
  - `kind, class, visibility, scope, path` (existing) — filter targets.
  - `body` (existing) — FTS5 source, also kept for snippet generation.
  - `actor_chain` (existing, JSON) — filter target.
  - `confidence REAL`, `salience REAL` (added) — ranking inputs.
  - `target_id_explicit TEXT` (added) — supersession lineage key. Shadow column to keep `target_id` (existing) reserved for the schema's existing meaning until the migration finishes; phase out in a follow-up after all readers migrate.
  - `tags_json TEXT` (added) — JSON array, indexed for `tags` filter Leaf.
  - `tombstone_reason TEXT` (added) — distinguishes Update/Expire/Forget/Purge per #46 acceptance.
  - `body_hash` (existing) — drives idempotent upsert.

Round-trip invariant: every column's value equals the corresponding projection from `record_json`. Enforced by `hot_columns_match_json.rs` proptest.

### 4.2 Migrations

Forward-only, append to existing `0001`–`0006`:

- **`0007_tombstone_reason.sql`** — `ALTER TABLE records ADD COLUMN tombstone_reason TEXT;`. Backfill not needed (no prod data).
- **`0008_record_extensions.sql`** —
  - `ALTER TABLE records ADD COLUMN record_json TEXT NOT NULL DEFAULT '{}';`
  - `ALTER TABLE records ADD COLUMN confidence REAL NOT NULL DEFAULT 0.0;`
  - `ALTER TABLE records ADD COLUMN salience REAL NOT NULL DEFAULT 0.0;`
  - `ALTER TABLE records ADD COLUMN target_id_explicit TEXT;`
  - `ALTER TABLE records ADD COLUMN tags_json TEXT NOT NULL DEFAULT '[]';`
  - Insert `schema_migrations` row.
- **`0009_fts_metadata.sql`** (PR-B only) — drop and recreate `records_fts` to cover `body` + `path` columns, recreate the AI/AD/AU triggers to mirror both. Safe at P0 (no live vaults). Insert `schema_migrations` row.
- **`0010_ranking_indexes.sql`** —
  - `CREATE INDEX records_confidence_idx ON records(confidence) WHERE active=1 AND tombstoned=0;`
  - `CREATE INDEX records_updated_at_idx ON records(updated_at) WHERE active=1 AND tombstoned=0;`
  - Insert `schema_migrations` row.

PR-A ships `0007`, `0008`, `0010`. PR-B ships `0009`.

### 4.3 Versioning (in-place via `records.active`)

Existing schema is row-per-version, with `(target_id, version)` unique and a partial unique index (`records_active_target_idx`) enforcing one active row per target.

`upsert(record)` semantics:

1. Compute `body_hash` over the canonical body (use existing `domain::canonical`).
2. `SELECT body_hash, version, record_id FROM records WHERE target_id = ? AND active = 1` (zero or one row).
3. **No prior row:** insert new with `version = 1, active = 1`. Outcome: `content_changed: true, prior_hash: None`.
4. **Prior row, `body_hash` matches:** no-op. Outcome: `content_changed: false, version: prior.version`. Idempotent — safe for replay (#8).
5. **Prior row, `body_hash` differs:** in single tx — `UPDATE records SET active=0 WHERE record_id=prior_id`, then `INSERT … version = prior.version + 1, active = 1`. Outcome: `content_changed: true, prior_hash: Some(prior.body_hash)`.

`created_at` carries from prior row when one exists; `updated_at = now`. Caller-controlled fields write through unchanged. `body_hash` is computed by store, never trusted from caller.

`target_id` is taken from `record.target_id`. Verb layer is responsible for setting it — store treats it as opaque.

### 4.4 Tombstone

`tombstone(id, reason)`:

```sql
UPDATE records
   SET tombstoned = 1,
       tombstone_reason = ?,
       updated_at = ?
 WHERE record_id = ?
```

- Operates on the specific `record_id` (one version), not the target.
- `active` flag is unchanged — tombstoning the active row signals "this fact existed and was retracted." The `records_latest` view already filters tombstoned out.
- Idempotent: tombstoning an already-tombstoned row is a no-op (still returns `Ok(())`).

### 4.5 Edges

- `put_edge`: `INSERT OR REPLACE INTO edges(src, dst, kind, weight) VALUES(?,?,?,?)`. Triggers in `0001` enforce `updates`-edge invariants (distinct target_ids, non-tombstoned endpoints, post-insert immutability).
- `remove_edge`: `DELETE FROM edges WHERE src=? AND dst=? AND kind=?`. Returns `bool` from affected-rows. `updates` edges are immutable — attempting removal surfaces the trigger error as `StoreError::Sql`.
- `neighbours(id, dir)`:
  - `Out`: `SELECT … FROM edges WHERE src = ? AND dst IN (SELECT record_id FROM records_latest)`.
  - `In`: symmetric on `dst`.
  - `Both`: union.

### 4.6 Pragmas (idempotent re-issue at every open)

Set after migrations, on every `open()`:

- `PRAGMA journal_mode = WAL`
- `PRAGMA synchronous = NORMAL`
- `PRAGMA foreign_keys = ON`
- `PRAGMA temp_store = MEMORY`
- `PRAGMA mmap_size = 268435456` (256 MB)

---

## 5. Keyword search

### 5.1 SQL skeleton

All placeholders are positional `?` bound in this order: `now`, `query`, visibility-allowlist values (one `?` per allowed visibility), filter params (from `compile_filter`), `cursor_bm25` (or NULL), `cursor_id` (or empty string), `limit`. The store concats only the visibility placeholder count and the filter SQL fragment into the skeleton; everything else is static.

```sql
SELECT r.record_id, r.target_id_explicit, r.scope, r.kind, r.class, r.visibility,
       bm25(records_fts) AS bm25_score,
       ? - r.updated_at AS recency_seconds,
       r.confidence, r.salience,
       ? - r.updated_at AS staleness_seconds,    -- same `now` value bound twice
       snippet(records_fts, 0, '<b>', '</b>', '…', 32) AS snippet,
       r.record_json
  FROM records_fts
  JOIN records r ON r.rowid = records_fts.rowid
 WHERE records_fts MATCH ?
   AND r.active = 1
   AND r.tombstoned = 0
   AND r.visibility IN (?, ?, ...)             -- one `?` per allowlist entry
   AND <compiled_filter_sql>                    -- "TRUE" if no filter
   AND (
        ? IS NULL                                -- cursor_bm25
     OR (bm25_score, r.record_id) > (?, ?)      -- cursor_bm25 again, cursor_id
   )
 ORDER BY bm25_score, r.record_id
 LIMIT ?;
```

- `staleness_seconds` is identical to `recency_seconds` at P0. Refined later when scope-specific staleness thresholds land — store reports the raw value, ranker interprets.
- The skeleton is a single static string. The only dynamic substitutions are: visibility allowlist placeholder count, and `compiled_filter_sql` from `domain::filter::compile_filter`. Both are structural; user values bind through `params`.
- `record_json` returns alongside ranking inputs so callers can hydrate the full `MemoryRecord` without a second round-trip when needed (verb layer decides whether to deserialize).

### 5.2 Empty-query path

If `args.query.trim().is_empty()`: skip the FTS join entirely. Run a `SELECT … FROM records WHERE active=1 AND tombstoned=0 AND <visibility> AND <filter> ORDER BY updated_at DESC, record_id LIMIT :limit` and return `SearchCandidate` with `bm25 = 0.0` and a `snippet` derived from the first 80 chars of `body`. Lets callers run filter-only listings through one API.

### 5.3 Filter integration

Verb layer:
1. `let validated = validate_filter(&search_args.filters.unwrap_or(empty))?;`
2. `store.search_keyword(&KeywordSearchArgs { filter: Some(validated), … }).await`

Store:
1. `let CompiledFilter { sql, params } = compile_filter(&validated);`
2. Splice `sql` into the static skeleton; bind `params` after the visibility placeholders, then the cursor params.

Empty filter (no leaves) → `compile_filter` returns `("TRUE", vec![])` per existing API. No special-casing needed in store.

Any error from `compile_filter` after a successful `validate_filter` is a bug → `StoreError::Invariant { what: "post-validate compile failed" }`.

### 5.4 FTS5 syntax errors

A malformed `MATCH` expression fails inside SQLite as `rusqlite::Error::SqliteFailure` with a message like `fts5: syntax error near "…"`. The store inspects the message and re-wraps as `StoreError::FtsQuery { message }` so the verb layer can return a user-actionable error code rather than a generic SQL failure. Unrelated SQL errors stay `StoreError::Sql`.

### 5.5 Cursor encoding

`KeywordCursor { bm25: f64, record_id: RecordId }` is opaque on the wire — base64(json). Bytes in / bytes out; never parsed by callers. Store decodes on entry, encodes on exit. Pagination is keyset-style on `(bm25, record_id)` so result order is stable across pages.

`MAX_LIMIT = 200` enforced inside `search_keyword`. Caller asks for more → silently capped, not an error.

---

## 6. Errors

Extend the existing `StoreError`:

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    #[error("record not found: {id}")]
    NotFound { id: String },

    #[error("capability unavailable: {what}")]
    CapabilityUnavailable { what: &'static str },

    #[error("FTS5 query parse error: {message}")]
    FtsQuery { message: String },

    #[error("transaction error")]
    Tx(#[source] rusqlite::Error),

    #[error("sqlite error")]
    Sql(#[source] rusqlite::Error),

    #[error("background sqlite worker died")]
    Worker(#[source] tokio_rusqlite::Error),

    #[error("record codec error")]
    Codec(#[source] serde_json::Error),

    #[error("invariant violated: {what}")]
    Invariant { what: String },

    // existing: Migration { … } and any others kept as-is
}
```

`From<rusqlite::Error>` for `StoreError::Sql` for `?` ergonomics. FTS-parse errors are detected by `SqliteFailure` message-prefix (`fts5: ` or `unknown special query`) and re-wrapped before they leave `search_keyword`.

---

## 7. Tracing

Per CLAUDE.md §6.6, every trait-method impl is instrumented:

```rust
#[tracing::instrument(
    skip(self, record),
    err,
    fields(verb = "upsert", record_id = %record.id, target_id = %record.target_id)
)]
async fn upsert(&self, record: &MemoryRecord) -> Result<UpsertOutcome, StoreError> { … }
```

Field discipline:
- `body`, `record_json`, `snippet`, `query` — never logged above `trace`.
- `query` field on `search_keyword` is logged as `query.len = N`, not the string.
- `kind`, `class`, `visibility`, `scope`, `record_id`, `target_id` — fine at `info`.

---

## 8. Tests

Layout: integration tests in `crates/cairn-store-sqlite/tests/`. Helpers in `cairn-test-fixtures` (dev-only).

### 8.1 PR-A (#46)

- `crud_roundtrip.rs` — upsert → get → list → tombstone → versions for randomized `MemoryRecord` shapes. Asserts `record_json` round-trips bit-for-bit.
- `upsert_idempotent.rs` — proptest, 1000 iterations: same canonical body_hash → no version bump → `content_changed: false`. Different body → `content_changed: true` + version bump.
- `versioning.rs` — content change bumps `version`, flips `active=0` on prior, prior row reachable via `versions(target)`.
- `tombstone_reasons.rs` — each `TombstoneReason` writes the correct column value; `records_latest` excludes tombstoned. Tombstone is idempotent (proptest).
- `edges_crud.rs` — `put_edge` / `remove_edge` / `neighbours(Out|In|Both)`. `updates`-edge immutability surfaces as `StoreError::Sql` containing the trigger message.
- `tx_rollback.rs` — `with_tx` returning `Err` rolls back both record upserts and edge inserts inside the closure.
- `hot_columns_match_json.rs` — proptest, 1000 iterations: every denormalized column equals its `record_json` projection.

### 8.2 PR-B (#47)

- `search_keyword_basic.rs` — insert N records, query, assert `bm25` ordering, snippet present.
- `search_keyword_filters.rs` — visibility/scope/kind/class filter combinations exercise `compile_filter` end-to-end.
- `search_keyword_supersession.rs` — superseded (`updates`-edge dst) and tombstoned rows never returned.
- `search_keyword_cursor.rs` — pagination round-trip; `next_cursor` resumes deterministically across pages.
- `search_keyword_empty_query.rs` — empty query → filter-only listing ordered by `updated_at DESC`.
- `search_keyword_fts_errors.rs` — malformed FTS5 query → `StoreError::FtsQuery`, not `Sql`.
- `search_keyword_injection.rs` — Leaf values containing `%`, `'`, `;`, `--`, NUL byte, unicode escapes never break SQL or escape the parameterization. Validates the `compile_filter` contract from the consumer side.
- `capability_caps_off.rs` — synthetic store with `fts=false` cap → `search_keyword` returns `CapabilityUnavailable` without touching SQL.

### 8.3 Existing tests preserved

`migrations.rs`, `manifest_validates.rs`, `wal_fsm.rs`, `drift_corner_cases.rs`, `records_latest.rs`, `smoke.rs` — all keep passing. `records_latest.rs` raw-SQL fixtures need `record_json='{}'` injected once `0008` lands; update in the same PR.

### 8.4 Fixture helpers (`cairn-test-fixtures`)

Add (dev-only):

- `pub fn sample_record(seed: u64) -> MemoryRecord` — deterministic record with rotating kind/class/visibility/scope.
- `pub async fn tempstore() -> (TempDir, SqliteMemoryStore)` — file-backed store in a temp vault.
- `pub async fn memstore() -> SqliteMemoryStore` — in-memory store for fast cases.
- `pub fn keyword_args(query: &str) -> KeywordSearchArgs` — minimal builder.

---

## 9. Verification

Mirrors CLAUDE.md §8. Per-PR subset:

```bash
cargo fmt --all --check
cargo clippy -p cairn-core -p cairn-store-sqlite -p cairn-test-fixtures \
    --all-targets --locked -- -D warnings
cargo nextest run -p cairn-core -p cairn-store-sqlite --locked --no-fail-fast
cargo test --doc -p cairn-core -p cairn-store-sqlite --locked
./scripts/check-core-boundary.sh
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" \
    cargo doc -p cairn-core -p cairn-store-sqlite --no-deps --document-private-items --locked
```

Full workspace check before opening either PR:

```bash
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

`cargo deny check` + `cargo audit` only when `Cargo.toml` deps change. Adding `tokio_rusqlite` triggers it for PR-A.

---

## 10. PR sequence

1. **PR-A** — trait extension (`cairn-core`, contract version bump 0.1 → 0.2), `MemoryRecord.target_id` field, migrations `0007` + `0008` + `0010`, `tokio_rusqlite` integration in `open.rs`, all CRUD/edge/tx methods + tests. Cap flags after merge: `fts=false, vector=false, graph_edges=true, transactions=true`. Medium-large diff.
2. **PR-B** — migration `0009` (FTS metadata), `search_keyword` impl + tests, flip `fts=true`. Smaller diff. Depends on PR-A.

Each PR opens with: link to this spec, brief sections cited, invariants touched, verification output pasted. Plans for both PRs are written together via the writing-plans skill once this spec is approved.

---

## 11. Risks and follow-ups

- **`tokio_rusqlite` version pinning** — review the latest stable release; pin tightly. If it isn't already a workspace dep, adding it is justified in the PR.
- **`MemoryRecord.target_id` extension** — ripples into anything that constructs `MemoryRecord` today (verbs, fixtures, ingest path). PR-A surveys callers and updates them; default-derive `target_id = id` for fresh records is acceptable scaffolding only inside test fixtures, not in production code paths.
- **`target_id_explicit` shadow column** — kept as a transitional name to avoid colliding with any pre-existing semantic for `records.target_id`. After PR-A lands, file a follow-up to consolidate; do not block on it.
- **FTS5 drop+recreate in `0009`** — safe at P0 because no live vaults exist. Document in the migration that this is a P0-only allowance; later FTS schema changes use online table-rebuild patterns.
- **Search ranking** — store returns ranking inputs only. The `Ranker` pure function in `cairn-core` (brief §5.1) is a separate concern; this spec does not implement it. If ranking surfaces later need a field this spec did not include, extend `SearchCandidate` and bump trait version.
- **Concurrency** — single-connection serialization is a known throughput ceiling. Acceptable for P0; P1 swap to a pool is API-compatible.
