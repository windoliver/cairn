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
  `MemoryStore`'s graph methods can return `CapabilityUnavailable`.
  **No new capability fields are added.** The existing
  `MemoryStoreCapabilities::graph_edges: bool` (already public,
  `crates/cairn-core/src/contract/memory_store.rs:48`) is the single
  source of truth: `cairn-store-sqlite` already sets it `true` once
  migrations 0042/0043 are applied; every other adapter leaves it at
  the `Default` value of `false`. Reusing this avoids a public
  contract bump. The MCP handler reads `graph_edges` at `list_tools`
  time and at `call_tool` time (defense in depth — same predicate,
  both sites).

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
allowed set. Edges with `source_record_id IS NULL` (orphan
provenance) are **excluded by default** — they can be opted into via
an explicit `include_unattributed: bool` argument that the caller
must set deliberately.

#### Scope resolution on the MCP path (prerequisite)

Today the MCP `search` handler hard-codes `visibility_allowlist =
vec![]` (see `crates/cairn-mcp/src/handler.rs::handle_search`),
which the search verb interprets as "no narrowing." That is the
existing fail-open we are explicitly *not* inheriting here. Graph
tools are stricter: an empty resolved scope set means **deny all**,
not "allow all."

This work depends on a concrete scope-resolution API on the MCP
path. The contract:

```rust
// In cairn-core or cairn-mcp — exact location to be decided in the
// implementation plan.
pub trait McpSessionScope {
    /// Resolve the active session's allowed record scope ids.
    /// An empty Vec means "no scopes authorized" → tools fail closed.
    /// `None` means scope resolution is unconfigured for this
    /// deployment → tools advertise as unavailable.
    fn allowed_scopes(&self) -> Option<Vec<ScopeId>>;
}
```

The MCP handler accepts an `Arc<dyn McpSessionScope>` at
construction time. `serve_stdio_with_store` takes one explicitly;
deployments without a scope source must supply a deny-all impl
(or accept that graph tools advertise `unavailable`).

**This trait does not exist yet in the repo.** Landing it is
in-scope for this issue and gates the rest of the work. Two
acceptable shapes:

1. **In-scope, this PR:** add `McpSessionScope` and a default impl
   that reads scope ids from a config field (e.g.
   `cairn.toml::[mcp.scope] = ["..."]`) — explicit, file-based,
   matches Cairn's "no hidden global state" invariant.
2. **Out of scope, blocked-on prerequisite:** if (1) is too large
   for one PR, this issue blocks on a separate prerequisite issue
   that lands `McpSessionScope`. Until that lands, graph tools must
   stay disabled (capability bit returns `false` even on
   graph-capable stores).

The implementation plan picks one of these paths explicitly.
Shipping the graph tools without a scope source is **not**
permitted by this spec.

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

### 3.0 Single shared `visible_edges` primitive

Every graph read in this spec must derive from one shared
**`visible_edges`** primitive that bakes in *both* the active-at-now
temporal filter *and* the per-session scope filter from §2.1.1.
There is no public method that reads `entity_edges` without going
through this primitive — defense in depth against any future SQL
sketch forgetting one of the two clauses.

In SQL the primitive is a session-local view installed at connection
open (or, equivalently, a CTE prefix injected by `GraphQueries`):

```sql
-- visible_edges — the only edge source ANY graph tool reads from.
-- (a) temporal: only edges currently active
-- (b) provenance: only edges whose source record is in the
--     caller's allowed scope set; orphan edges (NULL
--     source_record_id) are excluded unless the tool sets
--     :include_unattributed = 1.
WITH visible_edges AS (
  SELECT e.*
  FROM entity_edges e
  WHERE e.expired_at IS NULL
    AND e.valid_at  <= :now
    AND (e.invalid_at IS NULL OR e.invalid_at > :now)
    AND (
      EXISTS (
        SELECT 1 FROM records r
        WHERE r.record_id = e.source_record_id
          AND r.scope IN rarray(:allowed_scopes)
      )
      OR (:include_unattributed = 1
          AND e.source_record_id IS NULL)
    )
)
```

Likewise a derived **`visible_nodes`** primitive — a node is
"visible" iff its provenance can be established through any
in-scope record, **not just through a currently-active edge**. A
zero-edge or all-edges-aged-out entity that is otherwise
authorized must remain readable; collapsing those to `NotFound`
would be a correctness regression, not a security feature.

Node provenance comes from three independent sources, any of which
suffices for visibility (the OR is required to handle isolated and
historical entities):

1. **Active edges** — at least one `visible_edges` row touches
   the node. Covers the common case.
2. **Episodic provenance** — at least one row in
   `entity_episodes` (migration 0044) for this entity points at a
   record in `allowed_scopes`. Covers entities created or
   referenced via ingest sources whose edges have all been
   tombstoned or expired-out.
3. **Historical edges** — at least one *non-active*
   `entity_edges` row (expired or out-of-window) touches the node
   *and* its `source_record_id` is in `allowed_scopes`. Covers
   the "all my edges aged out but I still exist" case.

Direct reads of `entity_nodes` without composing one of the three
provenance branches are forbidden in tool SQL.

