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
  concat `TOOLS` + `GRAPH_TOOLS` **only when the wired store advertises
  graph capability** — `store.is_some()` is not sufficient because
  `MemoryStore`'s graph methods are allowed to return
  `CapabilityUnavailable` by default. Add a `MemoryStore::capabilities()
  -> StoreCapabilities` accessor (or extend the existing capability
  set) with a `graph_traversal: bool` bit, set `true` only by
  `cairn-store-sqlite` once migrations 0042/0043 are applied. The MCP
  handler reads that bit at `list_tools` time and at `call_tool` time
  (defense in depth — same predicate, both sites).

  This gates discovery on the *actual* graph capability, not on
  whether a store handle is present. Clients that cache `tools/list`
  responses never see a graph tool they cannot invoke. Manifest
  snapshot tests cover both states (graph-capable and graph-incapable)
  so the gating is byte-verifiable.
- **`crates/cairn-cli/src/...mcp dispatch`** (edit). Pass the open store
  into `CairnMcpServer` so graph tools have a backend. Today
  `serve_stdio` always builds an unwired handler; introduce
  `serve_stdio_with_store(store, config)` and call it from the CLI.

Why A (chosen over alternatives B/C from brainstorm): graph queries are
SQLite-shaped (recursive CTEs), have no business logic to share across
surfaces, and are MCP-only per issue scope. Adding an 8th contract is
brief-level work and overkill for read-only queries.

### 2.1.1 Authorization & scope (CRITICAL)

The graph tables (`entity_nodes`, `entity_edges`) carry no
tenant/workspace/session columns of their own. Record-level reads
elsewhere in Cairn enforce visibility by having the caller pass a
`visibility_allowlist` of scope ids; the graph schema cannot do the
same directly. **Skipping this control would let any caller who knows
one entity id traverse edges sourced from any record in the database,
including other tenants/workspaces sharing the same vault file.** The
issue's "without going through the 8-verb layer" framing is about
*shape* (graph queries vs. record retrieval), not about *bypassing
authorization*.

The spec therefore mandates **scope-by-provenance**: every graph
read derives its allowed scope set from the active session, then
filters edges by joining `entity_edges.source_record_id` →
`records.scope` and keeping only edges whose source record is in the
allowed set. A node is reachable iff at least one in-scope edge
touches it. Edges with `source_record_id IS NULL` (orphan provenance)
are **excluded by default** — they can be opted into via an explicit
`include_unattributed: bool` argument that the caller must set
deliberately.

The shared active-at-now predicate composes with this scope filter:

```sql
EXISTS (
  SELECT 1 FROM records r
  WHERE r.record_id = e.source_record_id
    AND r.scope IN rarray(:allowed_scopes)
)
AND e.expired_at IS NULL
AND e.valid_at  <= :now
AND (e.invalid_at IS NULL OR e.invalid_at > :now)
```

This is the **only** safe form of any graph read. `GraphQueries`
takes the resolved `allowed_scopes` slice as a constructor argument
and bakes it into every query — there is no public method that omits
the scope predicate. The MCP dispatch layer fills `allowed_scopes`
from the active session's scope resolution (same source the search
verb uses) before invoking `GraphQueries`.

A test must assert: an edge whose `source_record_id` belongs to a
record outside `allowed_scopes` is invisible to all five tools (no
neighbor, no traversal step, no timeline entry, no surprise hit).

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

All queries default to a single shared **active-at-now** predicate
applied uniformly:

```sql
e.expired_at IS NULL
  AND e.valid_at  <= :now
  AND (e.invalid_at IS NULL OR e.invalid_at > :now)
```

Defined once as `fn active_edge_predicate(alias: &str) -> &str` (or a
SQL view `entity_edges_active`) and reused by `get_entity` edge count,
`get_neighbors`, `graph.query` traversal, and `surprising_connections`.
The only query that intentionally bypasses temporal slicing is
`graph.timeline`, which is the audit view by definition.

Future-dated edges (`valid_at > now`) are therefore invisible to all
present-time reads — closing the leak that an earlier draft of this
spec allowed.

### 3.1 `graph.get_entity`

