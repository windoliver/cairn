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
| `graph.timeline` | All currently-visible edges for an entity ordered by `valid_at`. Two explicit flags (`include_history`, `include_expired`) opt into broader temporal windows; defaults match the active-at-now predicate. |
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
  concat `TOOLS` + `GRAPH_TOOLS` **only when the conjunction of two
  conditions holds**:

  1. The wired store advertises graph capability via the existing
     `MemoryStoreCapabilities::graph_edges: bool` field
     (`crates/cairn-core/src/contract/memory_store.rs:48`). No new
     capability struct fields are added.

     **`CairnConfig::capabilities().graph_edges` does NOT
     change.** That flag promises end-to-end runtime support
     surfaced via `cairn status`, but graph tools depend on
     per-transport (`single_tenant`) and per-request (resolver
     outcome) conditions a global capability set cannot
     represent. Flipping it would over-advertise to deployments
     where `tools/list` must hide every `graph.*` tool.

     **A new `mcp_graph_tools` capability surface lands in this
     PR** so `status` and `tools/list` both read from a single
     authoritative predicate — no split-brain. Add a method
     `CairnConfig::mcp_graph_tools_available(scope, transport)
     -> McpGraphAvailability` that returns one of
     `Available { tool_count: 5 }`,
     `UnavailableSingleTenantOff`,
     `UnavailableNoStoreCapability`, or
     `UnavailableNoScopeResolver`. `cairn status` reports the
     enum's discriminant in its capability section; the MCP
     handler reuses the same function (composed with
     per-request scope resolution) to gate `tools/list` and
     `tools/call`. The two surfaces cannot drift because they
     share the same code path.

     **Rollout prerequisites (in-scope for this PR, must land
     atomically):**

     1. **MCP config schema.** Add an `[mcp.stdio]` section to
        `cairn.toml`:
        ```toml
        [mcp.stdio]
        single_tenant = false               # default: deny
        principal     = "<scope-tuple ref>" # required when
                                            # single_tenant = true
        ```
        Defaults are fail-closed: `single_tenant = false` means
        graph tools are unavailable, no principal needed; flipping
        to `true` requires a `principal` value or config
        validation rejects the file. Schema lands in
        `crates/cairn-core/src/config/mod.rs` next to the existing
        config sections, with a serde-derived loader and a
        validation error for the missing-principal case.
     2. **CLI wiring.** `crates/cairn-cli/src/mcp.rs:22` and
        `crates/cairn-mcp/src/lib.rs::serve_stdio` currently
        launch an unwired handler. Replace `serve_stdio()` with
        `serve_stdio_with_store(store, scope, config,
        principal)` that the CLI calls after opening the SQLite
        store. The unwired entry point either disappears or is
        gated to returning the 8-verb manifest only. The CLI
        derives `principal` from `config.mcp.stdio.principal`;
        if absent (because `single_tenant = false`), it passes
        `None` and graph tools stay disabled — no implicit
        process-global default. A CLI integration test asserts
        that `cairn mcp` started against a config without
        `single_tenant = true` returns the 8-verb manifest only.
     3. **Status integration.** `cairn status` consumes
        `mcp_graph_tools_available` and prints one of the four
        states verbatim. A snapshot test covers each.

     The `MemoryStore::capabilities().graph_edges` flag from
     `cairn-store-sqlite` is already correct and stays
     unchanged. The CLI wiring, config schema, status
     integration, and MCP handler gating logic all move
     together in this PR; partial rollout is the failure mode
     called out earlier.
  2. A non-deny-all `McpSessionScope` resolver is wired into the
     handler (see §2.1.1). A deployment that has graph storage but
     no scope resolver does **not** advertise the tools.

  Both conditions are evaluated at `list_tools` time *and* at
  `call_tool` time (defense in depth — same predicate, both sites).
  Crucially, the second condition is **not** "a resolver is
  configured" but "the resolver returns a usable scope set for
  *this* request." `list_tools` invokes
  `scope.allowed_scopes(&ctx)` against the current
  `McpAuthContext`; the tools are advertised only if the call
  succeeds and returns a non-empty `Vec<ScopeTuple>`. A request with
  no bound caller, an `Err` resolution, or an empty scope set sees
  *no* `graph.*` tools — capability discovery is per-request, not
  a server-global property.

  Concretely, the helper is the **same** function `call_tool`
  uses (§4.2) — there is no second predicate that could drift:

  ```rust
  fn graph_tools_listed_for(
      &self,
      ctx: &McpAuthContext<'_>,
  ) -> bool {
      // Single shared predicate — see §6 graph_tools_available.
      // Covers transport+single_tenant precondition, store
      // capability, scope resolver presence, AND a successful
      // non-empty resolution.
      graph_tools_available(
          self.store.as_deref(),
          self.scope.as_deref(),
          &self.config,
          self.transport,
          ctx,
      )
  }
  ```

  Cached `tools/list` manifests therefore cannot expose graph
  tools to a caller who could not invoke them — each `tools/list`
  response is bound to a specific `McpAuthContext` and to the
  process's transport + `single_tenant` configuration.

  The manifest snapshot test covers the **transport × store ×
  resolver** matrix:

  | transport | single_tenant | graph_edges | resolver outcome | listed? |
  |---|---|---|---|---|
  | stdio   | false | (any)  | (any)            | no  |
  | stdio   | true  | false  | (any)            | no  |
  | stdio   | true  | true   | None             | no  |
  | stdio   | true  | true   | Err(_)           | no  |
  | stdio   | true  | true   | Ok(vec![])       | no  |
  | stdio   | true  | true   | Ok(non-empty)    | yes |

  Only the bottom row lists graph tools. Future network
  transports add their own rows; the same predicate evaluates
  them.

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

### 2.1.0 Node-payload exposure boundary (CRITICAL)

`entity_nodes.name_norm` is **globally UNIQUE** across the whole
vault — `upsert_entity` deduplicates by it (see
`crates/cairn-store-sqlite/src/entity_graph/node.rs::upsert_entity`).
That makes node *rows* shared infrastructure across every scope
that ever encountered the same canonical name. The `name` and
`summary` columns therefore have no per-scope provenance: the
first writer wins, and a later in-scope edge that makes the node
visible cannot tell whose summary it is reading.

