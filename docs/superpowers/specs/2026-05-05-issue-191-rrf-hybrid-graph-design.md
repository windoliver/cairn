# Issue #191 — RRF Hybrid Search w/ 1-hop Graph Expansion

**Issue:** [#191](https://github.com/windoliver/cairn/issues/191)
**Brief refs:** §8.0 (search verb), §4 (MemoryStore), §6.11 (SQLite — `WITH RECURSIVE`)
**Updates:** #47 (FTS5), #48 (sqlite-vec), #49 (hybrid orchestration)
**Status:** spec — implementation pending

---

## 1. Goal

Add a third retrieval leg — 1-hop entity-graph neighborhood expansion — to the
existing 2-leg RRF hybrid search (FTS5 BM25 + sqlite-vec semantic ANN).
Confidence-aware so that `INFERRED`/`AMBIGUOUS`-edge neighbors are penalized
relative to `EXTRACTED`-edge neighbors. Surfaces non-obvious connections while
preserving the existing keyword/semantic recall floor.

## 2. Non-goals

- Multi-hop traversal (depth > 1). Reserved for a follow-up.
- Re-tuning RRF constant `k` away from 60. Keep canonical default.
- Replacing the cosine re-rank pass; graph leg feeds RRF, cosine pass remains.
- Online graph mutation. Graph leg is read-only.

## 3. Algorithm (final)

The graph leg performs a true 1-hop expansion: seed entities → edge →
**neighbor** (opposite-endpoint) entities → records mentioning the neighbor.
This surfaces records that do not contain the query terms but are connected
via the entity graph (the discovery case). Seeds are restricted to entities
mentioned by **visible records that match the query** so that no unauthorized
lexical data influences ranking.

```
# Step 1: visible_query_records — UNION of keyword and semantic hits
#         (both already scope/visibility-filtered upstream). Using only
#         BM25 would give paraphrased / low-lexical-overlap queries zero
#         graph expansion, defeating the leg's purpose precisely where
#         hybrid retrieval should help most.
visible_query_records = unique(bm25_hits.record_ids
                            ∪ semantic_hits.record_ids)            (≤100)

# Step 2: seed entities — only those mentioned by visible_query_records.
#         entity_episodes is the (node, record) join table.
seeds = SELECT DISTINCT entity_node_id
          FROM entity_episodes
         WHERE record_id IN visible_query_records

# Step 3: neighbor entities — opposite-endpoint nodes of edges touching seeds.
#         Edges must be live in BOTH bitemporal dimensions (invalid_at AND
#         expired_at), and the edge's *provenance record* must itself be
#         visible to the caller — otherwise hidden evidence could raise the
#         confidence of a visible neighbor.
neighbors = SELECT
              CASE WHEN e.source_id IN seeds THEN e.target_id ELSE e.source_id END
                AS neighbor_id,
              MAX(e.confidence_score) AS conf
            FROM entity_edges e
            JOIN records pr ON pr.id = e.source_record_id
           WHERE (e.source_id IN seeds OR e.target_id IN seeds)
             AND e.invalid_at IS NULL
             AND e.expired_at IS NULL
             AND e.confidence_score >= confidence_min
             AND e.source_record_id IS NOT NULL    -- orphan edges (NULL provenance) ineligible
             AND pr is visible to caller (auth_scope + visibility)
           GROUP BY neighbor_id
           HAVING neighbor_id NOT IN seeds      -- exclude self-references

# Step 4: candidate records — records mentioning a neighbor entity.
#         Auth (auth_scope + visibility) and the user filter all apply.
#         Dedup uses ranked_query_records (top 50 per lexical leg, ≤ 100),
#         NOT visible_query_records (overfetch pool, ≤ 400). Records
#         overfetched as seeds but ranked out of fusion can still surface.
graph_hits = SELECT r.id, n.conf
               FROM neighbors n
               JOIN entity_episodes ep ON ep.entity_node_id = n.neighbor_id
               JOIN records r          ON r.id = ep.record_id
              WHERE auth_scope predicate ON r
                AND visibility predicate ON r
                AND filter predicate ON r
                AND r.tombstoned = 0 AND r.active = 1
                AND r.id NOT IN ranked_query_records   -- dedup vs. RRF-eligible only
              GROUP BY r.id
              ORDER BY MAX(n.conf) DESC, r.updated_at DESC
              LIMIT 50

# Step 5: RRF fusion (graph leg's rank divided by its connecting-edge
#         confidence so INFERRED/AMBIGUOUS neighbors land below EXTRACTED).
contrib_graph(doc, rank, conf) = 1 / (k + rank / max(conf, ε))
contrib_other(doc, rank)       = 1 / (k + rank)
rrf_score(doc) = Σ contrib_*(doc, ...)

final = sort_desc(by rrf_score) → cosine rerank top-K → page
```

Constants: `k=60`, `leg_limit=50`, `confidence_min=0.5`, `ε=1e-3`.

**Trust boundary:** seeds derive from **records the caller can see and that
match the query**, not from a global entity FTS. An alias indexed only from
a hidden record cannot match — the lexical match happens against
`records_fts` (already scope-filtered), and entities are looked up by id
afterwards via `entity_episodes`. Tested in §10.2.

**Additive property (with rank-rescue, NOT discovery-only):** step 4
excludes `ranked_query_records` (the records actually fused into RRF —
top 50 of each lexical leg). It does NOT exclude the wider auth-only
seed pool (`visible_query_records`, ≤ 400). Two consequences callers
can rely on:

1. **No double-counting in the fusion pool**: a record present in the
   top-50 of either lexical leg cannot also appear via the graph leg.
   RRF won't see the same record twice.
2. **Rank-rescue is intentional**: a record overfetched as a graph
   seed but ranked outside the lexical top-50 (e.g., position 75) CAN
   re-enter via graph evidence. This rescues lexically-matched records
   that the lexical legs alone would have dropped, while the truly
   neighbor-only record (no lexical match at all) is the more
   striking case. Both are valid graph-leg hits; the leg is "additive
   with rank rescue", not "discovery-only".

Test §10.2's rank-rescue case asserts (1) rank-75 records re-enter via
graph and (2) the same record is not double-counted in fusion.

## 4. Architecture

### 4.1 Crate boundaries (unchanged)

- `cairn-core` — pure functions & traits; adds `GraphCandidate` and extends
  `HybridSearchInputs`/`HybridSearchParams`.
- `cairn-store-sqlite` — owns the `WITH RECURSIVE` SQL and the
  `search_graph_neighbors` impl.
- `cairn-cli` — adds `--confidence-min` flag.

`cairn-core` gains zero new external deps. Store gains zero. (Confirmed against
§3 dependency rule.)

### 4.2 Public types added to `cairn-core::search`

```rust
/// One graph-leg candidate: a record reached via a neighbor entity edge.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphCandidate {
    pub record_id: RecordId,
    /// Confidence score of the *connecting edge* (max if multiple edges
    /// reach the same record). Range `[0.0, 1.0]`. Used for the rank
    /// penalty in `hybrid_search`.
    pub edge_confidence_score: f32,
    /// 1-based rank of this candidate within the graph leg's output
    /// (i.e., its position in the §5.1 SQL result order). Carried
    /// explicitly so RRF fusion does not infer rank from
    /// (potentially out-of-order) hydrated list position. The graph
    /// SQL emits rows in `ORDER BY conf DESC, updated_at DESC`; this
    /// field captures that order before hydration shuffles ids.
    pub graph_rank: usize,
}
```

`HybridSearchInputs` gains:

```rust
/// Graph-leg candidates. Order in the Vec is irrelevant; rank is read
/// from `GraphCandidate::graph_rank` to keep fusion deterministic.
pub graph: Vec<GraphCandidate>,
```

`HybridSearchParams` gains:

```rust
/// `ε` floor for the graph-leg confidence penalty divisor. Default 1e-3.
pub confidence_floor: f32,
```

Backwards-compat: callers using `HybridSearchInputs::default()`-equivalent
struct literals must opt into `graph: vec![]`. We add a `..Default::default()`
constructor on inputs/params to keep the diff small.

### 4.3 New trait method on `MemoryStore`

```rust
async fn search_graph_neighbors(
    &self,
    args: &GraphNeighborsArgs<'_>,
) -> Result<Vec<GraphCandidate>, Self::Error>;
```

Where:

```rust
pub struct GraphNeighborsArgs<'a> {
    /// Record ids from the auth-only seed retrieval (UNIONed across
    /// keyword and semantic legs), up to 2 * GRAPH_SEED_OVERFETCH = 400.
    /// Seeds the graph traversal. Empty list ⇒ empty result.
    pub seed_record_ids: Vec<RecordId>,
    /// Record ids actually fused into RRF (top 50 of each filtered
    /// lexical leg, UNIONed, ≤ 100). Used as the dedup set in step 4
    /// of §5.1 — graph results exclude these so RRF cannot
    /// double-count, but seeds NOT in this list (overfetched rank
    /// 51-200 records) remain eligible for rank-rescue via graph
    /// evidence. Required: omitting this would force the trait impl
    /// to dedup against the wrong pool.
    pub ranked_record_ids: Vec<RecordId>,
    /// Pre-validated user-narrowing filter (timestamps, tags, kind/class
    /// narrowing). Applied **only** to the returned neighbor record
    /// (step 4). User narrowing must not erase otherwise-authorized
    /// edges based on where the edge was observed.
    pub filter: Option<ValidatedFilter<'a>>,
    /// Authorization scope tuple — the security predicate. Applied to
    /// BOTH the provenance record (step 3) and the neighbor record
    /// (step 4). `filter` is recall-narrowing only.
    ///
    /// **This field is part of a wider `MemoryStore` change.** Promoting
    /// `auth_scope` into a first-class field on the graph leg only would
    /// let policy drift between legs — the keyword leg would still
    /// authorize through `KeywordSearchArgs.filter`, and a careless
    /// caller could pass a narrower keyword filter than graph
    /// auth_scope (or vice versa). To prevent that, this issue also
    /// adds `auth_scope: ScopeTuple` to `KeywordSearchArgs` and
    /// `SemanticSearchArgs` (and so to `HybridSearchArgs`). The shared
    /// keyword-leg row-mapper applies it identically across all three
    /// legs and graph hydration. Callers construct `auth_scope` once
    /// per request from their auth context; the verb layer threads it
    /// through unchanged. This is a contract-version event (§12.1) and
    /// is the reason the bump is `0.4.0 → 0.5.0`, not a patch.
    pub auth_scope: ScopeTuple,
    /// Visibility values the caller is allowed to see. Applied to BOTH
    /// the neighbor record (step 4) AND the edge provenance record
    /// (step 3). Authorization.
    pub visibility_allowlist: Vec<MemoryVisibility>,
    /// Max candidates returned. = `HYBRID_LEG_LIMIT` from the orchestrator.
    pub limit: usize,
    /// `confidence_score` floor applied SQL-side to the edge step (step 3).
    pub confidence_min: f32,
}
```

**Predicate application (step 3 vs. step 4):** authorization predicates
(`auth_scope`, `visibility_allowlist`, tombstone, active) apply to BOTH
provenance and neighbor. The user-narrowing `filter` applies **only** to
the neighbor record. This split is the whole reason `auth_scope` is a
separate field on the args struct — overloading user filter onto
provenance would erase otherwise-authorized graph evidence based on
where the edge happened to be extracted.

| Predicate kind          | Step 3 (provenance `pr`) | Step 4 (neighbor `r`) |
|---|---|---|
| `auth_scope`            | applied | applied |
| `visibility_allowlist`  | applied | applied |
| `tombstoned = 0`        | applied | applied |
| `active = 1`            | applied | applied |
| `filter` (user narrow)  | **NOT** applied | applied |

This is the contract from day one — there is no "until the split lands"
deferral. The `auth_scope` field is mandatory on `GraphNeighborsArgs`;
the orchestrator (`do_search_hybrid`) extracts it from the same context
that builds visibility/scope predicates for the keyword leg, so the
three legs cannot drift.

Capability: extends **`MemoryStoreCapabilities`** with `graph_search:
bool` (see §8 — runtime capability, not config-derived). Stores without
the entity_graph schema or the `carray` extension return
`CapabilityUnavailable`. The hybrid path treats unavailable graph leg
the same as it currently treats an unavailable semantic leg: skip the
leg, RRF still runs on the survivors. The orchestrator reads
`MemoryStoreCapabilities::graph_search` (NOT `config::CapabilitySet`)
when deciding to dispatch the graph leg. Degradation is surfaced
explicitly per §5.3.

### 4.4 Extended args

`HybridSearchArgs` gains `pub confidence_min: f32` (threaded into the graph
leg only).

## 5. Store implementation

### 5.1 SQL — single statement (authorization-first, true 1-hop)

The query takes the **union of keyword + semantic leg over-fetched
record-ids** as a parameter (`?1` = bound list of `record_id`s, length ≤
`2 * GRAPH_SEED_OVERFETCH = 400` by default; configurable via
`SearchConfig.graph_seed_overfetch`). Both upstream legs already enforce scope/visibility,
so the union is fully authorized. That list is the **only** lexical
input; the graph leg does not run FTS5 itself, so a hidden alias on an
entity node cannot drive the match. The same list is reused later (Step 4)
to de-dup graph hits against everything the upstream legs already returned,
keeping the graph leg purely additive.

```sql
WITH RECURSIVE
    -- Step 1a: SEED set — over-fetched record ids from keyword+semantic
    -- legs (≤ 400 = 2 * GRAPH_SEED_OVERFETCH). Seeds the entity-mention
    -- traversal so broad queries do not lose connected results to a
    -- 50-per-leg cap.
    visible_query_records(id) AS (
        SELECT value FROM carray(?1)              -- 400-cap, bound by store layer
    ),
    -- Step 1b: RANKED set — record ids that actually enter RRF fusion
    -- (top 50 of each lexical leg, ≤ 100). Used ONLY for the step-4
    -- dedup so that an overfetched-but-not-ranked seed (rank 51-200)
    -- can still surface via graph evidence. Distinct from
    -- visible_query_records by design — see the broad-query test.
    ranked_query_records(id) AS (
        SELECT value FROM carray(?5)              -- 100-cap, bound by store layer
    ),
    -- Step 2: seed entity nodes — only those mentioned by visible_query_records.
    -- entity_episodes(episode_id REFERENCES records(record_id), entity_node_id, ...)
    seeds(id) AS (
        SELECT DISTINCT ep.entity_node_id
          FROM entity_episodes ep
         WHERE ep.episode_id IN (SELECT id FROM visible_query_records)
    ),
    -- Step 3: neighbor entities — opposite-endpoint of seed-touching edges.
    --         Edges are filtered on:
    --           (a) live in both bitemporal dimensions
    --               (invalid_at IS NULL AND expired_at IS NULL — the
    --               schema's live-edge predicate; matches §6.11 of the
    --               brief and migration 0043),
    --           (b) confidence floor (?2),
    --           (c) edge provenance is visible to the caller — the edge's
    --               source_record_id MUST also be in visible_query_records
    --               OR pass the same visibility/scope predicate. Without
    --               this filter, an edge extracted from a hidden record
    --               could raise the confidence aggregate of a visible
    --               neighbor, leaking unseen evidence into ranking.
    --         MAX(confidence_score) is computed AFTER the provenance
    --         filter so only authorized evidence contributes.
    neighbors(neighbor_id, conf) AS (
        SELECT neighbor_id, MAX(confidence_score)
          FROM (
            SELECT
                CASE WHEN e.source_id IN (SELECT id FROM seeds)
                     THEN e.target_id ELSE e.source_id END  AS neighbor_id,
                e.confidence_score                          AS confidence_score
              FROM entity_edges e
              JOIN records pr ON pr.record_id = e.source_record_id
             WHERE (e.source_id IN (SELECT id FROM seeds)
                 OR e.target_id IN (SELECT id FROM seeds))
               -- Bitemporal "live now" predicate matches the production
               -- read path used by `do_graph_edges` — both end-bounds
               -- not-fired AND both start-bounds satisfied. ?6 = now_event,
               -- ?7 = now_ingest (caller-supplied via the verb's
               -- `as_of_event_time` / `as_of_ingest_time`, defaulting to
               -- the request timestamp). Without start bounds, a
               -- future-dated edge would surface neighbors before the
               -- fact is valid.
               AND e.valid_at   <= ?6              -- event-time start bound
               AND e.created_at <= ?7              -- ingest-time start bound
               AND (e.invalid_at IS NULL OR e.invalid_at > ?6)
               AND (e.expired_at IS NULL OR e.expired_at > ?7)
               AND e.source_record_id IS NOT NULL  -- orphan edges excluded
               AND e.confidence_score >= ?2
               AND pr.tombstoned = 0
               AND pr.active = 1
               -- Authorization-only on provenance: auth_scope + visibility.
               -- User-narrowing `filter` is NOT applied here (see §4.3).
               AND <auth_scope predicate ON pr>
               AND <visibility predicate ON pr>
          )
         WHERE neighbor_id NOT IN (SELECT id FROM seeds)   -- exclude self-references explicitly
         GROUP BY neighbor_id
    )
-- Step 4: candidate records — records mentioning a neighbor entity.
--         Authorization (visibility + scope) AND the user-supplied filter
--         BOTH apply here. The result is de-duped against the seed list
--         so the leg is additive vs. keyword/semantic.
SELECT r.record_id, MAX(n.conf) AS conf
  FROM neighbors n
  JOIN entity_episodes ep ON ep.entity_node_id = n.neighbor_id
  JOIN records r          ON r.record_id = ep.episode_id
 WHERE r.tombstoned = 0
   AND r.active = 1
   -- Latest-record/supersession predicate, same as do_search_keyword.
   -- Without it, a superseded neighbor (kept in records for history)
   -- could re-enter solely via the graph leg even though keyword and
   -- semantic legs would never return it.
   AND <supersession predicate ON r>
   AND <auth_scope predicate ON r>      -- auth, same shared helper as keyword
   AND <visibility predicate ON r>      -- auth, same shared helper as keyword
   AND <filter predicate ON r>          -- user narrowing, neighbor only
   AND r.record_id NOT IN (SELECT id FROM ranked_query_records)
   --                              ^^^^^^^^^^^^^^^^^^^^^^^^
   -- Dedup ONLY against records that actually enter RRF fusion (top 50
   -- of each lexical leg). Records overfetched as graph SEEDS but
   -- ranked outside the fusion pool (51-200) remain eligible to
   -- re-enter via graph scoring — otherwise broad queries silently
   -- drop strongly-connected hits.
 GROUP BY r.record_id
 ORDER BY conf DESC, r.updated_at DESC
 LIMIT ?3;
```

`WITH RECURSIVE` keyword is used (per AC + brief §6.11) even though depth=1
needs no recursion today, so depth>1 follow-ups are syntactically additive.

**Trust boundary:** lexical matching happens upstream against `records_fts`
(already scope-filtered). Entity nodes are reached by id-join through
`entity_episodes`; their FTS index is **never queried**. A hidden alias
cannot influence seed selection.

**Visibility predicate:** assembled by the same shared helper that builds
the predicate for `do_search_keyword` and `do_search_semantic`, so the three
legs cannot drift. Drift would be a §4 contract violation.

**Implementation note (carray is required):** `rusqlite`'s `carray`
extension is mandatory for this feature. Three list-bound query shapes
need it: `visible_query_records` (≤400 ids), `ranked_query_records`
(≤100 ids), and the hydration `IN carray(?1)` clause (≤50 ids).

The capability probe in §8 includes a `carray` availability check
(execute `SELECT 1 FROM carray(?)` once at `MemoryStore::open`). If
`carray` is unavailable, `graph_search` reports false at startup and
the hybrid path emits `DegradedLeg::Graph { reason:
"capability_unavailable" }` per request. **There is no inline-VALUES
fallback path.** The rollout signal is explicit (capability + metric +
response field), and operators see the degraded state in metrics and
response payload. `carray` ships with `rusqlite` under default features
and is enabled in `Cargo.toml` for all production builds; the only
realistic path to disablement is a hand-rolled embed without the
feature flag, which the probe surfaces on first connect rather than at
query time.

### 5.2 New module

`crates/cairn-store-sqlite/src/store/graph_search.rs` — async wrapper around
the SQL above using the `tokio_rusqlite` worker. Mirrors the shape of
`store/hybrid.rs` for consistency.

### 5.2.1 Graph-only candidate hydration

Keyword and semantic legs return full `SearchCandidate` rows (snippet,
`bm25`, `semantic_distance`, `record_json`, scope, visibility,
`recency_seconds`, `staleness_seconds`, `confidence`, `salience`). The
graph leg returns only `(record_id, edge_confidence_score)` so it must
hydrate before the result page is assembled.

**Hydration step:** the graph-only path goes through the **same shared
row-mapper** as `do_search_keyword`, which already handles the
millisecond-timestamp arithmetic correctly (see
`crates/cairn-store-sqlite/src/store/search.rs::row_mapper` —
`recency_seconds` and `staleness_seconds` are computed from raw
`now_ms` minus the column's ms value, divided by 1000 in Rust). No
new SQL recomputes timestamps in the hydration query.

**Rank preservation:** the graph SQL (§5.1) returns rows in a
deterministic order (`ORDER BY conf DESC, r.updated_at DESC`). That
order IS the graph leg's rank input to RRF and must be preserved
through hydration. SQLite returns `IN carray(...)` results in arbitrary
order, so the hydration step CANNOT rely on the SQL row order. The
implementation:

1. The graph SQL output is captured as `Vec<(record_id, conf,
   graph_rank: usize)>` where `graph_rank` is the row index
   (1-based) in the original SQL result.
2. The hydration query fetches by id (any order); the result is
   re-sorted by `graph_rank` in Rust before merging into
   `HybridSearchInputs.graph`.
3. The merged `Vec<GraphCandidate>` carries `graph_rank` explicitly
   on each entry (added to the struct).
4. `rrf_fusion_weighted` reads the explicit `graph_rank` rather than
   inferring rank from list position.

Test §10.3 includes a determinism check: identical vault + identical
query produces byte-identical `graph_rank`/RRF output across N
repeated calls.

The hydration SELECT is:

```sql
SELECT r.record_id, r.target_id, r.scope, r.kind, r.class, r.visibility,
       r.body AS record_json,
       r.updated_at, r.created_at      -- raw ms; row-mapper converts
  FROM records r
 WHERE r.record_id IN carray(?1)            -- graph-only ids (≤ 50)
   AND r.tombstoned = 0
   AND r.active = 1
   -- Defense-in-depth: re-apply the FULL authorization predicate set
   -- the graph-leg SQL applied at step 4 (supersession + auth_scope +
   -- visibility + filter), not a subset. If graph ids ever come from
   -- a cache or a buggy upstream, hydration must independently refuse
   -- superseded or cross-scope records.
   AND <supersession predicate ON r>
   AND <auth_scope predicate ON r>
   AND <visibility predicate ON r>
   AND <filter predicate ON r>;
```

Hydration is implemented via the **same shared row-mapper** as
`do_search_keyword`, taking the same `(auth_scope, visibility_allowlist,
filter)` triple. The triple is constructed once per `do_search_hybrid`
call and passed to all three downstream queries (keyword leg, graph SQL
step 4, graph hydration), so the four predicates cannot drift.

(`confidence` / `salience` are not columns on `records` in the current
schema — they live on the deserialized `MemoryRecord` body. The
`SearchCandidate.confidence` / `SearchCandidate.salience` fields the
keyword leg returns are derived in the row-mapping layer from the same
body. The graph-only hydration uses the same row-mapping helper as
`do_search_keyword`, not a hand-rolled SELECT — single source of truth.)

Output `SearchCandidate` field shape for graph-only rows:

| Field | Value |
|---|---|
| `bm25`              | `0.0` (sentinel; absent from FTS results) |
| `semantic_distance` | `None` |
| `snippet`           | empty string `""` (no FTS5 `snippet()` source) |
| `confidence`/`salience`/`recency_seconds`/`staleness_seconds` | derived from `records` body via the shared row-mapper |
| `record_json`       | hydrated `body` column |

`bm25=0.0` is a **sentinel** the explain serializer maps to `null` in the
`--json` output so callers don't mistake it for a strong BM25 hit.
`ScoreExplain` gains `graph_rank: Option<usize>` and
`edge_confidence_score: Option<f32>` so the graph contribution is visible
to debugging clients.

**Re-applying auth at hydration time:** the WHERE clause re-applies the
visibility and filter predicates even though the graph SQL already did.
This is defense in depth — guards against a future bug where the graph
SQL is bypassed (e.g., a cache layer); cost is one trivially indexed
predicate evaluation per id.

`hydrate_candidates` in `store/hybrid.rs` is extended to merge three
sources (keyword, semantic, graph-only-hydrated). Records present in
multiple sources keep the keyword/semantic row (richer signal); graph-
only ids contribute their hydrated row.

### 5.3 Hybrid orchestrator extension

`store/hybrid.rs::do_search_hybrid`:

- The graph leg depends on **both** the keyword AND semantic leg outputs
  (their UNIONed record-id list seeds the graph leg), so the three legs
  cannot all run in parallel. New shape:
  1. **Two retrieval purposes, two query shapes per leg:**
     - **Filtered fusion retrieval** — runs identically to today's
       `do_search_keyword` / `do_search_semantic`: applies
       `auth_scope` + visibility + user `filter`, returns top-50
       fully-hydrated rows per leg. Feeds the RRF fusion pool. This
       preserves "best filtered matches" semantics: a narrow filter
       cannot be starved by unfiltered out-of-filter records flooding
       the top-N cutoff.
     - **Auth-only seed retrieval** — runs in parallel: applies
       `auth_scope` + visibility + tombstone/active checks **AND the
       same supersession/latest-record predicate as
       `do_search_keyword`** (a record is a valid seed only if it is
       the latest active version — superseded retired versions are
       excluded). It does NOT apply the user-narrowing `filter`. The
       projection is `record_id` only. Returns up to
       `GRAPH_SEED_OVERFETCH = 200` ids per leg. Feeds the graph
       seed pool (∪ to ≤ 400 unique ids). The auth-only path is what
       makes the graph leg's §4.3 contract hold: provenance records
       that pass auth but fail the user filter can still seed graph
       expansion of in-filter neighbors. Reusing the supersession
       predicate prevents stale retired records from driving graph
       expansion through a path the keyword leg already excludes.
     Yes — this means BM25 and ANN run TWICE per hybrid request: once
     filtered (for fusion), once auth-only (for graph seeds). The
     auth-only path is id-only (no hydration), so the dominant cost
     is the index traversal. For BM25 the marginal cost over today's
     filtered run is ~one extra index scan; for vec0 the cost is one
     extra ANN traversal at limit=200, which the §10.5 bench gates
     against the 100ms p99 budget. If the bench shows the duplicated
     ANN traversal is the latency limiter, `GRAPH_SEED_OVERFETCH` is
     lowered (the latency gate is non-negotiable). Two queries are
     the cost of the §4.3 + filtered-recall guarantees together; one
     retrieval cannot satisfy both.
  2. **Structured fan-out via `tokio::task::JoinSet`**, NOT
     `try_join!`. `try_join!` short-circuits on the first error and
     drops sibling futures, which would orphan their SQLite work on
     the pool — defeating the safety valve. The orchestrator instead:
     - Spawns each leg as a task on a `JoinSet`. Every leg owns its
       own pool connection (§5.3 pool init contract); cancellation is
       initiated by signaling the leg's `CancellationToken` (which
       drives `interrupt()` + `progress_handler` per §5.3).
     - Awaits ALL four tasks (`while let Some(...) = set.join_next()`)
       — early errors do NOT cause sibling drop. Each leg gets a
       chance to either complete or run its cancellation path
       cleanly before the request returns.
     - Aggregates per-leg outcomes into `degraded_legs` per the
       timeout table below. The first error from a non-degradable
       leg (e.g., `filtered_keyword` timeout — see table) is held
       until all four tasks finish, then surfaced.
     Drop-triggered cleanup is not relied on. Cancellation is
     explicit, signaled via tokens, and verified by §10.5(c)
     (sibling-completion test).
     Inputs to the graph leg: `seed_record_ids` from the auth-only
     union and `ranked_record_ids` from the filtered top-50s, both
     populated only from legs that completed without timeout.
  3. The id-only overfetch is a `SearchConfig` knob
     (`graph_seed_overfetch`, default 200, max 500).
  3a. **Runtime safety valve.** Two layers of protection against the
      double-traversal cost spiking under production load:
      - **Pool initialization, sizing, and checkout lifecycle.**
        - **Init contract.** The `HybridConnectionPool`'s connections
          are NOT a separate, lazily-initialized set — they go through
          the **same `open_connection` factory** as the store's
          primary connection. That factory is the single source of
          truth for: `carray` extension load, `sqlite_vec` load,
          progress-handler registration, busy-timeout, journal mode,
          and any other per-connection setup. `MemoryStore::open`
          does NOT report `caps.graph_search = true` until **every**
          pool connection has been built via that factory AND each
          one has passed the §8 capability probes individually.
          Test §10.2's "pool-init failure" case injects a load
          failure on one pool connection and asserts the whole
          feature reports unavailable.
        - **Sizing and what the pool serves.** The cancellable pool
          serves **all four hybrid legs** that face a deadline:
          `filtered_keyword`, `filtered_semantic`, `auth_keyword_ids`,
          `auth_semantic_ids`, plus the graph-leg query. Because the
          deadline table makes `filtered_keyword` non-degradable
          (must complete or fail the request), it MUST run on a
          cancellable connection too — otherwise a slow filtered
          query orphans the underlying SQLite work and starves
          later requests despite the deadline expiring. Migrating
          the existing keyword/semantic legs onto the pool is part
          of this issue; their behavior is unchanged from the
          caller's perspective.

          **Two pool tiers** to keep the mandatory keyword recall
          floor isolated from graph-pool contention:
          - **Tier A (mandatory)**: dedicated connections for
            `filtered_keyword` and `filtered_semantic`. Size =
            `num_cpus` (default), configurable as
            `SearchConfig::filtered_pool_size`. Acquisition has a
            **request-level deadline** (`SearchConfig::
            filtered_acquire_timeout_ms`, default 200ms — 2× the
            p99 budget). On timeout the request fails with
            `StoreError::Timeout` and a verb-level error reason of
            `tier_a_acquire_timeout` so operators can distinguish
            queue saturation from in-query slowness. The acquire
            deadline is independent of and additive to the per-leg
            execution deadline — together they cap total request
            wall time. Filtered legs cannot be starved by graph
            traffic because graph never queues on Tier A; they CAN
            be starved by other filtered traffic, which is why the
            acquire deadline exists.
          - **Tier B (graph)**: dedicated connections for
            `auth_keyword_ids`, `auth_semantic_ids`, and
            `search_graph_neighbors`. Size = `2 * num_cpus`
            (default), configurable as
            `SearchConfig::graph_pool_size`. Acquire has a
            `pool_acquire_timeout_ms` deadline (default = leg
            deadline); on timeout the leg degrades to
            `DegradedLeg::Graph { reason: DeadlineExceeded, source:
            ... }`. Tier B saturation cannot fail the mandatory
            keyword leg.

          Both tiers honor the same pool-init contract and per-
          checkout cancellation lifecycle below. The keyword/
          semantic existing `tokio_rusqlite` worker is retired in
          favor of Tier A — single execution model across all
          hybrid legs.
        - **Per-checkout cancellation lifecycle.** Connection-scoped
          cancellation (`interrupt()`, `progress_handler`) requires
          explicit reset before reuse. The lifecycle:
          1. **Acquire**: borrow a connection; install a fresh
             `CancellationToken` and re-arm `progress_handler` against
             it. Run a `SELECT 1` smoke probe; if it fails (e.g.,
             SQLITE_INTERRUPT pending from a stale state), discard
             the connection and rebuild via the factory.
          2. **Run**: leg's SQL executes; deadline watchdog can fire
             `interrupt()` + token cancel.
          3. **Release**: ALWAYS reset on release, regardless of
             outcome — clear the token, deregister the leg's
             progress callback, run `sqlite3_db_release_memory`
             (no-op if SQLite ignored it). If reset fails, **discard
             the connection and rebuild** via the factory before the
             slot is returned to the pool. Interrupted connections
             are NEVER reused without a verified reset.
        Test §10.5 includes a "cross-request reuse" case: one
        request times out, the next request gets the same pool slot
        and runs successfully — proves the reset/discard lifecycle.

      - **Per-leg deadline with real, statement-scoped cancellation.**
        Each of the four parallel queries runs with a deadline budget
        (`SearchConfig::graph_leg_deadline_ms`, default 60ms — 60% of
        the p99 budget). A bare `tokio::time::timeout` only drops the
        awaiter, leaving the SQLite worker pinned on the orphaned
        job and starving later requests; that is forbidden. SQLite's
        `interrupt()` and `progress_handler` are connection-scoped,
        not statement-scoped, so a naive use would also abort sibling
        queries running on the same connection. To avoid both pitfalls
        the implementation uses **per-leg dedicated connections from a
        bounded cancellable pool**:
        - The store maintains a small pool of read-only SQLite
          connections (`HybridConnectionPool`, size = 4 by default —
          one per parallel leg of a hybrid request, plus a small
          margin). Each cancellable leg of a hybrid request acquires
          its own connection from the pool for the duration of the
          query and releases it on completion or cancellation.
        - When the deadline elapses, a watchdog task calls
          `interrupt()` ONLY on that leg's connection. Sibling legs
          on their own connections are unaffected.
        - `progress_handler` is registered per-connection with a
          callback gated by that connection's `CancellationToken`,
          fired only by its own watchdog. Belt-and-braces: even if
          `interrupt()` races with statement reset, the progress
          handler catches the cancel on the next opcode batch
          (default opcode interval 1024).
        - The pool sits behind a semaphore: if all connections are
          busy, the next leg waits. The semaphore + per-leg
          connection together guarantee a timed-out leg cannot leak
          work onto an unrelated request's connection.
        Together this guarantees per-leg isolation AND that the
        SQLite work stops within milliseconds of the deadline.

        **Closed `DegradationReason` enum** — degradation reasons are
        a typed enum, not free-form strings, so callers and tests key
        on stable variants:

        ```rust
        #[non_exhaustive]
        pub enum DegradationReason {
            CapabilityUnavailable,    // graph_search=false at startup
            DeadlineExceeded,         // any leg timed out
            SqlError,                 // SQLite returned an error
            WorkerPanic,              // tokio_rusqlite worker panicked
        }
        pub enum DegradedLeg {
            Semantic { reason: DegradationReason },
            Graph    { reason: DegradationReason, source: GraphSource },
        }
        #[non_exhaustive]
        pub enum GraphSource {
            All,                      // entire graph leg failed
            AuthKeywordSeed,          // auth-only keyword seed query
            AuthSemanticSeed,         // auth-only semantic seed query
        }
        ```

        Multiple `DegradedLeg` entries can appear in one response (one
        per affected source). The `source` field on `Graph` makes the
        seed-vs-traversal distinction explicit so operators can tell
        which input failed.

        **Per-leg timeout semantics** — distinct behavior for each of
        the four parallel legs:

        | Leg | Timeout result | `DegradedLeg` entry |
        |---|---|---|
        | `filtered_keyword` (top-50, hydrated) | **Request fails** with `StoreError::Timeout`. Keyword leg is the existing recall floor; degrading it would silently regress 2-leg search behavior. | none — error path |
        | `filtered_semantic` (top-50, hydrated) | Skip the leg, run hybrid with keyword + graph only. Existing fail-open behavior for semantic, preserved. | `Semantic { reason: DeadlineExceeded }` |
        | `auth_keyword_ids` (top-200, ids only) | Drop this seed source from the union; graph runs with semantic seeds only. | `Graph { reason: DeadlineExceeded, source: AuthKeywordSeed }` |
        | `auth_semantic_ids` (top-200, ids only) | Drop this seed source from the union; graph runs with keyword seeds only. | `Graph { reason: DeadlineExceeded, source: AuthSemanticSeed }` |

        If both auth-only seed queries exceed the deadline the graph
        leg skips entirely (`Graph { reason: DeadlineExceeded, source:
        All }`).
        If both `filtered_*` legs would fail simultaneously, the
        keyword leg's failure preempts the semantic-skip — the
        request still errors, no half-fused result is returned.

        Test §10.5's deadline-circuit gate verifies (a) the request
        returns within deadline + 10ms, (b) the worker pool can
        accept a new request immediately after the deadline fires,
        (c) a sibling leg on a different connection completes
        normally when one leg is interrupted (proves per-leg
        isolation), AND (d) each leg's specific timeout-recovery
        path is tested against the table above.
        Test §10.5's deadline-circuit gate verifies (a) the request
        returns within deadline + 10ms, (b) the worker pool can
        accept a new request immediately after the deadline fires,
        AND (c) a sibling leg on a different connection completes
        normally when one leg is interrupted (proves per-leg
        isolation).
      - **Adaptive overfetch reduction.** A rolling p99 latency
        estimate per leg (EWMA over the last 1000 requests) feeds
        back into `effective_overfetch`: if observed p99 exceeds 80%
        of the gate, the runtime halves the overfetch limit for new
        requests until p99 recovers. Bounded between
        `[HYBRID_LEG_LIMIT, graph_seed_overfetch]`. Logged at
        `info!` whenever the runtime adjusts so operators see the
        adaptation in metrics.
      Both layers are tested under load in §10.5: a synthetic vault
      that pushes the auth-only ANN past the deadline must produce a
      `degraded_legs: [Graph { reason: DeadlineExceeded, source: ... }]` result
      rather than a slow request that breaches the gate.
  4. Graph leg inputs (built once the four parallel retrievals
     complete): `seed_record_ids = unique(auth_keyword_ids ∪
     auth_semantic_ids)` (≤ 400, auth-only no filter),
     `ranked_record_ids = unique(filtered_keyword_top50 ∪
     filtered_semantic_top50)` (≤ 100, filter applied). The graph leg
     itself is fast (id joins, no FTS or vector math).
  5. The bench in §10.5 includes a "broad query" case (>200 keyword
     hits, mixed-confidence neighbors) to validate the 200-default
     against the latency gate.
- Graph leg failures degrade to an empty list (FTS5 zero-match seeds → empty
  is the normal case), **but never silently**:
  1. The failure is logged at `warn!` with the underlying `StoreError` and a
     `verb=search_hybrid leg=graph` tag.
  2. A counter `cairn_search_graph_leg_failures_total{reason=...}` is
     incremented for **every** branch where the graph leg does not run
     to completion — runtime SQL error (`reason="sql_error"`), worker
     panic (`reason="worker_panic"`), AND capability-based disablement
     (`reason="capability_unavailable"`). The capability-unavailable
     branch is the dominant rollout-failure case (schema skew →
     `graph_search=false` at startup → silent 2-leg search), so it MUST
     be counted alongside runtime failures. Zero-match-seeds is *not* a
     failure and does not increment. Operators alert on rate of
     `reason!=zero_seeds` to catch both runtime and rollout regressions.
     A startup-time gauge `cairn_search_graph_leg_available` (0/1 per
     instance) lets dashboards distinguish "always disabled on this
     node" from "intermittent runtime failure".
  3. The `HybridSearchPage` carries a new `degraded_legs:
     Vec<DegradedLeg>` field that callers (CLI `--json`, MCP, SDK)
     surface to the user. The wire shape is the typed enum defined in
     §5.3 (`DegradationReason` + `GraphSource`). An empty vec means
     full-fidelity hybrid. This is the canonical shape across
     `HybridSearchPage`, `SearchOutcome`, IDL, CLI JSON, and SDK
     transport — there is no string-keyed alternative.
  4. The CLI prints a `warning:` line on stderr when `degraded_legs` is
     non-empty so an interactive user notices a partial result; `--json`
     emits the field unconditionally.
- Capability: if `caps.graph_search` is false the leg is skipped and recorded
  as `DegradedLeg::Graph { reason: CapabilityUnavailable, source: All }`. Same model
  as today's semantic leg, but now visible to the caller.

Rollback signal: clients can assert `degraded_legs.is_empty()` in
post-deploy smoke tests. Operators can alert on the counter to catch
schema-skew regressions that the legacy 2-leg path would have hidden.

## 6. Pure orchestrator

`cairn-core::search::orchestrator::hybrid_search` is extended:

```rust
let bm25_list = inputs.keyword.clone();   // rank inferred from list position (today)
let sem_list  = inputs.semantic.clone();  // rank inferred from list position (today)

// Graph leg: rank carried explicitly on each candidate.
// `RankedCandidate` is the shape `rrf_fusion_weighted` consumes for legs
// where rank is stored, not inferred.
let graph_list: Vec<RankedCandidate> = inputs.graph
    .iter()
    .map(|g| RankedCandidate {
        record_id: g.record_id.clone(),
        rank: g.graph_rank,
        weight: g.edge_confidence_score,    // for the confidence penalty
    })
    .collect();

let fused = rrf_fusion_weighted(
    &[
        Leg::ListPosition(bm25_list),                              // rank from index
        Leg::ListPosition(sem_list),                               // rank from index
        Leg::Explicit(graph_list, params.confidence_floor),        // rank from field
    ],
    params.rrf_k,
);
```

`rrf_fusion_weighted` is a new pure helper accepting two leg shapes:
`Leg::ListPosition(Vec<ScoredCandidate>)` reads rank from the slice
index (today's behavior); `Leg::Explicit(Vec<RankedCandidate>, floor)`
reads rank from the per-candidate field and applies the confidence
penalty `effective_rank = rank / max(weight, floor)`. The existing
`rrf_fusion` keeps its signature and gains a 1-line shim:
`rrf_fusion(lists, k) = rrf_fusion_weighted(list_position_legs, k)`.

### 6.1 Cosine rerank — graph-only exemption

A graph-only candidate (a record that the keyword and semantic legs both
missed) by construction has weak query-vs-doc cosine similarity — that is
exactly the discovery case the leg exists to surface. The unmodified
cosine pass would push such candidates back out of the final page,
defeating the leg's purpose.

`cosine_rerank` is updated to operate on a tagged input list:

```rust
pub enum CandidateOrigin { Lexical, GraphOnly }   // graph-only ⇔ in graph leg AND in NEITHER bm25 nor semantic
pub struct OriginTaggedCandidate { pub inner: RrfCandidate, pub origin: CandidateOrigin }
```

Behavior — graph-only candidates skip the cosine term entirely; the
blend is renormalized so they compete on RRF alone with no synthetic
similarity bonus:

Both classes use the same `blend * rrf_norm` factor; only the cosine
addend differs:

- `Lexical` candidates: `final = blend * rrf_norm + (1-blend) *
  cosine_norm` where `cosine_norm = (cosine_raw + 1) / 2 ∈ [0, 1]`
  (normalize to non-negative; otherwise negative raw cosine would let
  graph-only beat by asymmetry alone).
- `GraphOnly` candidates: `final = blend * rrf_norm` — same RRF weight
  as lexical, NO cosine addend. The cosine term is omitted, not
  substituted with a neutral value, but the `blend` factor still
  applies to RRF so graph-only and lexical share the same RRF scale.

Why this formulation is symmetric:

- Equal RRF, lexical `cosine_norm = 0` (worst): lexical gets `blend *
  rrf + 0 = blend * rrf`, graph-only gets `blend * rrf`. **Tie**.
- Equal RRF, lexical `cosine_norm = 0.5` (neutral): lexical gets
  `blend * rrf + (1-blend) * 0.5`, graph-only gets `blend * rrf`.
  Lexical wins by `0.15` (default blend).
- Equal RRF, lexical `cosine_norm = 1` (perfect): lexical gets
  `blend * rrf + (1-blend) * 1`, graph-only gets `blend * rrf`.
  Lexical wins by `0.30` (default blend).
- Strictly stronger RRF on graph-only side: graph-only beats lexical
  only when its RRF strength × `blend` exceeds lexical's full term —
  earned, not bias.

A graph-only candidate cannot beat a lexical candidate at equal RRF
unless the lexical cosine term is exactly zero (worst case). It
cannot promote past a lexical candidate with any positive cosine.

Side effect on score scale: `final` for graph-only is in `[0, blend]`
(at most `blend * 1 = blend`); for lexical it's `[0, 1]`. Lexical can
exceed graph-only's max only via the cosine addend — exactly the
intended semantic.

End-to-end tests in §10.3 assert: (a) neighbor-only record survives
the rerank pass on RRF strength; (b) at equal RRF with `cosine_norm =
0`, lexical and graph-only tie (no asymmetric bias); (c) at equal RRF
with `cosine_norm > 0`, lexical strictly outranks graph-only.

### 5.4 Verb-envelope propagation

`degraded_legs` is meaningless if the verb dispatcher discards it on the
way to CLI/MCP/SDK. The verb-level outcome type
(`cairn_core::verbs::search::SearchOutcome`) is extended with a new
field:

```rust
#[non_exhaustive]
pub struct SearchOutcome {
    pub candidates: Vec<SearchCandidate>,
    pub explain: Option<Vec<ScoreExplain>>,
    pub degraded_legs: Vec<DegradedLeg>,    // NEW — empty for non-hybrid modes
}
```

- The dispatcher copies `degraded_legs` from `HybridSearchPage` into
  `SearchOutcome` unmodified. Keyword and semantic-only modes return
  an empty vec.
- `cairn-cli`'s `--json` emits `degraded_legs` as a top-level array.
  When the field is non-empty AND output is to a TTY, the CLI prints
  a one-line `warning: graph leg degraded (reason: ...)` to stderr —
  same UX a user would expect from any other partial-result signal.
- MCP wrapper exposes `degraded_legs` in the `search` response payload
  via the IDL update (§12.1). Schemars derive picks it up
  automatically from the typed struct.
- **`cairn-sdk` transport** — the SDK currently maps `SearchOutcome`
  → generated `SearchData` by hand in `crates/cairn-sdk/src/transport.rs`.
  This issue extends the generated `SearchData` IDL with
  `degraded_legs: Vec<DegradedLeg>` (`#[serde(default)]` so legacy
  payloads keep decoding) and updates the transport mapper to copy the
  field through. Without this update, SDK callers silently lose the
  signal — the spec's "never silent" claim becomes false on one of
  the three supported surfaces. Codegen run in the same PR; the
  generated diff is committed.
- **Tests** in §10.3:
  - Dispatcher: trigger graph-leg `CapabilityUnavailable` at the
    store, run the `search` verb end-to-end, assert
    `outcome.degraded_legs` contains one entry AND the `--json`
    output contains the field.
  - SDK round-trip: same trigger, but invoke through the SDK
    transport. Assert the deserialized `SearchData.degraded_legs`
    survives end-to-end. A regression here means the propagation
    can break silently in a future refactor.

## 7. Verb + CLI

- `cairn-core::verbs::search` — read `confidence_min` from `SearchConfig`,
  thread into `HybridSearchArgs`. CLI flag overrides config.
- `cairn-cli::verbs::search` — adds `--confidence-min <f32>` clap arg
  (`ValueHint::Other`, validated to `[0.0, 1.0]`). Exposed only when mode is
  `hybrid`.
- `SearchConfig.confidence_min: f32` defaults to `0.5`.
- `cairn-idl` — extend the search-args IDL definition; rerun
  `cairn-codegen`. (Generated diff committed in same PR.)

## 8. Capability surface

Graph-leg availability is a **runtime** capability (depends on schema
state and the loaded `carray` extension at connection time), so it
belongs on `MemoryStoreCapabilities`, not the config-derived
`CapabilitySet`. The two surfaces serve different purposes:

- `config::CapabilitySet` — capabilities derivable from static config
  (e.g., embedder configured, vector enabled). Computed before the
  store opens.
- `contract::MemoryStoreCapabilities` — capabilities the store
  advertises after opening, based on actual runtime state (schema
  version, loaded extensions, migration set).

This issue extends `MemoryStoreCapabilities` with
`graph_search: bool` (NOT `CapabilitySet`). Probes run inside
`MemoryStore::open` after migrations apply, populate the runtime
capability struct, and the orchestrator reads that struct when deciding
whether to dispatch the graph leg.

- `MemoryStoreCapabilities::graph_search: bool` — true only when
  **every** schema object that the graph SQL in §5.1 touches is present
  at the expected migration level. The probe MUST mirror the runtime
  query exactly — extra requirements cause false-negative capability
  results, missing requirements cause silent runtime failures. Detection
  probes (single transaction, executed once at `MemoryStore::open`):

  1. Migration version meets the floor pinned for this feature
     (`MIGRATION_FLOOR_GRAPH_SEARCH = 0044` — the entity_episodes migration,
     the latest object referenced by §5.1).
  2. `entity_episodes` exists with columns `episode_id` (FK →
     `records.record_id`) and `entity_node_id`. Note: the column is
     `episode_id`, **not** `record_id` — see migration 0044.
  3. `entity_edges` exists with columns `source_id`, `target_id`,
     `source_record_id`, `confidence_score`, `invalid_at`,
     `expired_at`, **`valid_at`, and `created_at`** (the graph SQL
     filters on both bitemporal start-bounds AND end-bounds — omitting
     any of the four lets schema skew slip past startup and surface
     as a runtime error). Every column the §5.1 SQL references on
     `entity_edges` MUST appear in this list.
  4. `records` exists with columns `record_id` (PK), `tombstoned`
     (INTEGER 0/1), `active` (INTEGER 0/1), `updated_at`, `created_at`,
     plus whatever columns the shared visibility/filter helper requires
     (today: `target_id`, `scope`, `kind`, `class`, `visibility`, `body`).
  5. `carray` extension is loadable: `SELECT 1 FROM carray(?)` succeeds
     with a bound array. Required for the seed-list, ranked-list, and
     hydration queries — see §5.1 implementation note.

  Note: `entity_nodes_fts` is **not** required. The graph SQL reaches
  entity nodes by id-join through `entity_episodes` only and never
  queries the entity FTS index — requiring it would disable the feature
  in valid schemas.

  Failing **any** probe yields `graph_search = false`. The probe results
  are logged once at `info!` with structured fields so a partial schema is
  surfaced rather than silently disabling the leg.

- `cairn.mcp.v1.search.hybrid` capability identifier unchanged. When the
  graph leg is unavailable, the `HybridSearchPage::degraded_legs` field
  (§5.3) advertises the degradation explicitly — clients are not left to
  infer it from result quality.

## 9. Errors

- `StoreError::CapabilityUnavailable { what: "graph_search" }` for explicit
  graph-only callers (`search_graph_neighbors` direct).
- Hybrid path never raises `CapabilityUnavailable` for the graph leg —
  degrades to empty list (parity with semantic leg behavior today).

## 10. Tests

### 10.1 `cairn-core` unit

- `rrf_fusion_weighted` — known rank lists w/ confidence weights → expected
  merged order.
- `hybrid_search` — `INFERRED` (conf=0.6) edge-only neighbor ranks below
  `EXTRACTED` (conf=1.0) edge-only neighbor at same input-list rank.
- `confidence_floor` floor prevents division-by-zero w/ malformed conf=0.

### 10.2 `cairn-store-sqlite` integration (real SQLite)

- Seeded vault: 3 records, 2 entity nodes, 3 edges of mixed confidence.
- Query that hits FTS5 seeds → returns expected graph candidates.
- Edges with `confidence_score < confidence_min` excluded.
- N+1 verifier: prepared-statement count ≤ 3 (seeds, edges, records — all in
  one prepared CTE).
- Empty seeds → empty list, no error.
- **Authorization-boundary tests** (covers finding §5.1):
  - Hidden-only alias: entity node X has surface form "ACME" indexed only
    from a hidden record. The query "ACME" matches no visible record →
    keyword leg empty → graph leg empty → no leak.
  - Hidden record references neighbor entity Y. A visible record also
    references Y (and matches the query). The hidden→Y edge must not
    contribute confidence to the visible neighbor: tested by asserting the
    `MAX(confidence_score)` aggregate equals the **visible** edge's
    confidence, not the hidden edge's.
  - Expired-edge regression: edge from seed→Y has `expired_at` set
    (ingestion-time tombstone). Y must NOT appear in `neighbors`. Same
    test pattern for `invalid_at` set.
  - Future-dated edge: edge with `valid_at > now_event`. Y must NOT
    appear in `neighbors` — the start bound is enforced. Same pattern
    for `created_at > now_ingest`. Without §5.1's start-bound
    predicates, future facts would leak into search.
- **Discovery-property test** (covers finding on missing 1-hop):
  - Vault: record A mentions entity X (matches query); edge X→Y; record B
    mentions Y but does NOT contain the query terms. Hybrid search must
    return B in `graph_hits`. Assert B is absent from `keyword_hits` and
    present in `graph_hits` — proves the leg surfaces neighbor-only records.
- **Capability-skew tests** (covers finding §8):
  - Drop one column from `entity_episodes` → `graph_search` reports false,
    hybrid path emits `DegradedLeg::Graph { reason: CapabilityUnavailable, source: All }`.
  - Drop `entity_edges.expired_at` only → `graph_search` reports false at
    startup (regression test for the missing-bitemporal-bound bug — without
    this column the runtime SQL would fail).
  - Vault has graph tables but no `entity_nodes_fts` → `graph_search`
    reports true (probe must not require the FTS index, since the SQL
    never queries it).
  - Inject a SQL error into the graph leg at runtime → counter increments,
    `degraded_legs` populated, hybrid still returns keyword + semantic.
- **SQL-correctness regression test** (covers self-exclusion finding):
  - Two seed sets — one containing the rowid-1 entity, one not. In both,
    a seed-to-seed edge must NOT cause a seed to appear as its own
    "neighbor" hit. Asserts the explicit `neighbor_id NOT IN seeds` clause
    rather than relying on `HAVING` ordinal references.
- **Hydration auth re-check** (covers hydration finding):
  - Inject a graph-leg result containing a record id outside the
    caller's `auth_scope` (simulates a buggy upstream / cache replay).
    Hydration MUST drop that id — assert it does not appear in the
    final `SearchCandidate` list. Documents the defense-in-depth
    contract.
- **carray-required capability test**:
  - Build a store with `carray` disabled at the `rusqlite` feature
    level. `MemoryStore::open` must report `caps.graph_search = false`
    via the §8 probe. Hybrid search emits `DegradedLeg::Graph { reason:
    "capability_unavailable" }`; keyword + semantic still return.
- **Stale-seed exclusion** (covers supersession finding):
  - Vault: record A v1 mentions entity X; A v2 supersedes v1 and
    does not mention X. Edge X → Y exists. Query matches X only via
    A v1 (which is now superseded). The auth-only seed retrieval must
    NOT include A v1 — supersession predicate matches keyword leg.
    Y must NOT appear in `graph_hits`. Without this guard, retired
    records would silently drive graph expansion.
- **Stale-neighbor exclusion** (covers graph step-4 supersession):
  - Vault: record N v1 mentions neighbor entity Y; N v2 supersedes
    v1, also mentions Y. Edge X → Y exists; query seeds X normally.
    The graph leg must surface ONLY N v2, not N v1. Without
    supersession on step 4 / hydration, the retired N v1 could
    re-enter via graph alone.
- **Orphan-edge exclusion** (covers null-provenance finding):
  - Vault: edge X → Y with `source_record_id` set, then provenance
    record deleted (so `source_record_id` becomes NULL via
    `ON DELETE SET NULL`). The graph leg MUST NOT surface Y at any
    confidence tier — without visible provenance, `auth_scope` and
    visibility cannot be evaluated, so the edge is ineligible.
    Provenance deletion is a recall trade-off the auth model requires.
- **Rank-rescue via graph** (covers seed-vs-dedup-pool finding):
  - Broad query: keyword leg returns 200 over-fetched ids; only top 50
    enter RRF fusion. Pick a record D ranked at position 75 in keyword
    (over-fetched, not RRF-fused). Add an edge from a top-10 entity to
    D's entity. The graph leg MUST return D — D is in the seed pool
    but NOT in `ranked_query_records`, so the dedup at step 4 lets D
    re-enter via graph evidence. Final page must include D.

### 10.3 Verb-level snapshot + end-to-end (`insta`)

- `--json` output of hybrid search w/ a graph hit — stable shape (adds
  `graph_rank` and `edge_confidence_score` to `ScoreExplain`, with
  `bm25=null` for graph-only hits).
- **Graph-only hydration**: end-to-end test — record reachable only via
  graph leg returns a fully populated `SearchCandidate` (snippet may be
  empty; record_json, scope, visibility, kind, class must be set). Without
  the §5.2.1 hydration step this test fails.
- **Graph-only survives rerank**: end-to-end test using a real embedder —
  vault contains record A (matches query lexically), edge A's-entity →
  B's-entity, record B (no query terms, low cosine to query). With
  `blend=0.7` (default), record B must appear in the final page. Without
  the exemption from §6.1 this test fails.
- **User-filter scoping** (covers §4.3 split): vault has visible record P
  (provenance, `tag=internal`) and visible record N (neighbor,
  `tag=public`) joined by an edge. Both records are inside the caller's
  `auth_scope`. Hybrid search with `filter: tag=public` MUST return N
  via the graph leg — the user-narrowing filter applies only to the
  neighbor, not to provenance. Asserts the auth/filter split contract.
- **Cross-scope provenance rejection**: vault has provenance record P
  outside the caller's `auth_scope`, neighbor N inside. Even with no
  user filter, N must NOT be returned via the graph leg — `auth_scope`
  is enforced on provenance and the edge is dropped.
- **Semantic-only seed**: a paraphrased query that only the semantic leg
  matches, with a graph edge to a neighbor record. Graph leg must seed
  from the semantic hit and return the neighbor (proves §3 step 1's
  union-of-legs design).

### 10.4 Property tests

- `rrf_fusion_weighted` monotonicity: lower-confidence weight ⇒ smaller score
  contribution at identical rank.

### 10.5 Bench (#99) and load gates

- **Baseline gate:** p99 < 100ms on 10k-record vault; assert in
  `cairn-bench` with Criterion.
- **Broad-query gate:** synthesized query producing >200 keyword hits
  on a 50k-record vault; p99 < 100ms with both auth-only and filtered
  retrievals running.
- **Saturation gate:** synthesized concurrency (32 parallel requests)
  on a 50k-record vault; p99 < 150ms (slightly relaxed for tail
  latency under contention) AND adaptive overfetch reduction observed
  to engage at least once during the run.
- **Deadline-circuit gate:** synthetic delay injected into the
  auth-only semantic ANN traversal exceeding `graph_leg_deadline_ms`;
  result must report `degraded_legs: [Graph { reason: DeadlineExceeded,
  source: AuthSemanticSeed }]` AND total request latency must remain ≤
  deadline + 10ms overhead. Without this gate the safety-valve design
  is unverified.
- **Tier A acquire-timeout gate:** saturate Tier A by holding all
  `filtered_pool_size` connections; submit a new hybrid request and
  verify it fails with `StoreError::Timeout` and reason
  `tier_a_acquire_timeout` after `filtered_acquire_timeout_ms`,
  not by blocking indefinitely. Proves the bounded-acquisition
  guarantee.

## 11. Acceptance criteria mapping

| AC | Mapped section |
|---|---|
| `HybridSearchOrchestrator` pure function | §6 |
| RRF formula unit-tested | §10.1 |
| 1-hop expansion as single SQL query (no N+1) | §5.1, §10.2 |
| Semantic leg skipped cleanly | §4.3 (existing behavior preserved) |
| `SearchArgs --confidence-min <float>` | §7 |
| p99 < 100ms / 10k-record vault | §10.5 |
| `--json` snapshot stable | §10.3 |

## 12.1 Contract versioning

The current public structs (`KeywordSearchArgs`, `SemanticSearchArgs`,
`HybridSearchArgs`, `HybridSearchInputs`, `HybridSearchParams`,
`HybridSearchPage`, `ScoreExplain`, `MemoryStoreCapabilities`) are NOT
`#[non_exhaustive]` and are constructed via struct literals across the
in-tree workspace. Adding required fields to any of them breaks
struct-construction at compile time. There is no honest way to ship the
auth-uniformity guarantees of §4.3 (one `auth_scope` on every search
leg) as a non-breaking change — pretending otherwise would either
recreate the policy-drift problem or sneak source-breaking changes past
adapters under a compatibility claim.

So this is a **MAJOR** contract version event (`0.4.0` → `0.5.0`).
The bump is the explicit incompatibility signal; out-of-tree adapters
must recompile.

Changes by struct:

| Struct | Change | Field defaults |
|---|---|---|
| `KeywordSearchArgs`  | + `auth_scope: ScopeTuple` (required) | none — caller must supply |
| `SemanticSearchArgs` | + `auth_scope: ScopeTuple` (required) | none — caller must supply |
| `HybridSearchArgs`   | + `auth_scope: ScopeTuple`, + `confidence_min: f32` | none / 0.5 |
| `GraphNeighborsArgs` | new struct (§4.3) | n/a |
| `HybridSearchInputs` | + `graph: Vec<GraphCandidate>` | empty vec |
| `HybridSearchParams` | + `confidence_floor: f32` | 1e-3 |
| `HybridSearchPage`   | + `degraded_legs: Vec<DegradedLeg>` | empty vec |
| `ScoreExplain`       | + `graph_rank: Option<usize>`, `edge_confidence_score: Option<f32>` | None |
| `MemoryStoreCapabilities` | + `graph_search: bool` | false |
| `MemoryStore` (trait)| + required method `search_graph_neighbors` (no default impl — adapters must implement) | n/a |

All structs gain `#[non_exhaustive]` in this same PR so future MINOR
additions don't recreate this break. Because `#[non_exhaustive]` makes
struct literals illegal in external crates, every affected struct
ALSO gains a public `bon`-derived builder in the same PR (per CLAUDE.md
§6.10: "Builders via `bon` for anything with >3 optional fields").
Specifically:

- `KeywordSearchArgsBuilder`, `SemanticSearchArgsBuilder`,
  `HybridSearchArgsBuilder`, `GraphNeighborsArgsBuilder` —
  request-side builders with `auth_scope` and any other required
  fields enforced at compile time on the builder (the `bon` macro's
  required-field guarantee).
- `HybridSearchInputs::new(...)`, `HybridSearchParams::default()`
  (already exists), `HybridSearchPage::new(...)`, `ScoreExplain::new(...)`,
  `MemoryStoreCapabilities::new(...)` — module-internal types use
  plain `new` constructors (no need for builder ergonomics).
- `DegradedLeg`, `GraphCandidate` — narrow types use plain struct
  literals internally; external crates that need to construct them
  use `DegradedLeg::graph_unavailable(reason)` etc. (small set of
  named constructors).

Out-of-tree adapter migration path: replace struct-literal
construction with the new builder. The major-version bump documents
the call-site change. A migration note in the changelog walks one
example end-to-end.

Plan:

- `cairn-core::contract::memory_store::CONTRACT_VERSION` → `0.4.0` →
  `0.5.0`.
- `cairn-store-sqlite::ACCEPTED_RANGE` → `[0.5.0, 0.6.0)`. **Lockstep
  upgrade — no rolling-upgrade compatibility window.** The contract
  changes are real: a new required trait method, required new fields
  on existing public structs, mandatory `auth_scope` for graph
  authorization. There is no honest way to make an unrebuilt `0.4.x`
  adapter binary advertise the new method or fields, so the
  previously-considered compatibility shim is dropped. Operators
  upgrade in lockstep: store, MCP wrapper, SDK, and any in-tree
  CLI/skill bundles all move from `0.4.x` to `0.5.0` together.
- `cairn-mcp` and `cairn-sdk` accepted ranges → `[0.5.0, 0.6.0)`.
- **Wire shape for `auth_scope`:** the IDL field is
  `auth_scope: ScopeTuple` — required, no `#[serde(default)]`. A
  payload missing the field is rejected at deserialization with a
  typed error mapped to the verb-level error envelope as
  `SearchError::MissingField { field: "auth_scope" }`. Old payloads
  do not collapse silently into empty-scope behavior because the
  ambiguity reviewer flagged in round 9 cannot be resolved on the
  wire — preserving omission-vs-explicit would require a
  contract-version-pinned `Option<ScopeTuple>` shape that the
  spec is no longer pursuing.
- The migration burden is acknowledged: out-of-tree adapter authors
  must rebuild against `0.5.0`. The next-minor follow-up removes
  nothing; this is the clean break.
- Tests assert: a `0.4.x`-shaped request (no `auth_scope` field)
  fails deserialization at the transport boundary with
  `SearchError::MissingField`. Only `0.5.0`-aware callers (explicit
  `auth_scope` populated) succeed. There is no degraded-graph
  fallback path for `0.4.x` payloads.
- All in-tree adapters and tests are updated to populate `auth_scope`
  and the new capability field. The existing
  `crates/cairn-store-sqlite/tests/capabilities_unchanged.rs` snapshot
  is regenerated; the regenerated snapshot is committed in the same
  PR.
- The IDL (`cairn-idl`) entries for search-args / search-response /
  score_explain / capability struct are updated and `cairn-codegen`
  re-run. `auth_scope` is required at BOTH the Rust-API boundary
(builders enforce it as a required field at compile time) and the
wire/IDL boundary (no `#[serde(default)]`; missing field is a
deserialization error mapped to `SearchError::MissingField`). Purely
  observability fields (`degraded_legs`, `graph_rank`,
  `edge_confidence_score`) carry `#[serde(default)]` so future minor
  additions stay forward-compat.
- Tests in `cairn-store-sqlite/tests/contract_version.rs` assert:
  - `ACCEPTED_RANGE.accepts(CONTRACT_VERSION)`,
  - `ACCEPTED_RANGE` lower bound equals `0.5.0` (lockstep, locked in
    CI),
  - A request without `auth_scope` is rejected at deserialization
    with `SearchError::MissingField { field: "auth_scope" }`,
  - A request with explicit `auth_scope` runs the full 3-leg path,
  - Handshake with a `0.4.x` adapter is rejected outright at
    `MemoryStore::open` (the version-range check fails before any
    request is served).

This is the only contract-version event in #191. All subsequent
follow-ups (depth>1, etc.) are designed to be additive within `0.5.x`,
which the new `#[non_exhaustive]` markers make safe.

## 12. Out of scope (filed as follow-ups)

- Depth > 1 traversal — needs separate ranking model.
- Cross-target graph queries — current scope rules apply unchanged.
- Embedding-driven seed expansion (semantic node match) — keyword FTS only
  for P0 graph leg.