**Disambiguated input.** The MCP tool exposes two mutually exclusive
arguments: `id: Option<String>` and `name: Option<String>`. Exactly
one must be set; the schemars schema enforces this with an `oneOf`
constraint. The earlier `id_or_name` overload is dropped — passing a
single string into a single column is correct only when ids and
names cannot collide, and we cannot guarantee that for arbitrary
content.

```rust
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(deny_unknown_fields)]
pub enum GetEntityArgs {
    ById  { id: String },
    ByName { name: String },
}
```

**Normalization.** `name_norm` is the dedup key chosen at upsert
time (see `cairn-store-sqlite::entity_graph::node::upsert_entity`),
and the graph's normalization is **not** plain `lower()` — it strips
punctuation, normalizes whitespace, and may apply Unicode folding.
A naive `lower(?1)` lookup would silently miss any entity whose
canonical form requires more than ASCII case folding.

The lookup must therefore call the **same** normalization function
that populated the row. Land a shared helper before the queries crate
uses it:

```rust
// crates/cairn-core/src/domain/graph/normalize.rs (new)
/// Canonical form used as the `name_norm` dedup key for entity nodes.
/// Single source of truth — every insertion site and every read-side
/// lookup MUST go through this function.
pub fn normalize_entity_name(input: &str) -> String { /* … */ }
```

Existing upsert sites that build `name_norm` inline are migrated to
this helper as part of the same PR (small change, currently scattered
across resolver and ingest paths).

**Two SQL queries** — one per arm — instead of one overloaded
`OR`-predicate. This gives unambiguous semantics and lets each form
use its own index path:

```sql
-- ById
SELECT id, name, summary, created_at FROM entity_nodes
WHERE id = ?1 AND expired_at IS NULL;

-- ByName
SELECT id, name, summary, created_at FROM entity_nodes
WHERE name_norm = ?1   -- ?1 = normalize_entity_name(input)
  AND expired_at IS NULL;
```

A regression test asserts that an entity inserted with a name like
`"Auth Service (v2)"` is round-trippable: `ByName { name: "Auth
Service (v2)" }` returns the row, while a naive `lower()` lookup
would not. A second test populates one row with `id = "X"` and a
*different* row with `name = "X"` and confirms `ById { id: "X" }`
returns the first while `ByName { name: "X" }` returns the second
— never the wrong one.

Live edge count via secondary query (uses the shared active-at-now
predicate from §3):

```sql
SELECT COUNT(*) FROM entity_edges e
WHERE (e.source_id = ?1 OR e.target_id = ?1)
  AND e.expired_at IS NULL
  AND e.valid_at <= :now
  AND (e.invalid_at IS NULL OR e.invalid_at > :now);
```

### 3.2 `graph.get_neighbors`

```sql
SELECT e.id, e.source_id, e.target_id, e.relation,
       e.confidence_score, e.valid_at
FROM entity_edges e
WHERE (e.source_id = ?1 OR e.target_id = ?1)
  AND e.expired_at IS NULL
  AND e.valid_at <= :now
  AND (e.invalid_at IS NULL OR e.invalid_at > :now)
  AND (?2 IS NULL OR e.relation = ?2)
  AND (?3 IS NULL OR e.confidence_score >= ?3);
```

Returned edges include the *other* node's id + name (joined).

### 3.3 `graph.query` (BFS/DFS)

**Traversal is driven from Rust, not by a single recursive CTE.** A
recursive `WITH RECURSIVE` cannot enforce a global node/edge budget
(per-path counters do not bound the total materialized set in a
fan-out graph) and cannot deterministically pick a parent edge per
node when multiple paths reach it. We therefore expand level-by-level
in Rust, with one SQL query per frontier wave, and decide budgets,
cycle exclusion, and parent edges in the same place.

#### Algorithm

```text
fn bfs(seed, max_hops, node_budget, token_budget):
    visited:   IndexMap<NodeId, NodeRecord>      // insertion-ordered
    parent_of: HashMap<NodeId, EdgeId>           // first-discoverer wins
    depth_of:  HashMap<NodeId, u32>
    frontier:  Vec<NodeId> = [seed_id]
    visited.insert(seed_id, seed_record)
    depth_of[seed_id] = 0

    for depth in 1..=max_hops:
        if frontier.is_empty(): break
        next_edges = SELECT_NEIGHBOR_EDGES(frontier_ids)   // see SQL below
        next_frontier = []
        for edge in next_edges (deterministic order):
            other_id = edge.other_endpoint
            if other_id in visited: continue              // cycle / re-discovery
            if visited.len() >= node_budget: break_outer  // hard global cap
            visited.insert(other_id, fetch_node(other_id))
            parent_of[other_id] = edge.id
            depth_of[other_id]  = depth
            next_frontier.push(other_id)
        frontier = next_frontier

    return visited (BFS order = insertion order)
           with parent_of[] for DFS reconstruction
           truncated by token_budget during serialization
```