Returning `summary` (or any node-level free-text payload) from an
MCP tool would be a cross-scope metadata leak — the same
mechanism the edge-level scope filter is designed to prevent
would be defeated at the node level.

`name` carries a softer but real leak risk too: a sensitive
canonical name written by one tenant becomes the dedup key for
the same name extracted (or fuzzy-matched into) any other
tenant's data. We therefore distinguish two cases:

1. **Caller-known names round-trip safely.** When a caller
   invokes `graph.get_entity { ByName: "AuthService" }`, the
   caller already knows the name they passed in. Echoing it
   back in the response is not a disclosure. In addition, the
   caller's own `allowed_scopes` ingestion may have produced
   the entity independently — in that case the caller already
   has the name on their side of the trust boundary.

2. **Names of *discovered* nodes (BFS hops, neighbor edges,
   timeline endpoints) are NOT echoed.** Those are nodes the
   caller did not ask for by name; returning the canonical
   name there is the leak path Codex flagged. Until per-scope
   node provenance lands, traversal output stays id-only on
   the discovered-node axis.

Concrete contracts:

- **`graph.get_entity { ById: id }`** returns `{ id, edge_count }` —
  id-only because the caller-asked-by-id case has no name to
  echo back safely.
- **`graph.get_entity { ByName: name }`** returns `{ id,
  echoed_name, edge_count }` where `echoed_name` is the input
  string verbatim (NOT a read of `entity_nodes.name`). Lets the
  caller correlate the id with the name they searched. Never
  surfaces a *different* canonical name.
- **`graph.get_neighbors`** returns edges with `(source_id,
  target_id, relation, confidence_score, valid_at)` — id-only;
  no joined node payload.
- **`graph.query`** returns the BFS subgraph as ids and edges
  only.
- **`graph.timeline`** returns edge fields exactly as defined in
  §3.4; no node-level columns.
- **`graph.surprising_connections`** returns edges plus `score`
  and the modal-vs-non-modal `scope_id`; no node payload.

`summary` is **never** returned by any tool. Restoring full
`name` / `summary` exposure for discovered nodes is a follow-up
issue, gated on per-scope node provenance.

This is a meaningful narrowing of the original issue's
`graph.get_entity` contract. The implementation plan documents
this in the user-facing tool descriptions, including the
rationale for echoed-name vs discovered-name asymmetry, so
callers understand the limitation without having to read the
spec.

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
provenance) carry no scope-bearing provenance at all and are
therefore **permanently hidden from MCP graph tools**. There is no
caller-facing flag to opt into them — exposing one would let any
caller surface unattributed edges from outside its scope, which
defeats the only authorization boundary available to the design.

Backfilling existing orphan edges with scope-bearing provenance is
tracked separately; until that lands, the MCP graph surface treats
them as if they did not exist. An operator-only ingestion or repair
path may surface them out-of-band, but that is not reachable from
this issue's tool set.

#### Scope resolution on the MCP path (prerequisite)

Today the MCP `search` handler hard-codes `visibility_allowlist =
vec![]` (see `crates/cairn-mcp/src/handler.rs::handle_search`),
which the search verb interprets as "no narrowing." That is the
existing fail-open we are explicitly *not* inheriting here. Graph
tools are stricter: an empty resolved scope set means **deny all**,
not "allow all."

This work depends on a concrete scope-resolution API on the MCP
path. The resolver must take **per-request context** — a zero-arg
resolver could only return one static scope set per server, which
breaks isolation in any deployment that multiplexes callers or
rotates sessions. The contract therefore takes a request context
and is invoked at every `tools/list` and `tools/call` boundary, not
once at construction:

**MCP transport reality check.** The pinned `rmcp` crate
threads a `RequestContext<RoleServer>` through `list_tools` and
`call_tool`, but it carries only `request_id`, `meta`,
`extensions`, and a peer handle — there is no built-in
authenticated session/principal field. Stdio MCP has no caller
identity whatsoever (the process on the other end of the pipe
is the only "caller"). HTTP-streamable / SSE transports could
plumb headers through `extensions`, but neither is in scope for
this PR.

Concrete consequences for this issue:

1. **Stdio transport (the only one this PR ships):** there is
   no per-call caller identity. `McpAuthContext` carries only
   `request_id` and a single deployment-resolved `principal:
   PrincipalId` injected at server-construction time from
   config. **Graph tools therefore advertise on stdio only when
   the operator explicitly opts into single-tenant mode** via
   `cairn.toml::[mcp.stdio] single_tenant = true`. Without that
   flag set, graph tools return `CapabilityUnavailable` even on
   a graph-capable store with a configured scope resolver — the
   conjunction in `graph_tools_available` includes a
   `single_tenant_stdio` precondition that reads this flag.

   This makes the trust boundary explicit: a shared MCP process
   without `single_tenant = true` cannot list or invoke
   `graph.*`. An operator who flips the flag is asserting that
   the process serves exactly one principal for its entire
   lifetime, which is the only configuration where the
   construction-time `principal` is a faithful authorization
   key. Multi-principal stdio servers must wait for the
   per-request identity work below.

   This is **not** "global static scopes": the resolver still
   runs per-request and the predicate is evaluated per-request.
   It is honest single-tenant behaviour with no implication of
   per-caller isolation — the spec stops promising what stdio
   cannot deliver.

2. **Future SSE / HTTP transports:** the implementation plan
   for those (separate issues) extends `McpAuthContext` with
   real per-request principal extraction from
   `RequestContext::extensions`. This spec reserves that
   field's role explicitly so we don't cement a single-tenant
   shape into the trait surface.

```rust
/// Per-request authorization context handed to the resolver.
///
/// On stdio transport (this PR), `principal` is fixed at
/// server construction and `request_id` varies per call. On
/// future network transports, both vary and `principal` is
/// extracted from `RequestContext::extensions` per request.
pub struct McpAuthContext<'a> {
    pub principal: &'a PrincipalId,
    pub request_id: &'a RequestId,
}

pub trait McpSessionScope: Send + Sync {
    /// Resolve the *requesting* session's allowed record scope ids.
    /// Empty Vec → fail closed (tool returns NotFound / empty).
    /// Err → resolver was unable to identify the caller; tool
    ///       returns CapabilityUnavailable rather than executing
    ///       on partial data.
    fn allowed_scopes(
        &self,
        ctx: &McpAuthContext<'_>,
    ) -> Result<Vec<ScopeTuple>, ScopeResolutionError>;
}
```