```sql
visible_nodes AS (
  SELECT n.*
  FROM entity_nodes n
  WHERE n.expired_at IS NULL
    AND (
      EXISTS (
        SELECT 1 FROM visible_edges e
        WHERE e.source_id = n.id OR e.target_id = n.id
      )
      OR EXISTS (
        SELECT 1 FROM entity_episodes ep
        JOIN records r ON r.record_id = ep.source_record_id
        WHERE ep.entity_id = n.id
          AND r.scope IN rarray(:allowed_scopes)
      )
      OR EXISTS (
        SELECT 1 FROM entity_edges he
        JOIN records r ON r.record_id = he.source_record_id
        WHERE (he.source_id = n.id OR he.target_id = n.id)
          AND r.scope IN rarray(:allowed_scopes)
      )
    )
)
```

This closes node-probe leaks (out-of-scope ids/names still return
`NotFound`) **and** preserves authorized lookup of isolated or
historical entities. A test must cover all three provenance
branches: zero-edge entity with an in-scope episode, entity whose
only edges are expired but in-scope, and the standard active-edge
case.

The implementation enforces this by making `GraphQueries` only
expose methods that internally compose these CTEs into every query;
there is no `read_node_by_id_unscoped` escape hatch. A test must
attempt to probe an out-of-scope id by every tool surface and
confirm all five return `NotFound` — never the row payload.

`graph.timeline` does not exempt itself from the scope filter —
even the audit view shows only edges the caller is authorized to
see. It does relax temporal slicing (returns expired edges when
`include_expired = true`), but the scope predicate is unconditional.

Future-dated edges (`valid_at > now`) and out-of-scope edges are
therefore invisible to all five tools through every code path.

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
`OR`-predicate. Each query reads from `visible_nodes` (the §3.0
primitive), never from `entity_nodes` directly, so an out-of-scope
id or name returns `NotFound` instead of leaking the payload:

```sql
-- ById  (allowed_scopes, include_unattributed bound by caller)
WITH visible_edges AS (...), visible_nodes AS (...)   -- §3.0
SELECT id, name, summary, created_at FROM visible_nodes
WHERE id = ?1;

-- ByName
WITH visible_edges AS (...), visible_nodes AS (...)
SELECT id, name, summary, created_at FROM visible_nodes
WHERE name_norm = ?1;   -- ?1 = normalize_entity_name(input)
```

Tests:

1. Round-trip: an entity inserted with a name like `"Auth Service
   (v2)"` is found by `ByName { name: "Auth Service (v2)" }`; a
   naive `lower()` lookup would not.
2. Id-vs-name collision: row A with `id = "X"`, row B with `name =
   "X"`. `ById { id: "X" }` returns A; `ByName { name: "X" }`
   returns B; never the wrong one.
3. **Scope leak probe (negative test):** an entity whose only
   provenance is a record outside `allowed_scopes` is *not*
   returned by either arm — the response is `NotFound`. Even a
   caller who knows the exact id sees nothing.

Live edge count reads from the §3.0 `visible_edges` primitive (which
already bakes in temporal AND scope filtering — never from
`entity_edges` directly):

```sql
WITH visible_edges AS (...)   -- §3.0
SELECT COUNT(*) FROM visible_edges e
WHERE e.source_id = ?1 OR e.target_id = ?1;
```

### 3.2 `graph.get_neighbors`

```sql
WITH visible_edges AS (...), visible_nodes AS (...)   -- §3.0
SELECT e.id, e.source_id, e.target_id, e.relation,
       e.confidence_score, e.valid_at
FROM visible_edges e
WHERE (e.source_id = ?1 OR e.target_id = ?1)
  AND (?2 IS NULL OR e.relation = ?2)
  AND (?3 IS NULL OR e.confidence_score >= ?3);
```

Returned edges include the *other* node's id + name, joined against
`visible_nodes` (so the joined node also passes the scope check —
no name leak through the neighbor field).

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

SQLite does not expose SELECT-list aliases to the same SELECT's
`WHERE` clause, so the alias `other_id` is computed in an inner
CTE and the `visited` filter + `ROW_NUMBER` partitioning happen one
level out. The query is written so it is executable verbatim — a
test runs the exact text against an in-memory SQLite to guarantee
the spec stays in sync with the implementation.

```sql
WITH visible_edges AS (...),     -- §3.0 — temporal + scope baked in
edge_with_other AS (
  -- Compute other_id once, in an inner CTE, so the outer SELECT
  -- can name it in WHERE / PARTITION BY without alias-scoping
  -- issues.
  SELECT
    e.id, e.source_id, e.target_id, e.relation, e.confidence_score,
    CASE WHEN e.source_id IN rarray(:frontier)
         THEN e.target_id ELSE e.source_id END AS other_id
  FROM visible_edges e
  WHERE e.source_id IN rarray(:frontier)
     OR e.target_id IN rarray(:frontier)
),
candidate AS (
  SELECT
    id, source_id, target_id, relation, confidence_score, other_id,
    ROW_NUMBER() OVER (
      PARTITION BY other_id
      ORDER BY confidence_score DESC, id ASC      -- deterministic
    ) AS rn
  FROM edge_with_other
  WHERE other_id NOT IN rarray(:visited)
)
SELECT id, source_id, target_id, relation, confidence_score, other_id
FROM candidate
WHERE rn = 1
ORDER BY confidence_score DESC, id ASC
LIMIT :wave_cap;
```