**Per-wave neighbor SQL.** A naive `LIMIT :wave_cap` on the raw edge
result is wrong: duplicate edges, multi-edges to the same neighbor,
and edges back to already-visited nodes all consume the budget
before the Rust dedup runs, which can drop reachable nodes that
appear later in the ordered set. The query must instead bound by
**distinct unseen neighbor**, picking exactly one canonical edge
per neighbor up front via a window function.

```sql
WITH candidate AS (
  SELECT
    CASE WHEN e.source_id IN rarray(:frontier)
         THEN e.target_id ELSE e.source_id END  AS other_id,
    e.id, e.source_id, e.target_id, e.relation, e.confidence_score,
    ROW_NUMBER() OVER (
      PARTITION BY
        CASE WHEN e.source_id IN rarray(:frontier)
             THEN e.target_id ELSE e.source_id END
      ORDER BY e.confidence_score DESC, e.id ASC   -- deterministic
    ) AS rn
  FROM entity_edges e
  WHERE (e.source_id IN rarray(:frontier)
      OR e.target_id IN rarray(:frontier))
    AND e.expired_at IS NULL
    AND e.valid_at <= :now
    AND (e.invalid_at IS NULL OR e.invalid_at > :now)
    AND EXISTS (
      SELECT 1 FROM records r
      WHERE r.record_id = e.source_record_id
        AND r.scope IN rarray(:allowed_scopes)
    )
    AND other_id NOT IN rarray(:visited)
)
SELECT id, source_id, target_id, relation, confidence_score, other_id
FROM candidate
WHERE rn = 1
ORDER BY confidence_score DESC, id ASC
LIMIT :wave_cap;
```

`wave_cap = node_budget - visited.len()` now bounds *distinct unseen
neighbors* directly, because the inner CTE collapses each candidate
to a single canonical edge before `LIMIT` applies. Reachable nodes
later in the ordered set can no longer be displaced by duplicate
edges to earlier-seen ones.

The implementation must additionally **stream** rows
(`Rows::next()` loop, not `query_map().collect()`) and break out of
the read loop the moment `visited.len() >= node_budget`, so an
adversarial graph that just-fits the SQL `LIMIT` cannot still cause
the Rust side to allocate the full wave before truncating.

#### Properties this gives us

1. **Hard global node bound.** `visited.len() >= node_budget` halts
   the loop in Rust regardless of fan-out — no intermediate set can
   exceed `node_budget` rows.
2. **Hard depth bound.** Outer `for` loop is bounded by `max_hops`
   (≤5 from the schema), so worst-case wave count is deterministic.
3. **Cycle exclusion.** `if other_id in visited: continue` catches
   every cycle and every re-discovery edge, with no path-string
   tricks.
4. **Deterministic parent edge.** The `ORDER BY confidence_score DESC,
   id ASC` clause + first-writer-wins on `parent_of` makes parent
   assignment a pure function of the edge set, so DFS reconstruction
   is reproducible run-to-run.
5. **DFS** is a post-order walk over `parent_of` starting from the
   seed; same edge set, just visited in stack order.
6. **Token budget (secondary)** still applies during serialization:
   accumulate `serde_json::to_string(&row).len()` and stop emitting
   when over budget. Layered defense, not primary.

#### Trade-offs we accept

- One SQL round-trip per BFS depth instead of a single recursive CTE.
  At `max_hops=5` and `node_budget=1024` this is ≤5 queries, each
  parameter-bound on the previous wave's ids. SQLite handles this in
  microseconds; the simplicity and budget correctness are worth it.
- `rarray` (carray module) needs to be enabled in the SQLite
  connection. If unavailable, fall back to `IN (?,?,?,...)` with a
  re-prepared statement per wave — same semantics, slightly more
  prepare overhead.

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