The MCP handler stores `Option<Arc<dyn McpSessionScope>>`, but
**reads it per-request** with the active `McpAuthContext`. Caching
the previous caller's scopes is forbidden — every tool invocation
re-resolves through the trait. `tools/list` either resolves with the
current request's context (single-tenant deployments still work,
multi-tenant deployments stay isolated) or, if pre-flight discovery
must happen before any caller is bound, returns an empty graph
manifest until the first authenticated request.

If a deployment is intentionally single-tenant, that is
expressible: a resolver that ignores `ctx` and returns a fixed
`Vec<ScopeTuple>` is a valid impl. The contract just makes per-request
isolation possible for the cases that need it.

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

The shared active-at-now predicate composes with this scope
filter — the latter is the §3.0a stable-lineage,
ScopeTuple-aware EXISTS, not a scalar `IN`. See §3.0a for the
canonical form; the SQL below sketches only the temporal part:

```sql
<§3.0a AuthorizedSource expansion>
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

### 3.0a Stable-lineage authorization predicate

`records` is bitemporally versioned: `record_id` identifies a
concrete version, `target_id` is the stable lineage key, and at
most one row per `target_id` carries `active = 1`. A graph edge's
`source_record_id` points at a *concrete* version that may be
superseded by a newer in-scope version moments later in the
normal upsert flow. Authorizing strictly on
`r.record_id = e.source_record_id AND r.active = 1` would make
graph reads go dark whenever the source record is replaced —
even though the underlying fact is still authoritative.

#### `records.scope` is JSON, not a scalar

In this repo `records.scope` stores the canonical JSON
serialization of `ScopeTuple` (dimensions: `tenant`, `workspace`,
`session_id`, `entity`, `user`, `agent`; `None` fields omitted).
Existing scope-aware queries in `cairn-store-sqlite` use
`json_extract(scope, '$.<dim>')` with the
`coalesce(json_extract(...), '') = coalesce(?N, '')` idiom (see
`crates/cairn-store-sqlite/src/store/tx.rs::payload_hash_count_in_scope`
and `crates/cairn-core/src/domain/filter.rs:697-704`). In that
existing semantics, **`None` on a dimension means "record's
dimension is NULL"** — exact null-equality, **not** a wildcard.

The graph auth predicate must use the same exact-equality
semantics. Reusing `ScopeTuple` *as a wildcard selector* would
silently widen authorization (a resolver returning
`tenant=Some("a"), user=None` would authorize every user in
tenant `a`, instead of only records whose user is unset). That
is unacceptable.

The contract:

```rust
// McpSessionScope returns concrete scope tuples — fully
// specified to the same exactness the store enforces. The
// `Vec` is the union of authorized scopes; each entry is matched
// with the existing exact null-equality predicate, never with
// wildcard semantics.
pub trait McpSessionScope: Send + Sync {
    fn allowed_scopes(
        &self,
        ctx: &McpAuthContext<'_>,
    ) -> Result<Vec<ScopeTuple>, ScopeResolutionError>;
}
```

A record's scope matches an entry iff **every dimension equals
the entry's value** under the same `coalesce(..., '') =
coalesce(?, '')` idiom the rest of the store uses. There is no
"any" wildcard. If a deployment needs to authorize across a
sub-tree (e.g. all users in tenant `a`), the resolver enumerates
the concrete tuples and returns them as separate `Vec` entries
— it does not introduce a pattern language. A `Vec` of multiple
entries is OR'd together.

If wildcard semantics ever become necessary, they land as a
separate pattern type with its own predicate in a follow-up
issue, not by overloading `ScopeTuple`.

#### The canonical authorization predicate

Two non-negotiable rules apply to every rendering:

1. **Every supported scope dimension is bound on every entry**,
   not just `Some(_)` ones. `None` binds the SQL parameter as
   `NULL` and the `coalesce(..., '') = coalesce(?, '')`
   comparison enforces exact null-equality. Omitting a
   dimension's WHERE clause would silently widen the predicate
   into a wildcard, exactly the failure mode §3.0a forbids.
2. **Authorization is anchored to immutable edge provenance**,
   not to the current active head's scope. See §3.0a-bis
   below.

```sql
-- AuthorizedSource(:src_record_id) — the canonical predicate.
-- One EXISTS per ScopeTuple entry, OR'd together. EVERY
-- supported dimension (tenant, workspace, session_id, entity,
-- user, agent — six dimensions per ScopeTuple) is compared on
-- every entry. None entries bind a NULL parameter; the
-- coalesce idiom makes that an exact "dimension IS NULL" check
-- rather than a wildcard.
EXISTS (
  SELECT 1
  FROM records r_src
  JOIN records r_active
    ON r_active.target_id = r_src.target_id
  WHERE r_src.record_id   = :src_record_id
    AND r_active.active     = 1
    AND r_active.tombstoned = 0
    -- Authorization is keyed off the SOURCE VERSION's stored
    -- scope (immutable provenance), AND requires a matching
    -- active head on the lineage to prove the chain is not
    -- fully tombstoned. Two separate predicates joined to one
    -- ScopeTuple entry — both must hold.
    AND coalesce(json_extract(r_src.scope,    '$.tenant'),    '')
        = coalesce(?, '')
    AND coalesce(json_extract(r_src.scope,    '$.workspace'), '')
        = coalesce(?, '')
    AND coalesce(json_extract(r_src.scope,    '$.session_id'),'')
        = coalesce(?, '')
    AND coalesce(json_extract(r_src.scope,    '$.entity'),    '')
        = coalesce(?, '')
    AND coalesce(json_extract(r_src.scope,    '$.user'),      '')
        = coalesce(?, '')
    AND coalesce(json_extract(r_src.scope,    '$.agent'),     '')
        = coalesce(?, '')
)
```

A `Vec<ScopeTuple>` with N entries produces N parallel `EXISTS`
clauses joined by `OR`. The exact rendering is an implementation
detail; the spec requirements are (a) every dimension bound on
every entry, (b) `r_src` carries the scope predicate (not
`r_active`), and (c) authorization SQL never reads `scope` as a
scalar.

#### §3.0a-bis: immutable provenance, not lineage re-scope

A target chain can be re-scoped (a new active version with a
different `ScopeTuple` than its predecessors). If we keyed
authorization off the active head's scope, **historical edges
would silently re-scope themselves** — an edge sourced from a
record originally in scope `S_a` would become visible to scope
`S_b` callers the moment a same-target record is upserted into
`S_b`. That is a real cross-scope leak, not a versioning
nicety.

Authorization therefore reads scope from `r_src` (the version
that *produced* the edge), which is immutable: `records` rows
are never re-scoped in place; a new scope is a new
`record_id`/`target_id` row, and the historical row's scope
column never changes. The `r_active` join still gates against
full-chain tombstoning (no active row remaining anywhere on
the lineage hides the edge), but it does not contribute scope
matching.

A test must cover: a target_id chain whose original version is
in `S_a` and whose newest active version is in `S_b`. A caller
authorized for `S_b` does **not** see the edge produced by the
older `S_a` version. A caller authorized for `S_a` does see it
(provided some active row still exists on the lineage).

Tests:

1. An edge whose `source_record_id` is a *superseded*
   (`active = 0`) record **stays visible to callers
   authorized for the source version's scope**, provided some
   active row still exists on the same `target_id` chain. The
   active head's scope is irrelevant to authorization (§3.0a-bis).
2. Tombstoning the entire `target_id` chain (no `active = 1`
   row remaining) removes the edge.
3. A `ScopeTuple` entry with `tenant = Some("a"), user = None`
   matches **only** records whose `tenant = "a"` AND `user IS
   NULL` — not records carrying any user inside tenant `a`.
   Asserts there is no wildcard semantics — every dimension is
   bound on every entry.
4. A `Vec` with two `ScopeTuple` entries authorizes the union of
   records matching either entry exactly.
5. **Lineage re-scope adversarial test:** a target chain with
   the original version in scope `S_a` and the newest active
   version in `S_b`. A caller authorized for `S_b` (only) does
   **not** see edges sourced from the older `S_a` version. A
   caller authorized for `S_a` (only) does see them as long as
   any active row still exists on the lineage.
6. **Tombstoned endpoint adversarial test:** an active,
   in-scope edge between live node `A` and tombstoned node `B`
   (`entity_nodes.expired_at IS NOT NULL` on `B`). The edge
   does NOT appear in `visible_edges`, `get_neighbors(A)`,
   `query` (BFS from `A` cannot hop to `B`), `timeline(A)`,
   or `surprising_connections({A, B})`. Asserts node-liveness
   filtering at the shared edge primitive.

### 3.0b Set-valued parameter binding

The current SQLite open path (`crates/cairn-store-sqlite/src/
open.rs`) registers `vec0` for the embedding extension but does
**not** load the `carray` / `rarray` virtual-table extension. The
spec therefore uses **generated `IN (?, ?, ...)` placeholders**
for every set-valued parameter (`:allowed_scopes`, `:frontier`,
`:visited`, `:input`), not `rarray(...)`. Re-prepared statements
per wave are acceptable — the cost is negligible against the
correctness gain of running on the existing connection setup.

This applies to **all** SQL in this spec, not just the BFS
frontier. Earlier `rarray(...)` mentions in shared predicates are
read as `IN (?, ?, ...)` for implementation purposes.

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
--     caller's allowed scope set. Orphan edges
--     (source_record_id IS NULL) are unconditionally excluded —
--     no flag opts into them, because they carry no scope-bearing
--     provenance and exposing them would bypass authorization.
-- (c) node liveness: BOTH endpoints must be in visible_nodes,
--     so a tombstoned-but-not-yet-cleaned-up node id cannot
--     appear in any edge result. Without this, a deleted
--     entity would still leak through get_neighbors / timeline
--     / surprising_connections, and the BFS loop's
--     fetch_node(other_id) would fail to hydrate hops that the
--     edge query had already approved.
WITH visible_edges_raw AS (
  SELECT e.*
  FROM entity_edges e
  WHERE e.expired_at IS NULL
    AND e.valid_at  <= :now
    AND (e.invalid_at IS NULL OR e.invalid_at > :now)
    AND e.source_record_id IS NOT NULL
    AND EXISTS (                                  -- §3.0a
      SELECT 1
      FROM records r_src
      JOIN records r_active
        ON r_active.target_id = r_src.target_id
      WHERE r_src.record_id   = e.source_record_id
        AND <ScopeTuple-match-clause>             -- §3.0a expansion
        AND r_active.active     = 1
        AND r_active.tombstoned = 0
    )
),
visible_nodes AS (...),                           -- §3.0 (defined below)
visible_edges AS (
  SELECT e.*
  FROM visible_edges_raw e
  WHERE e.source_id IN (SELECT id FROM visible_nodes)
    AND e.target_id IN (SELECT id FROM visible_nodes)
)
```

