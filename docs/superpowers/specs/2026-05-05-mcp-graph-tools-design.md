# MCP graph traversal tools — design

**Issue:** [#190](https://github.com/windoliver/cairn/issues/190)
**Brief sections:** §4 (MCPServer contract), §6.12 (MCP — schemars, stdio transport, wire compat), §8.0.a (capability advertisement)
**Date:** 2026-05-05

## 1. Goal

Expose Cairn's entity knowledge graph as first-class MCP tools so harnessed
agents can traverse and explore the graph directly — without going through
the 8-verb `MemoryRecord` layer. All queries are pure SQLite, offline,
deterministic, no LLM calls.

Five tools:

| Name | Purpose |
|---|---|
| `graph.query` | BFS/DFS from a seed entity within hop + token budget |
| `graph.get_entity` | Exact entity lookup by id or name; live edge count |
| `graph.get_neighbors` | 1-hop neighborhood with optional relation/confidence filter |
| `graph.timeline` | All edges for an entity ordered by `valid_at` |
| `graph.surprising_connections` | Cross-scope, high-confidence edges between a set of entities |

## 2. Architecture

### 2.1 Module placement

- **`crates/cairn-store-sqlite/src/entity_graph/queries.rs`** (new). All
  read-only graph traversal logic. `GraphQueries` struct holding `&Pool`
  (or whichever connection handle the existing graph modules use).
  Methods return plain Rust structs (`GraphNode`, `GraphEdge`,
  `TimelineEntry`, `SurpriseHit`) — no MCP types leak into the store crate.
- **`crates/cairn-mcp/src/graph_tools.rs`** (new). schemars-derived
  `*Args` input types for the 5 tools. `GRAPH_TOOLS:
  &'static [GraphToolDecl]` registry parallel to the IDL-generated
  `TOOLS`. A dispatch function maps tool name → `GraphQueries` method →
  serialized `CallToolResult`.
- **`crates/cairn-mcp/src/handler.rs`** (edit). Extend `list_tools` to
  concat `TOOLS` + `GRAPH_TOOLS`. Extend `call_tool` to route graph names
  before the existing search/stub paths. Reuses the existing
  `Option<Arc<dyn MemoryStore>>` field.
- **`crates/cairn-cli/src/...mcp dispatch`** (edit). Pass the open store
  into `CairnMcpServer` so graph tools have a backend. Today
  `serve_stdio` always builds an unwired handler; introduce
  `serve_stdio_with_store(store, config)` and call it from the CLI.

Why A (chosen over alternatives B/C from brainstorm): graph queries are
SQLite-shaped (recursive CTEs), have no business logic to share across
surfaces, and are MCP-only per issue scope. Adding an 8th contract is
brief-level work and overkill for read-only queries.

### 2.2 Schema dependencies

Schema already landed in #186:

- `entity_nodes(id, name, name_norm UNIQUE, summary, created_at,
  expired_at, tombstone_reason, embedding_id)`
- `entity_edges(id, source_id, target_id, relation, confidence,
  confidence_score, valid_at, invalid_at, created_at, expired_at,
  tombstone_reason, source_record_id, body_hash)`
- Indexes: `entity_edges_source_relation_idx`,
  `entity_edges_target_relation_idx`, `entity_edges_valid_at_idx`.

No new migration required.

## 3. Query semantics

All queries default to:

- `expired_at IS NULL` (exclude tombstoned rows).
- For point-in-time views: `valid_at <= now AND (invalid_at IS NULL OR
  invalid_at > now)`.

### 3.1 `graph.get_entity`

```sql
SELECT id, name, summary, created_at
FROM entity_nodes
WHERE (id = ?1 OR name_norm = lower(?1))
  AND expired_at IS NULL
LIMIT 1;
```

Live edge count via secondary query:

```sql
SELECT COUNT(*) FROM entity_edges
WHERE (source_id = ?1 OR target_id = ?1)
  AND expired_at IS NULL AND invalid_at IS NULL;
```

### 3.2 `graph.get_neighbors`

```sql
SELECT e.id, e.source_id, e.target_id, e.relation,
       e.confidence_score, e.valid_at
FROM entity_edges e
WHERE (e.source_id = ?1 OR e.target_id = ?1)
  AND e.expired_at IS NULL
  AND (?2 IS NULL OR e.relation = ?2)
  AND (?3 IS NULL OR e.confidence_score >= ?3);
```

Returned edges include the *other* node's id + name (joined).

### 3.3 `graph.query` (BFS/DFS)

```sql
WITH RECURSIVE frontier(node_id, depth, parent_edge) AS (
  SELECT id, 0, NULL FROM entity_nodes
  WHERE (id = ?1 OR name_norm = lower(?1)) AND expired_at IS NULL
  UNION ALL
  SELECT
    CASE WHEN e.source_id = f.node_id THEN e.target_id ELSE e.source_id END,
    f.depth + 1,
    e.id
  FROM frontier f
  JOIN entity_edges e
    ON (e.source_id = f.node_id OR e.target_id = f.node_id)
   AND e.expired_at IS NULL AND e.invalid_at IS NULL
  WHERE f.depth < ?2
)
SELECT DISTINCT node_id, depth, parent_edge FROM frontier
ORDER BY depth ASC, node_id;
```

- BFS uses level-order (depth ASC) — implicit from recursion.
- DFS reorders the result set client-side (depth-first stack walk over
  the same materialized rows). SQLite's `WITH RECURSIVE` does not
  guarantee DFS ordering directly.
- `max_hops` capped at 5 in the input schema.
- Token budget enforced in Rust: serialize each emitted node/edge with
  `serde_json::to_string`, accumulate `s.len()`, stop when accumulator
  exceeds `token_budget` (rough byte-budget proxy; documented as
  approximate). Default 4000 from issue.

### 3.4 `graph.timeline`

```sql
SELECT id, source_id, target_id, relation, confidence_score,
       valid_at, invalid_at, created_at, expired_at,
       tombstone_reason, source_record_id
FROM entity_edges
WHERE (source_id = ?1 OR target_id = ?1)
  AND (?2 = 1 OR expired_at IS NULL)
ORDER BY valid_at ASC, created_at ASC;
```

`?2 = include_expired` (bool → 0/1). Stable secondary sort by
`created_at` for deterministic output when multiple edges share
`valid_at`.

### 3.5 `graph.surprising_connections`

Score: `confidence_score * (1 + cross_scope_bonus)`.

Cross-scope bonus = 1.0 if the edge's `source_record_id` belongs to a
different ingestion provenance chain than the seed entities' typical
edges; 0.0 otherwise.

P0 simplification: cross-scope = "edge's `source_record_id` is distinct
from the modal `source_record_id` across the input set's other edges."
Implemented as a CTE that computes the modal record per entity, then
flags edges whose `source_record_id` differs.

```sql
WITH input(id) AS (VALUES (?1), (?2), ...),
     modal_record AS (
       SELECT i.id AS entity_id, e.source_record_id AS rec, COUNT(*) AS n
       FROM input i JOIN entity_edges e
         ON (e.source_id = i.id OR e.target_id = i.id)
        AND e.expired_at IS NULL
       GROUP BY i.id, e.source_record_id
     ),
     scored AS (
       SELECT e.*,
              e.confidence_score *
              (1.0 + CASE WHEN /* modal mismatch */ THEN 1.0 ELSE 0.0 END) AS score
       FROM entity_edges e
       WHERE e.source_id IN input AND e.target_id IN input
         AND e.expired_at IS NULL AND e.invalid_at IS NULL
     )
SELECT * FROM scored ORDER BY score DESC LIMIT ?n;
```

(Final SQL refines modal join — sketch above; the implementation may use
`Vec<RecordId>` accumulation in Rust if the SQL gets unwieldy.)

## 4. MCP wiring

### 4.1 Registry shape

```rust
pub struct GraphToolDecl {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: &'static OnceLock<Vec<u8>>, // schemars JSON bytes
}

pub static GRAPH_TOOLS: &[GraphToolDecl] = &[
    GraphToolDecl { name: "graph.query", ... },
    GraphToolDecl { name: "graph.get_entity", ... },
    GraphToolDecl { name: "graph.get_neighbors", ... },
    GraphToolDecl { name: "graph.timeline", ... },
    GraphToolDecl { name: "graph.surprising_connections", ... },
];
```

Schema bytes generated lazily via `schemars::schema_for!(QueryGraphArgs)`
on first read, cached in `OnceLock`. (Const-bake via build.rs is
possible but adds toolchain coupling — defer unless snapshot stability
demands it.)

### 4.2 Dispatch

In `handler.rs::call_tool`:

```rust
if let Some(decl) = GRAPH_TOOLS.iter().find(|d| d.name == name.as_ref()) {
    return Ok(graph_tools::dispatch(store, &name, arguments).await);
}
```

`graph_tools::dispatch` parses args with `serde_json::from_value`,
invokes the matching `GraphQueries` method, serializes the result.
Errors map to `CallToolResult::error` mirroring `handle_search`.

If `store` is `None`, return a `CapabilityUnavailable`-shaped error
(graph tools fail closed when no store is wired, per invariant 6).

### 4.3 Namespace

Tool names use a `graph.` prefix to avoid collision with the 8 verbs and
future IDL additions. JSON schemas live alongside in
`crates/cairn-mcp/src/graph_tools.rs`.

## 5. Stdio blank-line relay

Today `serve_stdio` calls `rmcp::transport::io::stdio()` directly, which
forwards every byte to the rmcp framer. Some harnesses (Claude Desktop
historically) emit blank lines between JSON-RPC frames; the framer
chokes on them.

Add a small relay shim:

- Spawn a blocking task that reads `tokio::io::stdin()` line-by-line
  (`tokio::io::AsyncBufReadExt::lines`).
- Drop lines where `line.trim().is_empty()`.
- Forward retained lines to a `tokio::io::duplex` writer that the rmcp
  transport consumes.
- Stdout passthrough is identity (rmcp writes well-formed frames).

This is symmetric with Graphify's pattern referenced in the issue.

## 6. Status / capability advertisement

`graph_tools::is_available(store: Option<&dyn MemoryStore>) -> bool`
returns `true` iff a store is wired. `CairnMcpServer::capabilities()`
gets a new field — or, if the contract enum is closed, we stash this in
the existing `extensions` slot. Spec defers wiring into `cairn status`
output to a follow-up if it requires touching the IDL contract; the
manifest snapshot test is the in-PR signal.

## 7. Tests

- **`crates/cairn-store-sqlite/tests/graph_queries.rs`** — fixture: 6
  nodes, 8 edges (one tombstoned, one cross-scope). Assertions:
  - BFS from `A` with `max_hops=2` returns the right depth-stratified
    set; `max_hops=5` capped at 5 by the input parser, not the SQL.
  - Token budget halts emission at the documented byte threshold.
  - `timeline` is `valid_at ASC` and excludes expired by default;
    `include_expired = true` includes them.
  - `get_neighbors` `relation` filter narrows correctly.
  - `surprising_connections` ranks cross-scope edge above same-scope
    edge of equal `confidence_score`.
- **`crates/cairn-mcp/tests/graph_tools_manifest.snap`** — `insta`
  snapshot of merged manifest (names, descriptions, schema JSON) for
  byte-identity across rebuilds (brief §8.0.a).
- **`crates/cairn-mcp/tests/blank_line_relay.rs`** — feed stdin with
  blank lines interleaved between frames; assert rmcp parses cleanly.
- **Unit:** token-budget accumulator helper (table-driven).

All run under `cargo nextest run --workspace --locked --no-fail-fast`.

## 8. Out of scope

- Writes to the graph from MCP (future write-side issue).
- SSE / HTTP transports (P1, separate issue).
- `cairn status` integration if it requires IDL changes (defer; manifest
  snapshot is the in-PR signal).
- Embedding-aware ranking in `surprising_connections` (deterministic
  scoring only for P0).

## 9. Invariants touched

- **§2.4 Seven contracts** — no new contract added (queries live in the
  existing store crate).
- **§2.5 WAL + two-phase apply** — read-only path, untouched.
- **§2.6 Fail closed on capability** — graph tools return error when no
  store is wired.
- **§2.10 Privacy by construction** — graph queries never log
  `entity_nodes.name` or `summary` above `debug`.

## 10. Verification

Standard checklist (CLAUDE.md §8): fmt, clippy, check, nextest, doctests,
core boundary, codegen `--check`, docgen `--check` if any user-facing
surface (it is — manifest snapshot moves), deny/audit/machete.