Every CTE that reads `entity_edges` reuses the **shared active-at-now
predicate** from §3 — same `expired_at IS NULL AND valid_at <= :now AND
(invalid_at IS NULL OR invalid_at > :now)` clause that `get_neighbors`,
`get_entity` edge count, and `graph.query` apply. This is required:
without it, `surprising_connections` would re-leak future-dated edges
and silently drop currently-active edges that carry a future
`invalid_at`, which the rest of the API does not.

```sql
WITH input(id) AS (VALUES (?1), (?2) /* … */),
     active_edges AS (
       SELECT e.*
       FROM entity_edges e
       WHERE e.expired_at IS NULL
         AND e.valid_at <= :now
         AND (e.invalid_at IS NULL OR e.invalid_at > :now)
     ),
     modal_record AS (
       SELECT i.id AS entity_id,
              e.source_record_id AS rec,
              COUNT(*) AS n
       FROM input i
       JOIN active_edges e
         ON (e.source_id = i.id OR e.target_id = i.id)
       GROUP BY i.id, e.source_record_id
     ),
     scored AS (
       SELECT e.*,
              e.confidence_score *
              (1.0 + CASE WHEN /* modal mismatch */ THEN 1.0 ELSE 0.0 END)
                AS score
       FROM active_edges e
       WHERE e.source_id IN input
         AND e.target_id IN input
     )
SELECT * FROM scored ORDER BY score DESC, id ASC LIMIT ?n;
```

The `active_edges` CTE is the single source of truth for "currently
real" edges in this query — modal computation, scoring, and the final
projection all read from it, so future-dated and future-invalidated
edges cannot enter ranking through any path.

Final SQL refines the modal join — sketch above; the implementation
may use `Vec<RecordId>` accumulation in Rust if the SQL gets unwieldy,
but the temporal predicate is non-negotiable.

Adversarial test cases (mandatory):

- An edge with `valid_at = now + 1h` MUST be excluded from the result
  set and from the modal-record computation.
- An active edge with `invalid_at = now + 1h` MUST be **included**
  (currently real, future-invalidated) — this is the case a naive
  `invalid_at IS NULL` filter incorrectly drops.

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

### 5.1 Framing — MCP stdio is NOT LSP

**Pin this up front to avoid confusion with LSP-style transports:**
MCP over stdio is **newline-delimited JSON-RPC** — one complete JSON
object per line, terminated by `\n`. It does **not** use LSP's
`Content-Length: N\r\n\r\n<body>` framing. There is no header block,
no required blank-line delimiter inside a frame, and no byte-counted
body. See the MCP spec §"stdio transport" and the rmcp
`transport::io::stdio()` reader, which is line-based.

Because frames are exactly one line each, any blank line on stdin is
**inter-frame whitespace by definition** — never part of a body.
Filtering blank lines before the rmcp framer therefore cannot corrupt
a valid frame.

### 5.2 Why a relay is needed

Some harnesses (Claude Desktop and a few wrapper shells) interleave
blank lines or stray `\r` between JSON-RPC frames during startup or
when the host writes log noise to the same pipe. The rmcp framer is
strict and will surface a parse error on the empty token rather than
skip it, breaking the session.

### 5.3 Shim

- Spawn one task reading `tokio::io::stdin()` line-by-line via
  `AsyncBufReadExt::lines()`.
- For each line: skip if `line.trim().is_empty()`. Otherwise write
  `line` + `'\n'` into the write half of a `tokio::io::duplex(64 *
  1024)` channel.
- Hand the read half of that duplex to rmcp as its stdin. Stdout is
  passed through directly (rmcp's outgoing frames are already
  well-formed single-line JSON).
- Cancellation: when stdin reaches EOF the relay drops the duplex
  writer, which the rmcp side observes as EOF and exits cleanly.

**Invariants the shim preserves:**

1. Every non-empty input line is forwarded byte-for-byte (no trim, no
   re-encoding) — rmcp sees the exact JSON the harness sent.
2. The relay never holds more than one line at a time; no buffering
   semantics that could reorder concurrent writes (stdin has only one
   writer anyway).
3. Stdout is untouched — server-to-host frames are not normalized.

This is the same pattern Graphify ships and the one called out in the
issue.

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