Seed loading also goes through the §3.0 primitives: BFS calls
`get_entity` (which reads from `visible_nodes`) for the seed, so a
caller who supplies an out-of-scope seed id receives `NotFound`
before any traversal begins. There is no path that returns the seed
node payload before scope is checked.

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

Timeline is the audit view, so it relaxes the temporal filter (it
intentionally returns expired and outside-of-now-window edges when
the caller asks). The **scope filter is unconditional** — even the
audit view shows only edges the caller is authorized to see.
Implemented as a sibling of `visible_edges` that applies *only* the
scope predicate (not the temporal one):

```sql
WITH scope_visible_edges AS (
  SELECT e.* FROM entity_edges e
  WHERE (
    EXISTS (
      SELECT 1 FROM records r
      WHERE r.record_id = e.source_record_id
        AND r.scope IN rarray(:allowed_scopes)
    )
    OR (:include_unattributed = 1 AND e.source_record_id IS NULL)
  )
)
SELECT id, source_id, target_id, relation, confidence_score,
       valid_at, invalid_at, created_at, expired_at,
       tombstone_reason, source_record_id
FROM scope_visible_edges
WHERE (source_id = ?1 OR target_id = ?1)
  AND (?2 = 1 OR expired_at IS NULL)
ORDER BY valid_at ASC, created_at ASC;
```

`?2 = include_expired` (bool → 0/1). Stable secondary sort by
`created_at` for deterministic output when multiple edges share
`valid_at`. Negative test: an edge with `source_record_id` outside
`allowed_scopes` is absent from the timeline regardless of
`include_expired`.

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
WITH visible_edges AS (...),         -- §3.0: temporal + scope
     input(id) AS (VALUES (?1), (?2) /* … */),
     modal_record AS (
       SELECT i.id AS entity_id,
              e.source_record_id AS rec,
              COUNT(*) AS n
       FROM input i
       JOIN visible_edges e
         ON (e.source_id = i.id OR e.target_id = i.id)
       GROUP BY i.id, e.source_record_id
     ),
     scored AS (
       SELECT e.*,
              e.confidence_score *
              (1.0 + CASE WHEN /* modal mismatch */ THEN 1.0 ELSE 0.0 END)
                AS score
       FROM visible_edges e
       WHERE e.source_id IN input
         AND e.target_id IN input
     )
SELECT * FROM scored ORDER BY score DESC, id ASC LIMIT ?n;
```

`visible_edges` is the single source of truth for "real, in-scope"
edges — modal computation, scoring, and the final projection all
read from it, so future-dated, future-invalidated, *and*
out-of-scope edges cannot enter ranking through any path.

Final SQL refines the modal join — sketch above; the implementation
may use `Vec<RecordId>` accumulation in Rust if the SQL gets unwieldy,
but the temporal predicate is non-negotiable.

Adversarial test cases (mandatory):

- An edge with `valid_at = now + 1h` MUST be excluded from the result
  set and from the modal-record computation.
- An active edge with `invalid_at = now + 1h` MUST be **included**
  (currently real, future-invalidated) — this is the case a naive
  `invalid_at IS NULL` filter incorrectly drops.
- An active, in-window edge whose `source_record_id` is outside
  `allowed_scopes` MUST be excluded from both the modal-record
  computation and the final projection — out-of-scope edges cannot
  surface as a "surprise" hit through any path.

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

Capability discovery is **the same predicate everywhere** — there is
no separate "is the store wired" path. The single source of truth is
the `StoreCapabilities { graph_edges: bool }` bit introduced in
§2.1: `cairn-store-sqlite` sets it `true` once migrations
0042/0043/0044 are applied; every other `MemoryStore` impl (and the
default trait method) returns `false`.

```rust
// One predicate, used at both list_tools and call_tool sites.
fn graph_tools_available(store: Option<&dyn MemoryStore>) -> bool {
    store
        .map(|s| s.capabilities().graph_edges)
        .unwrap_or(false)
}
```

`CairnMcpServer::capabilities()` gains a derived `graph_edges`
flag mirrored from the wired store; if the contract enum is closed
we surface it via the existing `extensions` slot. Wiring this into
`cairn status` output is deferred only if it requires an IDL bump —
the in-PR signal is the manifest snapshot test, which must cover
**three** states:

1. No store wired → no `graph.*` tools advertised.
2. Store wired but `graph_edges = false` (e.g. an in-memory
   stub) → no `graph.*` tools advertised.
3. Store wired with `graph_edges = true` → all five `graph.*`
   tools advertised, byte-identical across rebuilds.

Negative test: in case (2), invoking `graph.get_entity` returns
`CapabilityUnavailable` rather than executing — defense in depth at
the dispatch site mirrors the discovery gate.

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