This makes `visible_edges` reflect a self-consistent subgraph:
every edge it returns has two live, in-scope endpoints. The BFS
loop's `fetch_node` cannot hand back an `other_id` that
visible_nodes rejects, and `get_neighbors` / `timeline` /
`surprising_connections` cannot return an edge whose endpoint
has been tombstoned out from under it. The `visible_nodes` CTE
must therefore reference `visible_edges_raw` (the
provenance-filtered raw form) for its "active edges" provenance
branch — otherwise we'd have a circular CTE.

The stable-lineage predicate from §3.0a is part of the scope
check by definition — a tombstoned target chain cannot continue
to authorize edges sourced from it, but normal supersession of a
single record version does not. Every scope EXISTS clause in
this spec applies the same predicate.

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
3. **Past-invalidated edges** — at least one
   `entity_edges` row that is **past-invalidated**
   (`invalid_at <= :now`), non-tombstoned, and whose
   `source_record_id` is in `allowed_scopes`. Covers the
   "all my edges aged out but I still exist" case.

   **Tombstoned edges (`expired_at IS NOT NULL`) do not count**
   — they would resurrect entities operators have deleted.

   **Future-dated edges (`valid_at > :now`) do not count
   either** — they are invisible to every other tool path, so
   they must not be the sole provenance that makes a node
   discoverable. A node reachable only via a future-scheduled
   edge stays `NotFound` until that edge becomes active.

Direct reads of `entity_nodes` without composing one of the three
provenance branches are forbidden in tool SQL.

