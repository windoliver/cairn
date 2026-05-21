# Bitemporal Knowledge-Graph Schema (Issue #186)

**Status:** Draft — pending implementation
**Date:** 2026-05-02
**Issue:** [#186](https://github.com/windoliver/cairn/issues/186)
**Brief sections:** §3 (vault), §4 (MemoryStore contract), §5.6 (WAL two-phase apply)
**Depends on:** #46 (closed — MemoryStore CRUD landed), #55 (open — generic WAL driver; this PR writes correctly-shaped WAL rows that #55's recovery driver will later resume)

---

## 1. Goal & Non-Goals

### Goal

Add a bitemporal entity-level knowledge-graph substrate to `cairn-store-sqlite`:

- Entity nodes deduplicated by normalized name.
- Bitemporal edges with both *event-time* (`valid_at` / `invalid_at`) and *ingestion-time* (`created_at` / `expired_at`) windows.
- Three confidence tiers per edge: `EXTRACTED`, `INFERRED`, `AMBIGUOUS`.
- Contradiction-resolution flow that invalidates a stale edge and inserts the new one in one atomic, WAL-tracked operation.
- Episode join table linking entities to the records that mention them.

The substrate is the foundation for `ingest --folder` (entity extraction), graph-aware search, contradiction detection in `lint`, and temporal retrieval ("what did we know at time T?").

### Non-goals

The following are deliberately deferred to follow-up issues:

- Entity-vector embedder and ANN search over entities. (`embedding_id` column lands nullable; column is wired, embedder is not.)
- MCP graph-traversal tools.
- `lint` contradiction-detection check (will use the trait surface added here).
- `ingest --folder` entity extraction (will use the trait surface added here).
- Boot recovery for in-progress graph WAL ops — #55's scope.
- Generic Rust WAL driver API in `cairn-core` — #55's scope.

---

## 2. Architecture

Adds entity-graph as the **second concrete consumer** of the generic `wal_ops` + `wal_steps` schema landed in migration 0002. Identity-registry mutations (#50) are the first consumer; entity-graph mutations are the second. Two domain consumers of the same on-disk WAL schema is the forcing function for #55 to extract a reusable Rust driver.

### Crate boundaries

| Crate | Change |
|---|---|
| `cairn-core` | New domain types in `domain/graph/`; five new methods on `MemoryStore` trait, all with default impls returning `Err("capability unavailable: bitemporal_graph")`. Bumps `MemoryStore` `CONTRACT_VERSION` from 0.3.0 → 0.4.0. **No new dependencies.** |
| `cairn-store-sqlite` | Four new SQL migrations (0031–0034); new `entity_graph/` module implementing the five trait methods; in-tx writes to `wal_ops` + `wal_steps` mirroring the `identity/wal.rs` pattern. |
| `cairn-cli`, `cairn-mcp`, `cairn-sdk`, `cairn-workflows`, `cairn-sensors-local`, `cairn-test-fixtures` | No source changes. `cairn-test-fixtures::FixtureStore` keeps the default `Err` impls (it doesn't need to advertise the capability). `cairn status` JSON output unchanged — verified via existing snapshot test. |

### Capability flag

`MemoryStoreCapabilities` struct is **unchanged**. The new methods follow the existing pattern set by `search_semantic`, `search_hybrid`, and `index_stats` — default impls return a typed "capability unavailable" error and concrete adapters override. This keeps `cairn status` byte-identical and avoids overloading the existing `graph_edges: bool` flag (which covers record-level edges, a distinct substrate).

---

## 3. SQL Schema (4 migrations, append-only)

All migrations are append-only per CLAUDE.md §6.11. Numbered to follow the latest committed migration (0030).

### 3.1 `0031_wal_kind_widening.sql`

Extends the `wal_ops.kind` CHECK constraint to add five graph kinds:

- `graph_upsert_entity`
- `graph_upsert_edge`
- `graph_contradict`
- `graph_tombstone`
- `graph_link_episode`

SQLite CHECK constraints are immutable, so this migration table-rebuilds `wal_ops`:

1. `CREATE TABLE wal_ops_new` with the widened CHECK and identical other columns.
2. `INSERT INTO wal_ops_new SELECT * FROM wal_ops`.
3. Drop the existing triggers (`wal_ops_state_transition`, `wal_ops_envelope_immutable`, `wal_ops_terminal_immutable`, `wal_ops_no_delete`, `wal_ops_issued_seq_must_advance`) and the `wal_ops_open_idx` index.
4. `DROP TABLE wal_ops`; `ALTER TABLE wal_ops_new RENAME TO wal_ops`.
5. Recreate every trigger and index on the new table verbatim.
6. Stamp the migrations table.

The `wal_op_deps` and `wal_steps` FK references survive the rename because SQLite resolves FKs by name on access — the rebuild explicitly preserves the table name.

### 3.2 `0032_entity_nodes.sql`

```sql
CREATE TABLE entity_nodes (
    id              TEXT PRIMARY KEY,           -- ULID, base32
    name            TEXT NOT NULL,
    name_norm       TEXT NOT NULL UNIQUE,       -- lowercase, punctuation-stripped
    summary         TEXT,
    created_at      INTEGER NOT NULL,           -- unix ms ingestion time
    expired_at      INTEGER,                    -- NULL = live
    tombstone_reason TEXT,                      -- required when expired_at set
    embedding_id    TEXT                        -- nullable; future FK to entity_vectors
);
CREATE INDEX entity_nodes_name_norm_idx ON entity_nodes(name_norm);

-- FTS5 mirror over name + summary
CREATE VIRTUAL TABLE entity_nodes_fts USING fts5(
    name, summary,
    content='entity_nodes',
    content_rowid='rowid'
);

-- shrink-guard: no silent expiry without an explicit reason
CREATE TRIGGER entity_nodes_shrink_guard
  BEFORE UPDATE OF expired_at ON entity_nodes
  FOR EACH ROW
  WHEN NEW.expired_at IS NOT NULL AND NEW.tombstone_reason IS NULL
BEGIN
  SELECT RAISE(ABORT, 'entity_nodes.expired_at requires tombstone_reason');
END;

-- FTS sync triggers on INSERT / UPDATE / DELETE
CREATE TRIGGER entity_nodes_fts_ai AFTER INSERT ON entity_nodes BEGIN
  INSERT INTO entity_nodes_fts(rowid, name, summary)
  VALUES (NEW.rowid, NEW.name, NEW.summary);
END;
CREATE TRIGGER entity_nodes_fts_au AFTER UPDATE ON entity_nodes BEGIN
  INSERT INTO entity_nodes_fts(entity_nodes_fts, rowid, name, summary)
  VALUES ('delete', OLD.rowid, OLD.name, OLD.summary);
  INSERT INTO entity_nodes_fts(rowid, name, summary)
  VALUES (NEW.rowid, NEW.name, NEW.summary);
END;
CREATE TRIGGER entity_nodes_fts_ad AFTER DELETE ON entity_nodes BEGIN
  INSERT INTO entity_nodes_fts(entity_nodes_fts, rowid, name, summary)
  VALUES ('delete', OLD.rowid, OLD.name, OLD.summary);
END;
```

### 3.3 `0033_entity_edges.sql`

```sql
CREATE TABLE entity_edges (
    id                TEXT PRIMARY KEY,         -- ULID
    source_id         TEXT NOT NULL REFERENCES entity_nodes(id),
    target_id         TEXT NOT NULL REFERENCES entity_nodes(id),
    relation          TEXT NOT NULL,
    confidence        TEXT NOT NULL CHECK(confidence IN ('EXTRACTED','INFERRED','AMBIGUOUS')),
    confidence_score  REAL NOT NULL CHECK(confidence_score BETWEEN 0.0 AND 1.0),
    valid_at          INTEGER NOT NULL,         -- event-time start (unix ms)
    invalid_at        INTEGER,                  -- event-time end; NULL = currently valid
    created_at        INTEGER NOT NULL,         -- ingestion-time start
    expired_at        INTEGER,                  -- ingestion-time end; NULL = live
    tombstone_reason  TEXT,                     -- required when expired_at set
    source_record_id  TEXT REFERENCES records(id) ON DELETE SET NULL,
    body_hash         BLOB NOT NULL             -- blake3 over (confidence, score, valid_at, source_record_id)
);

-- exactly one live edge per (source, target, relation)
CREATE UNIQUE INDEX entity_edges_live_triple
  ON entity_edges(source_id, target_id, relation)
  WHERE invalid_at IS NULL AND expired_at IS NULL;

-- temporal query indexes
CREATE INDEX entity_edges_valid_at_idx     ON entity_edges(valid_at);
CREATE INDEX entity_edges_invalid_at_idx   ON entity_edges(invalid_at) WHERE invalid_at IS NOT NULL;
CREATE INDEX entity_edges_source_relation_idx ON entity_edges(source_id, relation);
CREATE INDEX entity_edges_target_relation_idx ON entity_edges(target_id, relation);

-- shrink-guard
CREATE TRIGGER entity_edges_shrink_guard
  BEFORE UPDATE OF expired_at ON entity_edges
  FOR EACH ROW
  WHEN NEW.expired_at IS NOT NULL AND NEW.tombstone_reason IS NULL
BEGIN
  SELECT RAISE(ABORT, 'entity_edges.expired_at requires tombstone_reason');
END;
```

### 3.4 `0034_entity_episodes.sql`

```sql
CREATE TABLE entity_episodes (
    episode_id     TEXT NOT NULL REFERENCES records(id) ON DELETE CASCADE,
    entity_node_id TEXT NOT NULL REFERENCES entity_nodes(id) ON DELETE CASCADE,
    linked_at      INTEGER NOT NULL,
    PRIMARY KEY (episode_id, entity_node_id)
);
CREATE INDEX entity_episodes_entity_idx ON entity_episodes(entity_node_id);
```

`ON DELETE CASCADE` on `episode_id` is intentional and asymmetric to `entity_edges.source_record_id` (which uses SET NULL): the episode-link is a pure derivation with no audit value once its record is gone, whereas an entity edge is a derived *fact* that should outlive its specific evidence.

---

## 4. Domain Types (`cairn-core`)

New module `crates/cairn-core/src/domain/graph/mod.rs`:

```rust
//! Bitemporal knowledge-graph types (brief §3, §4).

use std::time::SystemTime;
use crate::domain::record::RecordId;

/// Stable ULID for an entity node. Distinct from RecordId.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EntityId(String);

/// Stable ULID for an entity edge.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EntityEdgeId(String);

/// Confidence tier on an extracted edge (Graphify model).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeConfidence {
    /// Directly present in the source. score = 1.0.
    Extracted,
    /// Reasonable LLM inference (structural evidence). score 0.6–0.9.
    Inferred,
    /// Uncertain; flagged for `lint` review. score 0.1–0.3.
    Ambiguous,
}

#[derive(Debug, Clone)]
pub struct EntityNode {
    pub id:           EntityId,
    pub name:         String,
    pub name_norm:    String,
    pub summary:      Option<String>,
    pub created_at:   i64,
    pub embedding_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EntityEdge {
    pub id:               EntityEdgeId,
    pub source_id:        EntityId,
    pub target_id:        EntityId,
    pub relation:         String,
    pub confidence:       EdgeConfidence,
    pub confidence_score: f32,
    pub valid_at:         i64,
    pub invalid_at:       Option<i64>,
    pub created_at:       i64,
    pub source_record_id: Option<RecordId>,
}

/// Edge-direction selector for graph_edges queries.
/// Reuses the existing EdgeDir enum from contract::memory_store.
pub use crate::contract::memory_store::EdgeDir;

#[derive(Debug, Clone)]
pub struct GraphEdgesArgs<'a> {
    pub node_id:            &'a EntityId,
    pub direction:          EdgeDir,
    pub relation_filter:    Option<&'a str>,
    pub as_of_event_time:   Option<i64>,
    pub as_of_ingest_time:  Option<i64>,
    pub include_invalidated: bool,
}

/// Result of upsert_entity_edge / resolve_contradiction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityEdgeOutcome {
    pub new_edge_id:         EntityEdgeId,
    pub invalidated_edge_id: Option<EntityEdgeId>,
    pub body_was_unchanged:  bool,
}
```

`EntityId` and `EntityEdgeId` derive `From<&str>` / `Display` (per CLAUDE.md §6.10 newtype rules) — no `unwrap`s.

---

## 5. Trait Surface (`MemoryStore`)

Five new methods added to `crates/cairn-core/src/contract/memory_store.rs`. Every method has a default impl that returns `Err("capability unavailable: bitemporal_graph")` so existing adapters (`FixtureStore`) compile unchanged. The production `SqliteMemoryStore` overrides all five.

```rust
// In trait MemoryStore { ... }

/// Upsert an entity node. Deduplication happens at the `name_norm` level:
/// if a row with this name_norm already exists, return its `EntityId`;
/// otherwise insert a fresh row and return the new id. Idempotent.
async fn upsert_entity(&self, node: &EntityNode) -> Result<EntityId, StoreError> {
    let _ = node;
    Err("capability unavailable: bitemporal_graph".into())
}

/// Upsert an entity edge. If a live edge for (source, target, relation)
/// exists with a different body_hash, contradiction-resolves: invalidates
/// the old edge and inserts the new one in one atomic op. Identical-body
/// re-upsert is a no-op (`body_was_unchanged: true`, no WAL row written).
async fn upsert_entity_edge(&self, edge: &EntityEdge)
    -> Result<EntityEdgeOutcome, StoreError> {
    let _ = edge;
    Err("capability unavailable: bitemporal_graph".into())
}

/// Read edges adjacent to a node. Supports direction (in/out/both),
/// relation-filter, and as-of bitemporal slicing.
async fn graph_edges(&self, args: &GraphEdgesArgs<'_>)
    -> Result<Vec<EntityEdge>, StoreError> {
    let _ = args;
    Err("capability unavailable: bitemporal_graph".into())
}

/// Explicit contradiction resolution. Invalidates `old_edge_id` and inserts
/// `new_edge` in one atomic op. Mostly an internal hook used by
/// upsert_entity_edge; exposed for batch-contradiction callers (e.g. lint).
async fn resolve_contradiction(
    &self, old_edge_id: &EntityEdgeId, new_edge: &EntityEdge,
) -> Result<EntityEdgeOutcome, StoreError> {
    let _ = (old_edge_id, new_edge);
    Err("capability unavailable: bitemporal_graph".into())
}

/// Link an entity to a record that mentions it. Idempotent — returns
/// `Ok(true)` when a new link row was inserted, `Ok(false)` when the link
/// already existed.
async fn link_entity_episode(
    &self, entity_id: &EntityId, record_id: &RecordId,
) -> Result<bool, StoreError> {
    let _ = (entity_id, record_id);
    Err("capability unavailable: bitemporal_graph".into())
}
```

`CONTRACT_VERSION` bumps from 0.3.0 → 0.4.0.

---

## 6. Contradiction-Resolution Flow

`upsert_entity_edge(new)` in `cairn-store-sqlite::entity_graph::edge`:

1. Compute `body_hash = blake3(confidence ‖ confidence_score.to_le_bytes() ‖ valid_at.to_le_bytes() ‖ source_record_id.unwrap_or(""))`.
2. Open a SQLite write transaction.
3. `SELECT id, body_hash FROM entity_edges WHERE source_id=? AND target_id=? AND relation=? AND invalid_at IS NULL AND expired_at IS NULL`.
4. **No hit** → fresh insert:
   - Insert one `wal_ops` row, `kind='graph_upsert_edge'`, `state='ISSUED'`.
   - Transition `state` ISSUED → PREPARED.
   - Insert one `wal_steps` row, `step_ord=0`, `step_kind='insert_edge'`, `state='PENDING'`, with `pre_image=NULL`.
   - `INSERT INTO entity_edges (...)`.
   - Transition step PENDING → DONE.
   - Transition op PREPARED → COMMITTED.
   - Commit tx. Return `EntityEdgeOutcome { new_edge_id: <new>, invalidated: None, body_was_unchanged: false }`.
5. **Hit, identical `body_hash`** → idempotent no-op. Commit tx (no WAL rows written, no entity-table writes). Return `EntityEdgeOutcome { new_edge_id: <existing.id>, invalidated: None, body_was_unchanged: true }`. Mirrors `MemoryStore::upsert`'s body-hash idempotency.
6. **Hit, different `body_hash`** → contradiction:
   - Insert one `wal_ops` row, `kind='graph_contradict'`, transition to PREPARED.
   - Insert two `wal_steps` rows: `step_ord=0` (`step_kind='invalidate_edge'`, `pre_image=<serialized old edge>`), `step_ord=1` (`step_kind='insert_edge'`).
   - `UPDATE entity_edges SET invalid_at = NEW.valid_at WHERE id = <old.id>`.
   - Transition step 0 PENDING → DONE.
   - `INSERT INTO entity_edges (...)` for new edge.
   - Transition step 1 PENDING → DONE.
   - Transition op PREPARED → COMMITTED.
   - Commit tx. Return `EntityEdgeOutcome { new_edge_id: <new>, invalidated: Some(<old.id>), body_was_unchanged: false }`.

Every WAL state transition is gated by the existing triggers in migration 0002. A crash mid-transaction rolls everything back atomically; a crash *after* commit leaves a fully-applied terminal state. **In-progress recovery (crash between PREPARED and COMMITTED at the SQL layer is impossible because we use one tx; recovery for higher-level multi-tx flows is #55's scope.)**

`resolve_contradiction(old, new)` is a thin wrapper that runs steps 6.* directly (assumes the caller verified the contradiction). Used by `lint --fix-graph` to invalidate a specific known-bad edge.

---

## 7. Module Layout (`cairn-store-sqlite`)

```
crates/cairn-store-sqlite/src/
├── entity_graph/
│   ├── mod.rs            -- pub fn surface; re-exports
│   ├── node.rs           -- upsert_entity, name_norm dedup
│   ├── edge.rs           -- upsert_entity_edge, contradiction flow, body_hash
│   ├── episode.rs        -- link_entity_episode
│   ├── query.rs          -- graph_edges with as-of slicing
│   └── wal.rs            -- helpers: insert wal_ops + wal_steps in current tx
├── store/
│   └── trait_impl.rs     -- adds entity_graph delegations to MemoryStore impl
└── lib.rs                -- pub mod entity_graph;
```

`entity_graph/wal.rs` is the equivalent of `identity/wal.rs` but writes to the generic `wal_ops` + `wal_steps` tables (which already support multi-step ops via `step_ord` and `pre_image`).

---

## 8. Testing

Per CLAUDE.md §6.4: integration tests in `crates/cairn-store-sqlite/tests/entity_graph.rs` against real in-memory SQLite via `cairn-test-fixtures`. Plus one snapshot test in the existing `cairn-cli` status snapshot suite to verify `cairn status` JSON is unchanged.

| # | Test | What it proves |
|---|---|---|
| 1 | `migrations_apply_cleanly` | All four new migrations apply on a fresh DB; schema matches snapshot |
| 2 | `wal_kind_widening_preserves_existing_rows` | Pre-existing `wal_ops` rows survive 0031 table rebuild with all FKs/triggers intact |
| 3 | `cairn_status_capabilities_unchanged` (snapshot test, lives in cairn-cli) | `MemoryStoreCapabilities` JSON is byte-identical to pre-migration |
| 4 | `upsert_entity_inserts_new` | New entity returns fresh ULID; row is in `entity_nodes` |
| 5 | `upsert_entity_dedup_by_name_norm` | Second upsert with same `name_norm` returns existing id; no second row |
| 6 | `upsert_entity_edge_simple_insert` | First edge of a triple inserts cleanly; outcome reports `body_was_unchanged=false`, `invalidated=None` |
| 7 | `upsert_entity_edge_idempotent_reupsert` | Identical body re-upsert is no-op; no new `wal_ops` row; outcome `body_was_unchanged=true` |
| 8 | `upsert_entity_edge_contradiction_invalidates_old` | Live edge with different body → old has `invalid_at` set; new edge is live; both visible via `graph_edges(include_invalidated=true)` |
| 9 | `shrink_guard_rejects_silent_expiry` | `UPDATE entity_edges SET expired_at = ? WHERE id = ?` (without `tombstone_reason`) → trigger ABORTs |
| 10 | `fk_set_null_on_record_delete` | Insert edge with `source_record_id`; delete that record; edge survives with NULL FK |
| 11 | `link_entity_episode_idempotent` | First link returns `Ok(true)`; second link with same pair returns `Ok(false)`; one row in `entity_episodes` |
| 12 | `graph_edges_direction_in_out_both` | Three sub-cases — direction filter returns correct edge sets |
| 13 | `graph_edges_relation_filter` | Only edges matching `relation_filter` returned |
| 14 | `graph_edges_as_of_event_time` | At `t = valid_at - 1`, edge is absent. At `t = valid_at`, present. After `invalid_at`, absent (unless `include_invalidated`). |

The contradiction WAL trail (tests 6, 7, 8) is verified by querying `wal_ops` and `wal_steps` for the expected row counts, kinds, and terminal states.

---

## 9. Verification Checklist (CLAUDE.md §8)

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh           # no new deps in cairn-core
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check  # status snapshot unchanged
mdbook build docs/site
```

Plus on PR description: cite §3 (vault), §4 (MemoryStore contract), §5.6 (WAL) per CLAUDE.md §9; list the trait `CONTRACT_VERSION` bump (0.3.0 → 0.4.0) and the schema migrations (0031–0034); paste the verification output.

---

## 10. Open Risks & Mitigations

| Risk | Mitigation |
|---|---|
| 0031 wal_ops table-rebuild loses data or trigger semantics | Test 2 covers preservation; rebuild is run inside a single migration transaction; rebuild script copies every trigger and index verbatim |
| `body_hash` over a partial field set hides a real change | Document the included fields in code; if more fields become semantically load-bearing later, body_hash must be recomputed under a new column name and a follow-up migration must rebuild edges (treated as a brief-level change) |
| `resolve_contradiction` exposed without a strong contract for who calls it | Doc-comment it as "internal hook + lint escape hatch"; no CLI surface in this PR |
| "single live edge per (source, target, relation)" too restrictive for cardinality-many relations | Spec acknowledges this in Q7 brainstorm; v0.1 accepts the limitation; richer-cardinality follow-up will widen the partial UNIQUE to include `source_record_id` or add a `cardinality` column |
| Integration-test flakiness from FTS5 trigger ordering on entity_nodes | Use `INSERT OR IGNORE` semantics and pin trigger order; existing record FTS triggers are the reference pattern |
| #55 lands and changes the WAL row shape after this PR ships | This PR writes the canonical wal_ops/wal_steps shape from migration 0002. #55 introduces a Rust driver, not a schema change. If #55 needs a column we don't write, both consumers (identity, entity-graph) will be migrated together. |

---

## 11. Follow-ups (separate issues)

- Entity vector embedder + `entity_vectors` sqlite-vec table + ANN search (depends on #48 patterns)
- `cairn-mcp` graph-traversal tools using `graph_edges`
- `lint` contradiction-detection check using `resolve_contradiction`
- `ingest --folder` entity extraction using `upsert_entity` + `link_entity_episode`
- Boot recovery for in-progress graph WAL ops (consumed by #55's driver)
- Generic Rust WAL driver in `cairn-core` (#55)