```sql
visible_nodes AS (
  SELECT n.*
  FROM entity_nodes n
  WHERE n.expired_at IS NULL
    AND (
      EXISTS (
        -- visible_edges_raw, not visible_edges: the latter
        -- requires both endpoints in visible_nodes, which is
        -- this CTE — circular. Use the provenance-filtered
        -- raw form here.
        SELECT 1 FROM visible_edges_raw e
        WHERE e.source_id = n.id OR e.target_id = n.id
      )
      OR EXISTS (
        -- entity_episodes columns per migration 0044:
        --   episode_id     TEXT REFERENCES records(record_id)
        --   entity_node_id TEXT REFERENCES entity_nodes(id)
        -- Episode provenance uses the §3.0a stable-lineage
        -- predicate so a normal record supersession does not
        -- transiently hide the entity. Tombstoning the entire
        -- target chain (no active version remains in scope)
        -- removes visibility.
        SELECT 1 FROM entity_episodes ep
        JOIN records r_src
          ON r_src.record_id = ep.episode_id
        JOIN records r_active
          ON r_active.target_id = r_src.target_id
        WHERE ep.entity_node_id = n.id
          AND r_active.active     = 1
          AND r_active.tombstoned = 0
          AND <ScopeTuple-match-clause>           -- §3.0a expansion
      )
      OR EXISTS (
        -- Past-invalidated, non-tombstoned, NOT future-dated.
        -- Future-dated edges (valid_at > now) cannot be the sole
        -- provenance that makes a node discoverable — they are
        -- invisible to every other tool path. Tombstoned edges
        -- (expired_at IS NOT NULL) cannot resurrect deleted
        -- entities. Authorization uses the stable-lineage
        -- predicate (§3.0a), not the concrete record_id, so
        -- normal record supersession cannot transiently hide
        -- the node mid-update.
        SELECT 1 FROM entity_edges he
        WHERE (he.source_id = n.id OR he.target_id = n.id)
          AND he.expired_at IS NULL
          AND he.valid_at   <= :now                 -- not future
          AND he.invalid_at IS NOT NULL
          AND he.invalid_at <= :now                 -- past-invalid
          AND EXISTS (                              -- §3.0a
            SELECT 1
            FROM records r_src
            JOIN records r_active
              ON r_active.target_id = r_src.target_id
            WHERE r_src.record_id   = he.source_record_id
              AND <ScopeTuple-match-clause>         -- §3.0a expansion
              AND r_active.active     = 1
              AND r_active.tombstoned = 0
          )
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

**Disambiguated input.** The MCP tool wire shape is two mutually
exclusive **top-level** arguments: `{"id": "<ulid>"}` xor
`{"name": "<canonical name>"}`. Exactly one must be set; the
schemars schema enforces this with `oneOf` over two
single-property objects. The earlier `id_or_name` overload is
dropped — passing a single string into a single column is
correct only when ids and names cannot collide, and we cannot
guarantee that for arbitrary content.

The Rust binding uses `#[serde(untagged, deny_unknown_fields)]`
so the wire payload stays at the top level (no externally-tagged
`{"by_id": {...}}` wrapper, which the prose does not match):

```rust
#[derive(Deserialize, JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
pub enum GetEntityArgs {
    ById  { id: String },
    ByName { name: String },
}
```

`untagged` makes serde dispatch on field presence: a payload
with `id` deserializes into `ById`, a payload with `name`
deserializes into `ByName`, and a payload with both, neither,
or any other field is rejected. The generated JSON schema is
`oneOf` two single-property objects with `additionalProperties:
false`, matching the prose. A wire-format test asserts that
`{"id": "..."}`, `{"name": "..."}` round-trip and that `{}`,
`{"id": "...", "name": "..."}`, and `{"foo": "bar"}` are all
rejected.

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
primitive), never from `entity_nodes` directly. Per §2.1.0 the
projection is **id-only** plus an in-scope edge count; `name`
and `summary` are never returned because they have no per-scope
provenance.

```sql
-- ById
WITH visible_edges AS (...), visible_nodes AS (...)   -- §3.0
SELECT
  v.id,
  (SELECT COUNT(*) FROM visible_edges e
     WHERE e.source_id = v.id OR e.target_id = v.id) AS edge_count
FROM visible_nodes v
WHERE v.id = ?1;

-- ByName
WITH visible_edges AS (...), visible_nodes AS (...)
SELECT
  v.id,
  (SELECT COUNT(*) FROM visible_edges e
     WHERE e.source_id = v.id OR e.target_id = v.id) AS edge_count
FROM visible_nodes v
WHERE v.name_norm = ?1;   -- ?1 = normalize_entity_name(input)
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
4. **Future-only provenance (negative test):** an entity whose
   only edge has `valid_at = now + 1h` is `NotFound` for every
   tool. Once the clock advances past `valid_at`, it becomes
   visible. Asserts visible_nodes does not surface entities
   from edges that have not yet become active.
5. **Tombstoned-record episode provenance (negative test):**
   an entity whose only provenance is an `entity_episodes` row
   pointing to a now-tombstoned (`tombstoned = 1`) record is
   `NotFound`. Asserts deletion of the source record removes
   the entity from MCP visibility, not just from the records
   surface.
6. **Unauthenticated discovery (negative test):** a
   `tools/list` request whose `McpAuthContext` resolves to
   `Ok(vec![])` or `Err(_)` returns *no* `graph.*` tools.
   Same for `tools/call` — invocation in those states returns
   `CapabilityUnavailable`, not partial data.

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

Returned edges are **id-only** for the *other* endpoint per
§2.1.0 — no name, no summary, no joined `entity_nodes` payload.
The endpoint id is filtered through `visible_nodes` so an
out-of-scope neighbor id is also suppressed (the edge appears
only when both endpoints are visible to the caller).

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

Per §3.0b the SQL uses generated `IN (?, ?, ...)` placeholder
lists, not `rarray(...)`. The implementation re-prepares the
statement per wave because the frontier and visited sizes change.
The placeholder spans below are written as `IN (?,?,...)` for
brevity; at runtime they expand to exactly `frontier.len()` and
`visited.len()` `?` markers respectively.

```sql
WITH visible_edges AS (...),     -- §3.0 — temporal + scope baked in
edge_with_other AS (
  -- Compute other_id once, in an inner CTE, so the outer SELECT
  -- can name it in WHERE / PARTITION BY without alias-scoping
  -- issues.
  SELECT
    e.id, e.source_id, e.target_id, e.relation, e.confidence_score,
    CASE WHEN e.source_id IN (?,?,...)            -- :frontier
         THEN e.target_id ELSE e.source_id END AS other_id
  FROM visible_edges e
  WHERE e.source_id IN (?,?,...)                  -- :frontier
     OR e.target_id IN (?,?,...)                  -- :frontier
),
candidate AS (
  SELECT
    id, source_id, target_id, relation, confidence_score, other_id,
    ROW_NUMBER() OVER (
      PARTITION BY other_id
      ORDER BY confidence_score DESC, id ASC      -- deterministic
    ) AS rn
  FROM edge_with_other
  WHERE other_id NOT IN (?,?,...)                 -- :visited
)
SELECT id, source_id, target_id, relation, confidence_score, other_id
FROM candidate
WHERE rn = 1
ORDER BY confidence_score DESC, id ASC
LIMIT ?;                                          -- :wave_cap
```

When `:visited` is empty (first wave) the implementation omits
the `WHERE other_id NOT IN (...)` clause entirely rather than
emitting a degenerate `IN ()`. Same trick for the frontier on
unreachable seeds — handled in the Rust loop, not in SQL.

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
- Per §3.0b, `IN (?,?,...)` with re-prepared statements per
  wave is the binding strategy — `rarray`/`carray` is not loaded
  in the open path and is not relied on anywhere in this spec.

### 3.4 `graph.timeline`

Timeline is the audit-friendly view, but it does **not** silently
broaden temporal visibility. By default it returns the same set as
`visible_edges` (active-at-now AND in-scope), just sorted by
`valid_at` for the historical-narrative read pattern. Two
**explicit** flags relax temporal filtering, each independently:

- `include_history: bool` — also return edges that *were* current
  at some point but are no longer (`invalid_at IS NOT NULL` OR
  `valid_at > now`). Without this flag, future-dated and
  already-invalidated edges are excluded — same as every other
  tool.
- `include_expired: bool` — also return tombstoned edges
  (`expired_at IS NOT NULL`). Independent of `include_history`.

The scope predicate is **always** applied. There is no flag that
relaxes it.

```sql
WITH scope_filtered AS (
  -- Same stable-lineage predicate as visible_edges (§3.0a).
  -- Tombstoning the target chain removes edges from the
  -- timeline; normal record supersession does not.
  SELECT e.* FROM entity_edges e
  WHERE e.source_record_id IS NOT NULL
    AND EXISTS (
      SELECT 1
      FROM records r_src
      JOIN records r_active
        ON r_active.target_id = r_src.target_id
      WHERE r_src.record_id   = e.source_record_id
        AND <ScopeTuple-match-clause>         -- §3.0a expansion
        AND r_active.active     = 1
        AND r_active.tombstoned = 0
    )
),
visible_nodes AS (...)                         -- §3.0
SELECT id, source_id, target_id, relation, confidence_score,
       valid_at, invalid_at, created_at, expired_at,
       tombstone_reason, source_record_id
FROM scope_filtered
WHERE (source_id = ?seed OR target_id = ?seed)
  -- Endpoint liveness (parity with visible_edges in §3.0):
  -- both endpoints must be in visible_nodes so a tombstoned
  -- node id cannot leak through the audit view.
  AND source_id IN (SELECT id FROM visible_nodes)
  AND target_id IN (SELECT id FROM visible_nodes)
  -- Tombstone gate
  AND (:include_expired = 1 OR expired_at IS NULL)
  -- Temporal gate: drop unless include_history opts in.
  -- Defaults match the active-at-now predicate from §3.0 so
  -- the default timeline cannot leak future-dated or already-
  -- invalidated edges.
  AND (
    :include_history = 1
    OR (
      valid_at <= :now
      AND (invalid_at IS NULL OR invalid_at > :now)
    )
  )
ORDER BY valid_at ASC, created_at ASC;
```

Negative tests:

1. Out-of-scope `source_record_id` → absent from the timeline
   regardless of either flag.
2. `valid_at = now + 1h` with `include_history = false` → absent.
   With `include_history = true` → present.
3. `invalid_at = now - 1h` with `include_history = false` →
   absent (already invalidated). With `include_history = true` →
   present.
4. `expired_at IS NOT NULL` with `include_expired = false` →
   absent regardless of `include_history`.
5. **Tombstoned target chain** → absent from the timeline
   regardless of any flag combination. Asserts the §3.0a
   stable-lineage predicate is applied to the audit view.
6. **Superseded record version (active = 0 on the exact
   `source_record_id`, but a newer version of the same
   `target_id` is `active = 1` and in scope)** → still
   **present** in the timeline. Asserts that normal record
   supersession does not transiently hide the edge.
7. **Lineage re-scope (active head out of scope, source version
   in scope)**: a target chain whose original `source_record_id`
   row is in scope `S_a` and whose newest active version is in
   `S_b`. A caller authorized for `S_a` (only) → edge **present**
   in the timeline, because §3.0a-bis keys authorization off the
   immutable `r_src.scope`, not the active head. A caller
   authorized for `S_b` (only) → **absent** for the same reason.
   Asserts the timeline does not silently re-scope historical
   edges when a different-scope head appears.
8. **Tombstoned endpoint** (parity with the cross-tool test in
   §3.0): an active in-scope edge from live node `A` to
   tombstoned node `B` (`entity_nodes.expired_at IS NOT NULL`).
   `timeline(A)` does **not** return the edge regardless of
   `include_history` / `include_expired`. Asserts timeline
   composes the same endpoint-liveness predicate as the other
   tools.

### 3.5 `graph.surprising_connections`

Score: `confidence_score * (1 + cross_scope_bonus)`.

The bonus is keyed on **actual scope id**, not `source_record_id`,
so the ranking is genuinely cross-scope rather than just
cross-record. A dataset with many records inside one scope must
not award the bonus to same-scope edges that merely come from a
non-modal record — that would make "surprise" hits semantically
unreliable.

cross_scope_bonus = 1.0 iff `records.scope` for the edge's
`source_record_id` is **outside** the modal `records.scope`
computed across all in-scope edges that touch the input entities.
0.0 otherwise. The modal-scope computation joins through
`records`, the same way the §3.0 scope filter does, so the
ranking is consistent with the visibility model.

Every CTE that reads `entity_edges` reuses the **shared active-at-now
predicate** from §3 — same `expired_at IS NULL AND valid_at <= :now AND
(invalid_at IS NULL OR invalid_at > :now)` clause that `get_neighbors`,
`get_entity` edge count, and `graph.query` apply. This is required:
without it, `surprising_connections` would re-leak future-dated edges
and silently drop currently-active edges that carry a future
`invalid_at`, which the rest of the API does not.

```sql
WITH visible_edges AS (...),                -- §3.0: temporal + scope
     input(id) AS (VALUES (?1), (?2) /* … */),
     edge_scope AS (
       SELECT e.id AS edge_id, r.scope AS scope_id, e.*
       FROM visible_edges e
       JOIN records r ON r.record_id = e.source_record_id
     ),
     modal_scope AS (
       -- Most common scope across all in-scope edges that touch
       -- any input entity. Ties broken deterministically by
       -- scope id ASC.
       SELECT scope_id
       FROM edge_scope es
       JOIN input i
         ON (es.source_id = i.id OR es.target_id = i.id)
       GROUP BY scope_id
       ORDER BY COUNT(*) DESC, scope_id ASC
       LIMIT 1
     ),
     scored AS (
       SELECT es.*,
              es.confidence_score *
              (1.0 + CASE
                       WHEN es.scope_id <> (SELECT scope_id FROM modal_scope)
                       THEN 1.0
                       ELSE 0.0
                     END) AS score
       FROM edge_scope es
       WHERE es.source_id IN input
         AND es.target_id IN input
     )
SELECT * FROM scored ORDER BY score DESC, edge_id ASC LIMIT ?n;
```

`visible_edges` is the single source of truth for "real, in-scope"
edges; `edge_scope` annotates each with its actual scope id;
`modal_scope` derives the dominant scope to compare against.
Future-dated, future-invalidated, out-of-scope edges, *and*
edges that look cross-record but are same-scope cannot enter
ranking through any path.

Final SQL refines the modal join — sketch above; the implementation
may use `Vec<RecordId>` accumulation in Rust if the SQL gets unwieldy,
but the temporal predicate is non-negotiable.

Adversarial test cases (mandatory):

- An edge with `valid_at = now + 1h` MUST be excluded from the
  result set and from the modal-scope computation.
- An active edge with `invalid_at = now + 1h` MUST be **included**
  (currently real, future-invalidated) — this is the case a naive
  `invalid_at IS NULL` filter incorrectly drops.
- An active, in-window edge whose `source_record_id` is outside
  `allowed_scopes` MUST be excluded from both the modal-scope
  computation and the final projection — out-of-scope edges
  cannot surface as a "surprise" hit through any path.
- A dataset with **many in-scope records all in scope `S`** must
  award `cross_scope_bonus = 0` to every edge — non-modal record
  ids inside the same scope must not falsely score as cross-scope.
- A dataset with edges in scopes `{S_a, S_a, S_a, S_b}` must score
  the `S_b` edge with `bonus = 1.0` and the `S_a` edges with
  `bonus = 0.0` — confirming the comparison is on scope ids, not
  on `source_record_id`.

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
if let Some(_decl) = GRAPH_TOOLS.iter().find(|d| d.name == name.as_ref()) {
    let ctx = self.auth_context_from(&request_context)?;

    // SINGLE-PASS materialization — `materialize_graph_request`
    // resolves store + scope + `allowed_scopes` exactly once and
    // returns the materialized request bundle. There is no
    // separate boolean predicate that re-resolves later, so a
    // transient resolver Err or Ok(empty) cannot succeed in a
    // preflight check and then panic on a second call. The
    // helper is shared with `list_tools` (§2.1).
    let req = match self.materialize_graph_request(&ctx) {
        Ok(r) => r,
        Err(_) => return Ok(capability_unavailable_result(&name)),
    };

    let queries = GraphQueries::new(req.store, req.allowed, req.now_ms);
    return Ok(graph_tools::dispatch(&queries, &name, arguments).await);
}
```

`mcp_graph_tools_available` (Plan A `CairnConfig` predicate) stays
a **pure precondition** over `(scope, transport, store_caps)` — it
does not call the resolver. The single resolver call lives inside
`materialize_graph_request`, which both `list_tools` and `call_tool`
go through. There is exactly one `allowed_scopes(ctx)` evaluation
per request, so no within-request TOCTOU is possible. The dispatch
helper's signature still carries the resolved scopes via
`GraphQueries`, so even an internal bug that bypassed the gate
could not produce an unscoped read (no method on `GraphQueries`
omits the scope predicate — it is baked into every CTE). No
`.expect()` calls appear on the request-time authorization path.

```rust
pub struct GraphQueries<'a> {
    store: &'a dyn MemoryStore,
    allowed_scopes: Vec<ScopeTuple>,    // §3.0a; never Vec::new()
                                        // here — empty fail-closes
                                        // upstream
    now: UnixMillis,
}

pub async fn dispatch(
    queries: &GraphQueries<'_>,
    name: &str,
    arguments: Option<serde_json::Map<String, Value>>,
) -> CallToolResult { /* … */ }
```

`graph_tools::dispatch` parses args with `serde_json::from_value`,
invokes the matching `GraphQueries` method, serializes the result.
Errors map to `CallToolResult::error` mirroring `handle_search`.

The scope-resolution check fires **before** any store work; a
missing resolver, an `Err(_)`, or an `Ok(vec![])` short-circuits
to `CapabilityUnavailable`. There is no path that constructs
`GraphQueries` without a non-empty `allowed_scopes` slice — making
unscoped graph execution structurally impossible.

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

The shim is a **byte-oriented relay**, not a line-oriented one,
because line-based forwarding via `AsyncBufReadExt::lines()`
silently strips trailing `\r` from CRLF input and re-emits `\n`
— breaking any harness that relies on its exact framing bytes.
The implementation reads bytes directly with `AsyncReadExt`,
maintains a small ring buffer, and emits frames at every `\n`
boundary while preserving the bytes between framers exactly as
received (including a trailing `\r` if present).

- Spawn one task reading `tokio::io::stdin()` with `read_buf`
  into a `Vec<u8>` accumulator. The accumulator starts at
  64 KiB and **grows up to a 16 MiB hard cap** to handle
  legitimately large MCP frames (e.g. tool-call results carrying
  embedded blobs). On overflow past 16 MiB without seeing a `\n`
  the relay returns a `TransportError::FrameTooLarge`,
  `tracing::error!`s the size, and shuts the session down
  cleanly — never silently drops or truncates bytes.
- Scan the buffered bytes for `\n` boundaries. Each frame is the
  byte slice from the previous boundary up to and including the
  `\n`.
- A frame is **dropped** iff its body (the bytes before any
  trailing `\r\n` / `\n`) is empty or all whitespace. Drop
  decision is made on a copy; the *retained* frame is forwarded
  byte-for-byte from the original buffer slice — `\r\n` stays
  `\r\n`, `\n` stays `\n`.
- Forwarded bytes go to the write half of a
  `tokio::io::duplex(64 * 1024)` channel; the read half is
  handed to rmcp as its stdin.
- Cancellation: when stdin reaches EOF the relay mirrors
  rmcp's `JsonRpcMessageCodec::decode_eof()` semantics on the
  final buffered fragment. The fragment is parsed once as
  JSON; if it parses **and** is non-empty after trimming, the
  bytes are forwarded with a synthetic trailing `\n` so rmcp's
  framer accepts them as a final frame. If the fragment is
  empty or fails to parse, it is dropped with a
  `tracing::warn!`. Then the duplex writer is closed and rmcp
  observes EOF cleanly.

  This preserves the legitimate "client writes one last
  request and closes stdin" pattern (the request would
  otherwise be silently lost on shutdown) while still
  protecting rmcp from genuinely truncated/malformed trailing
  bytes. Discarding a *valid* trailing frame would be a
  user-visible correctness failure because the server would
  just see clean EOF.

**Invariants the shim preserves:**

1. Every retained frame is forwarded **byte-for-byte** from the
   original input buffer — no re-encoding, no line-ending
   normalization, no trimming. CRLF input stays CRLF; LF stays
   LF.
2. The accumulator holds at most one un-emitted frame at a
   time, capped at 16 MiB; oversized frames terminate the
   session with `FrameTooLarge` rather than violate the
   byte-preservation guarantee. No reordering, since stdin has
   a single writer.
3. Stdout is untouched — server-to-host frames are not
   normalized.

Tests:

1. A mix of `\n`- and `\r\n`-terminated frames interleaved with
   blank lines: the rmcp side receives the non-empty frames
   with their original line endings intact.
2. A single 8 MiB frame with embedded JSON: forwarded
   end-to-end without truncation. Asserts the accumulator grows
   correctly up to (but not past) the 16 MiB cap.
3. A 16+ MiB frame with no `\n`: the relay returns
   `TransportError::FrameTooLarge` and the session terminates
   cleanly. Asserts oversized input never reaches rmcp as a
   partial frame.
4. **EOF with a valid non-newline-terminated final frame**:
   `{"jsonrpc":"2.0","id":1,"method":"ping"}` followed
   immediately by EOF (no `\n`). The relay parses the
   trailing bytes, finds valid JSON, and forwards them with a
   synthetic `\n` so rmcp processes the final request. The
   request must NOT be silently dropped — that would lose a
   legitimate shutdown-time tool call.
5. **EOF with truncated/malformed trailing bytes**:
   `{"jsonrpc":"2.0",` followed immediately by EOF. JSON
   parse fails; the bytes are discarded with a
   `tracing::warn!`, rmcp observes clean EOF, no malformed
   frame reaches the framer.

This is the same pattern Graphify ships and the one called out in the
issue.

## 6. Status / capability advertisement

Capability discovery is **the same predicate everywhere** and is
**evaluated per-request**, not once at construction. The predicate
is the conjunction of (a) store advertises `graph_edges` and (b)
scope resolution returns a usable set for the *current* caller.
"Resolver configured" alone is not sufficient — see §2.1 for why.

```rust
// One predicate, used at both list_tools and call_tool sites.
fn graph_tools_available(
    store: Option<&dyn MemoryStore>,
    scope: Option<&dyn McpSessionScope>,
    config: &CairnConfig,
    transport: McpTransport,
    ctx: &McpAuthContext<'_>,
) -> bool {
    // Stdio precondition: graph tools advertise only when the
    // operator has opted into single-tenant mode. A shared
    // stdio process serves exactly one principal for its
    // lifetime, which is the only configuration where the
    // construction-time `principal` is a faithful auth key.
    if transport == McpTransport::Stdio
        && !config.mcp.stdio.single_tenant
    {
        return false;
    }
    let store_ok = store
        .map(|s| s.capabilities().graph_edges)
        .unwrap_or(false);
    if !store_ok { return false; }
    match scope {
        None => false,
        Some(s) => matches!(
            s.allowed_scopes(ctx),
            Ok(v) if !v.is_empty()
        ),
    }
}
```

A resolver that returns `Ok(vec![])` (deny-all for this caller)
correctly hides tools from that caller's `tools/list`; the same
resolver may return `Ok(vec!["scope-a"])` for a different caller
in the same server, listing tools for them. Per-request
isolation, not a server-global toggle.

**Out of scope for this PR:** changing `MCPServerCapabilities` or
`CairnMcpServer::capabilities()` to surface a derived
`graph_edges` flag. The current `MCPServerCapabilities` advertises
only transport and extension support; adding a graph-related field
is a contract/IDL change with conformance-suite implications that
do not belong in this issue. Discovery gating happens entirely
inside the MCP handler via `tools/list` filtering — clients
observe the gate by *not seeing* the tool, not by reading a
capability flag. If we later want explicit advertisement, that
lands as its own contract bump issue.

The in-PR signal is the manifest snapshot test, which covers the
full **transport × single_tenant × store × resolver** matrix
defined in §2.1. Only the
`(stdio, single_tenant=true, graph_edges=true, Ok(non-empty))`
cell may list any `graph.*` tools; every other cell returns the
standard 8-verb manifest with no graph entries. The matrix is
defined once in §2.1 and reused here — a snapshot test cannot
accidentally validate a fail-open manifest because both sites
consume the same `graph_tools_available` predicate.

Negative tests cover each "no" row: invoking `graph.get_entity` in
any of those states returns `CapabilityUnavailable` rather than
executing — defense in depth at dispatch mirrors the discovery
gate. A successful non-empty scope resolution **plus a graph-
capable store on a `single_tenant`-enabled stdio transport** is
the only configuration that may list or execute `graph.*`.

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
