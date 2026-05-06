# Plan C: MCP graph traversal tools (issue #190)

**Spec:** `docs/superpowers/specs/2026-05-05-mcp-graph-tools-design.md`
**Plan A (prereq):** `docs/superpowers/plans/2026-05-05-mcp-auth-substrate.md` — ships `McpSessionScope`, `McpAuthContext`, `McpStdioConfig`, `ConfigBackedScope`, `McpGraphAvailability`, `CairnConfig::mcp_graph_tools_available`, `serve_stdio_with_store(store, scope, config, principal)`, `cairn status` integration. Treat as already merged.
**Plan B (prereq):** `docs/superpowers/plans/2026-05-05-normalize-entity-name.md` — ships `cairn_core::domain::graph::normalize::normalize_entity_name(&str) -> String`. Treat as already merged.
**Brief sections:** §4 (MCPServer contract), §6.12 (MCP — schemars, stdio transport, wire compat), §8.0.a (capability advertisement)
**Date:** 2026-05-05

## Goal

Implement the read-only graph-traversal MCP surface that Plan A and Plan B leave dangling. Five tools (`graph.query`, `graph.get_entity`, `graph.get_neighbors`, `graph.timeline`, `graph.surprising_connections`) backed by a `GraphQueries` struct in `cairn-store-sqlite` that bakes per-session scope authorization into every CTE, plus a byte-oriented stdio relay shim that survives blank-line interleavings without corrupting CRLF/LF framing. Flip Plan A's stub `mcp_graph_tools_available` to return `Available { tool_count: 5 }` once all preconditions hold.

## Architecture

- **`cairn-store-sqlite::entity_graph::queries`** owns all SQL. `GraphQueries` is constructed with a borrowed store handle, the resolved `Vec<ScopeTuple>`, and a `now: i64`. Every method composes the §3.0 `visible_edges_raw` / `visible_edges` / `visible_nodes` CTE prefix — there is no public method that omits scope.
- **`cairn-mcp::graph_tools`** owns schemars-derived input types, the `GRAPH_TOOLS` static registry, and a `dispatch(queries, name, args)` entry point. No SQL.
- **`cairn-mcp::handler`** routes `tools/list` and `tools/call` through Plan A's `graph_tools_available` predicate before exposing or invoking any graph tool. Both sites consume the same predicate so the cached manifest cannot drift from per-request authorization.
- **`cairn-mcp::relay`** is a byte-oriented stdin shim that drops blank-line frames between JSON-RPC messages and preserves CRLF/LF byte-for-byte. Caps frame size at 16 MiB.
- **`cairn-core::config`** flips Plan A's stub: when transport is stdio, `single_tenant=true`, store advertises `graph_edges`, and a non-empty resolver result is observed, return `McpGraphAvailability::Available { tool_count: 5 }`.

Read paths only. No new migration. No new contract.

## Tech Stack

- Rust 1.95.0, edition 2024, resolver 3
- tokio (`flavor = "multi_thread"` in `serve_stdio_with_store`, `flavor = "current_thread"` in tests)
- rmcp 1.6.0 (pinned)
- schemars derive (input types, JSON Schema generation)
- rusqlite (workspace pin), `IN (?,?,...)` placeholder expansion (no `rarray`/`carray` extension)
- insta snapshot tests
- proptest for IDL round-trips already exists; not required here
- Workspace lints: `forbid(unsafe_code)`, pedantic clippy; `unwrap`/`expect` denied in `cairn-core`

## File Map

| File | Action |
|---|---|
| `crates/cairn-store-sqlite/src/entity_graph/queries.rs` | Create |
| `crates/cairn-store-sqlite/src/entity_graph/mod.rs` | Modify (add `pub mod queries`) |
| `crates/cairn-store-sqlite/tests/graph_queries.rs` | Create |
| `crates/cairn-mcp/src/graph_tools.rs` | Create |
| `crates/cairn-mcp/src/relay.rs` | Create |
| `crates/cairn-mcp/src/lib.rs` | Modify (`pub mod graph_tools; pub mod relay;`, switch `serve_stdio_with_store` to use relay) |
| `crates/cairn-mcp/src/handler.rs` | Modify (extend `list_tools`/`call_tool` with graph routing) |
| `crates/cairn-mcp/tests/graph_tools_manifest.rs` | Create |
| `crates/cairn-mcp/tests/relay.rs` | Create |
| `crates/cairn-core/src/config/mod.rs` | Modify (`mcp_graph_tools_available` returns `Available { tool_count: 5 }` when all conditions hold) |

---

## Task 1: `GraphQueries` skeleton + scope-clause builder

**Files:**
- `crates/cairn-store-sqlite/src/entity_graph/queries.rs` (Create)
- `crates/cairn-store-sqlite/src/entity_graph/mod.rs` (Modify)

Spec: §2.1, §3.0a, §3.0a-bis, §3.0b. Create the struct, the per-`ScopeTuple` six-dimension match clause builder, and a placeholder-list helper. No queries yet.

- [ ] **Write failing test.**
  ```rust
  // crates/cairn-store-sqlite/tests/graph_queries.rs (initial stub)
  use cairn_core::domain::scope::ScopeTuple;
  use cairn_store_sqlite::entity_graph::queries::GraphQueries;

  #[test]
  fn scope_clause_emits_six_coalesce_pairs_per_tuple() {
      let tuples = vec![ScopeTuple::default()];
      let (sql, n) = GraphQueries::scope_match_clause(&tuples);
      // Six dimensions × one tuple = six placeholder pairs OR-joined into
      // a single EXISTS arm. Bind count must match.
      assert_eq!(n, 6);
      assert_eq!(sql.matches("coalesce(json_extract").count(), 6);
      assert!(!sql.contains(" OR ")); // single tuple → no OR
  }

  #[test]
  fn scope_clause_two_tuples_or_joins_arms() {
      let tuples = vec![ScopeTuple::default(), ScopeTuple::default()];
      let (sql, n) = GraphQueries::scope_match_clause(&tuples);
      assert_eq!(n, 12);
      assert_eq!(sql.matches(" OR ").count(), 1);
  }
  ```
- [ ] **Run-fail.** `cargo nextest run -p cairn-store-sqlite --test graph_queries -- scope_clause`
- [ ] **Implement.**
  ```rust
  // crates/cairn-store-sqlite/src/entity_graph/queries.rs
  use cairn_core::domain::scope::ScopeTuple;
  use std::sync::Arc;

  use crate::store::SqliteMemoryStore;

  /// Read-only graph-traversal driver. Constructed per-request after
  /// the MCP handler has resolved the caller's scope set; every
  /// method bakes both the §3.0 active-at-now temporal predicate
  /// and the §3.0a stable-lineage scope predicate into its SQL.
  ///
  /// `allowed_scopes` is the resolver's `Vec<ScopeTuple>` and MUST be
  /// non-empty — empty means deny-all and is the caller's
  /// responsibility to short-circuit upstream (`graph_tools_available`).
  pub struct GraphQueries {
      pub(crate) store: Arc<SqliteMemoryStore>,
      pub(crate) allowed_scopes: Vec<ScopeTuple>,
      pub(crate) now: i64,
  }

  impl GraphQueries {
      pub fn new(
          store: Arc<SqliteMemoryStore>,
          allowed_scopes: Vec<ScopeTuple>,
          now: i64,
      ) -> Self {
          debug_assert!(
              !allowed_scopes.is_empty(),
              "invariant: GraphQueries requires non-empty allowed_scopes; \
               graph_tools_available must short-circuit upstream"
          );
          Self { store, allowed_scopes, now }
      }

      /// Render the §3.0a six-dimension scope match clause.
      ///
      /// Returns `(sql, bind_count)` where `sql` is intended to drop into
      /// an EXISTS subquery as the `<ScopeTuple-match-clause>` placeholder
      /// in the spec, and `bind_count` is the number of `?` parameters the
      /// caller must bind in order (tenant, workspace, session_id, entity,
      /// user, agent — six per tuple, OR'd between tuples).
      pub fn scope_match_clause(
          tuples: &[ScopeTuple],
      ) -> (String, usize) {
          const DIMS: [&str; 6] = [
              "tenant", "workspace", "session_id", "entity", "user", "agent",
          ];
          let mut arms: Vec<String> = Vec::with_capacity(tuples.len());
          for _ in tuples {
              let mut conds: Vec<String> = Vec::with_capacity(DIMS.len());
              for d in DIMS {
                  conds.push(format!(
                      "coalesce(json_extract(r_src.scope, '$.{d}'), '') \
                       = coalesce(?, '')"
                  ));
              }
              arms.push(format!("({})", conds.join(" AND ")));
          }
          (arms.join(" OR "), tuples.len() * DIMS.len())
      }

      /// Expand a `len`-wide `IN (?,?,...)` placeholder list. `len = 0`
      /// returns `"(NULL)"` so the caller may unconditionally splice it
      /// into SQL — a `NULL` membership test is always false, matching
      /// the spec's "empty visited / empty input → omit clause" guidance
      /// without runtime branching at the SQL level.
      pub(crate) fn placeholders(len: usize) -> String {
          if len == 0 {
              return "(NULL)".to_string();
          }
          let mut s = String::with_capacity(len * 2 + 1);
          s.push('(');
          for i in 0..len {
              if i > 0 { s.push(','); }
              s.push('?');
          }
          s.push(')');
          s
      }
  }
  ```
  ```rust
  // crates/cairn-store-sqlite/src/entity_graph/mod.rs
  pub mod queries;
  ```
- [ ] **Run-pass.** `cargo nextest run -p cairn-store-sqlite --test graph_queries -- scope_clause`
- [ ] **Commit.**
  ```bash
  git commit -m "$(cat <<'EOF'
  feat(store-sqlite): GraphQueries skeleton + scope-match clause builder (#190)

  Lands the read-only graph-traversal driver with the §3.0a six-dimension
  ScopeTuple match clause (coalesce-NULL exact equality, never wildcard)
  and an IN-placeholder helper. No queries materialized yet.

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

## Task 2: CTE prefix builder (`visible_edges_raw` / `visible_edges` / `visible_nodes`)

**Files:**
- `crates/cairn-store-sqlite/src/entity_graph/queries.rs` (Modify)
- `crates/cairn-store-sqlite/tests/graph_queries.rs` (Modify)

Spec: §3.0, §3.0a, §3.0a-bis. Centralize the three CTEs so every method splices an identical prefix. The CTE prefix returns the bind-parameter slot count so callers know how many `now`/scope binds to push before their own statement-specific binds.

- [ ] **Write failing test.**
  ```rust
  #[test]
  fn cte_prefix_emits_three_named_ctes_and_counts_binds() {
      let tuples = vec![ScopeTuple::default()];
      let (sql, prefix_binds) = GraphQueries::cte_prefix(&tuples);
      assert!(sql.contains("visible_edges_raw AS"));
      assert!(sql.contains("visible_nodes AS"));
      assert!(sql.contains("visible_edges AS"));
      // 3 :now binds in visible_edges_raw + past-invalidated branch in
      // visible_nodes + 3 scope expansions (raw, episode, past-invalid)
      // = 3 :now + 3*6 scope params = 21
      assert_eq!(prefix_binds.now_count, 3);
      assert_eq!(prefix_binds.scope_count, 18);
  }
  ```
- [ ] **Run-fail.** `cargo nextest run -p cairn-store-sqlite --test graph_queries -- cte_prefix`
- [ ] **Implement.**
  ```rust
  /// Bind-slot accounting for the CTE prefix. Callers bind in this
  /// order: `now` (×now_count), then scope tuple dimensions
  /// (×scope_count, in tuple-major / dimension-minor order matching
  /// `scope_match_clause`).
  pub struct CtePrefixBinds {
      pub now_count: usize,
      pub scope_count: usize,
  }

  impl GraphQueries {
      /// Build the canonical §3.0 CTE prefix used by every public
      /// query. The returned SQL ends with a trailing comma so the
      /// caller appends their own SELECT-feeding CTEs or trailing
      /// statement directly.
      pub(crate) fn cte_prefix(
          tuples: &[ScopeTuple],
      ) -> (String, CtePrefixBinds) {
          let (scope_clause, scope_per_block) =
              Self::scope_match_clause(tuples);
          // visible_edges_raw — temporal + scope + orphan exclusion
          let raw = format!(
              "visible_edges_raw AS (
                 SELECT e.*
                 FROM entity_edges e
                 WHERE e.expired_at IS NULL
                   AND e.valid_at  <= ?
                   AND (e.invalid_at IS NULL OR e.invalid_at > ?)
                   AND e.source_record_id IS NOT NULL
                   AND EXISTS (
                     SELECT 1 FROM records r_src
                     JOIN records r_active
                       ON r_active.target_id = r_src.target_id
                     WHERE r_src.record_id = e.source_record_id
                       AND ({scope_clause})
                       AND r_active.active     = 1
                       AND r_active.tombstoned = 0
                   )
               )"
          );
          // visible_nodes — three OR'd provenance branches
          let nodes = format!(
              "visible_nodes AS (
                 SELECT n.*
                 FROM entity_nodes n
                 WHERE n.expired_at IS NULL
                   AND (
                     EXISTS (
                       SELECT 1 FROM visible_edges_raw e
                       WHERE e.source_id = n.id OR e.target_id = n.id
                     )
                     OR EXISTS (
                       SELECT 1 FROM entity_episodes ep
                       JOIN records r_src ON r_src.record_id = ep.episode_id
                       JOIN records r_active
                         ON r_active.target_id = r_src.target_id
                       WHERE ep.entity_node_id = n.id
                         AND r_active.active     = 1
                         AND r_active.tombstoned = 0
                         AND ({scope_clause})
                     )
                     OR EXISTS (
                       SELECT 1 FROM entity_edges he
                       WHERE (he.source_id = n.id OR he.target_id = n.id)
                         AND he.expired_at IS NULL
                         AND he.valid_at   <= ?
                         AND he.invalid_at IS NOT NULL
                         AND he.invalid_at <= ?
                         AND EXISTS (
                           SELECT 1 FROM records r_src
                           JOIN records r_active
                             ON r_active.target_id = r_src.target_id
                           WHERE r_src.record_id = he.source_record_id
                             AND ({scope_clause})
                             AND r_active.active     = 1
                             AND r_active.tombstoned = 0
                         )
                     )
                   )
               )"
          );
          // visible_edges — endpoint liveness join
          let edges = "visible_edges AS (
                 SELECT e.* FROM visible_edges_raw e
                 WHERE e.source_id IN (SELECT id FROM visible_nodes)
                   AND e.target_id IN (SELECT id FROM visible_nodes)
               )".to_string();
          let sql = format!("WITH {raw}, {nodes}, {edges},");
          (
              sql,
              CtePrefixBinds {
                  // visible_edges_raw: valid_at + invalid_at  → 2 :now
                  // past-invalidated branch:        valid_at + invalid_at → 2 :now
                  // Wait — past-invalidated uses `<= :now` once for valid_at
                  // and once for invalid_at, so 2 :now there too. Total = 4.
                  // Plus 0 binds in episode branch. Total :now = 4? See
                  // SQL above: visible_edges_raw uses 2, past-invalid uses
                  // 2. That's 4. The test asserts 3 — keep the test in sync
                  // with the actual bind count and update it to 4.
                  now_count: 4,
                  scope_count: scope_per_block * 3,
              },
          )
      }
  }
  ```
  Update the test to expect `now_count = 4` (the spec's past-invalidated branch binds `valid_at <= :now` AND `invalid_at <= :now`).
- [ ] **Run-pass.** `cargo nextest run -p cairn-store-sqlite --test graph_queries -- cte_prefix`
- [ ] **Commit.**
  ```bash
  git commit -m "$(cat <<'EOF'
  feat(store-sqlite): CTE prefix builder for visible_edges/nodes (#190)

  Centralizes the §3.0 visible_edges_raw / visible_nodes / visible_edges
  CTE chain so every public GraphQueries method splices an identical
  authorization prefix. Returns bind-slot accounting so callers stage
  parameters in the right order.

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

## Task 3: `get_entity` — `ById` arm

**Files:**
- `crates/cairn-store-sqlite/src/entity_graph/queries.rs` (Modify)
- `crates/cairn-store-sqlite/tests/graph_queries.rs` (Modify)

Spec: §2.1.0 (id-only return), §3.1. Returns `{ id, edge_count }` — no `name`, no `summary`, ever.

- [ ] **Write failing test.**
  ```rust
  use cairn_test_fixtures::graph::tiny_graph;

  #[tokio::test(flavor = "current_thread")]
  async fn get_entity_by_id_returns_id_and_live_edge_count() {
      let f = tiny_graph().await;
      let q = GraphQueries::new(f.store.clone(), vec![f.scope_a.clone()], f.now);
      let hit = q.get_entity_by_id(&f.node_a).unwrap();
      assert_eq!(hit.id, f.node_a);
      assert_eq!(hit.edge_count, 2); // A↔B and A↔C in scope_a
  }

  #[tokio::test(flavor = "current_thread")]
  async fn get_entity_by_id_out_of_scope_returns_none() {
      let f = tiny_graph().await;
      let q = GraphQueries::new(f.store.clone(), vec![f.scope_b.clone()], f.now);
      assert!(q.get_entity_by_id(&f.node_a).unwrap().is_none());
  }
  ```
- [ ] **Run-fail.** `cargo nextest run -p cairn-store-sqlite --test graph_queries -- get_entity_by_id`
- [ ] **Implement.**
  ```rust
  use rusqlite::{params_from_iter, types::Value as SqlValue};

  pub struct EntityHit {
      pub id: String,
      pub echoed_name: Option<String>,
      pub edge_count: i64,
  }

  impl GraphQueries {
      pub fn get_entity_by_id(
          &self,
          id: &str,
      ) -> rusqlite::Result<Option<EntityHit>> {
          let (prefix, _binds) = Self::cte_prefix(&self.allowed_scopes);
          let sql = format!(
              "{prefix}
               edge_count_for_seed AS (
                 SELECT COUNT(*) AS c FROM visible_edges e
                 WHERE e.source_id = ? OR e.target_id = ?
               )
               SELECT v.id, (SELECT c FROM edge_count_for_seed)
               FROM visible_nodes v
               WHERE v.id = ?
               LIMIT 1"
          );
          let conn = self.store.read_conn()?;
          let mut stmt = conn.prepare(&sql)?;
          let mut binds: Vec<SqlValue> = Vec::new();
          self.push_prefix_binds(&mut binds);
          binds.push(SqlValue::Text(id.to_string()));
          binds.push(SqlValue::Text(id.to_string()));
          binds.push(SqlValue::Text(id.to_string()));
          let mut rows = stmt.query(params_from_iter(binds))?;
          if let Some(row) = rows.next()? {
              Ok(Some(EntityHit {
                  id: row.get(0)?,
                  echoed_name: None,
                  edge_count: row.get(1)?,
              }))
          } else {
              Ok(None)
          }
      }

      /// Push :now (×4) and ScopeTuple dimensions (tuple-major) onto
      /// the bind vector in the order `cte_prefix` expects.
      pub(crate) fn push_prefix_binds(&self, binds: &mut Vec<SqlValue>) {
          for _ in 0..4 {
              binds.push(SqlValue::Integer(self.now));
          }
          // 3 scope blocks (raw, episode, past-invalidated) — each
          // emits one full tuple-major expansion.
          for _ in 0..3 {
              for tup in &self.allowed_scopes {
                  for v in tup.dimension_iter() {
                      binds.push(match v {
                          Some(s) => SqlValue::Text(s.to_string()),
                          None    => SqlValue::Null,
                      });
                  }
              }
          }
      }
  }
  ```
  `ScopeTuple::dimension_iter` is assumed from Plan A. If absent, lift the closure inline (six match arms in fixed order).
- [ ] **Run-pass.** `cargo nextest run -p cairn-store-sqlite --test graph_queries -- get_entity_by_id`
- [ ] **Commit.**
  ```bash
  git commit -m "$(cat <<'EOF'
  feat(store-sqlite): GraphQueries::get_entity_by_id (#190)

  Id-only lookup against visible_nodes with an in-scope edge count.
  Out-of-scope ids return None — the §2.1.0 anti-leak contract.

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

## Task 4: `get_entity` — `ByName` arm (uses `normalize_entity_name`)

**Files:**
- `crates/cairn-store-sqlite/src/entity_graph/queries.rs` (Modify)
- `crates/cairn-store-sqlite/tests/graph_queries.rs` (Modify)

Spec: §2.1.0, §3.1. Returns `{ id, echoed_name: Some(input), edge_count }`. Looks up `name_norm = normalize_entity_name(input)` (Plan B).

- [ ] **Write failing test.**
  ```rust
  #[tokio::test(flavor = "current_thread")]
  async fn get_entity_by_name_normalizes_and_echoes_input() {
      let f = tiny_graph().await; // node "Auth Service (v2)" inserted
      let q = GraphQueries::new(f.store.clone(), vec![f.scope_a.clone()], f.now);
      let hit = q
          .get_entity_by_name("Auth Service (v2)")
          .unwrap()
          .expect("found");
      assert_eq!(hit.id, f.node_auth_service);
      // echoed_name is the literal input, NOT a read of entity_nodes.name
      assert_eq!(hit.echoed_name.as_deref(), Some("Auth Service (v2)"));
  }
  ```
- [ ] **Run-fail.** `cargo nextest run -p cairn-store-sqlite --test graph_queries -- get_entity_by_name`
- [ ] **Implement.**
  ```rust
  use cairn_core::domain::graph::normalize::normalize_entity_name;

  impl GraphQueries {
      pub fn get_entity_by_name(
          &self,
          name: &str,
      ) -> rusqlite::Result<Option<EntityHit>> {
          let norm = normalize_entity_name(name);
          let (prefix, _binds) = Self::cte_prefix(&self.allowed_scopes);
          let sql = format!(
              "{prefix}
               SELECT v.id,
                 (SELECT COUNT(*) FROM visible_edges e
                    WHERE e.source_id = v.id OR e.target_id = v.id)
               FROM visible_nodes v
               WHERE v.name_norm = ?
               LIMIT 1"
          );
          let conn = self.store.read_conn()?;
          let mut stmt = conn.prepare(&sql)?;
          let mut binds: Vec<SqlValue> = Vec::new();
          self.push_prefix_binds(&mut binds);
          binds.push(SqlValue::Text(norm));
          let mut rows = stmt.query(params_from_iter(binds))?;
          if let Some(row) = rows.next()? {
              Ok(Some(EntityHit {
                  id: row.get(0)?,
                  // §2.1.0: echo the caller's literal input, never the
                  // canonical row name.
                  echoed_name: Some(name.to_string()),
                  edge_count: row.get(1)?,
              }))
          } else {
              Ok(None)
          }
      }
  }
  ```
- [ ] **Run-pass.** `cargo nextest run -p cairn-store-sqlite --test graph_queries -- get_entity_by_name`
- [ ] **Commit.**
  ```bash
  git commit -m "$(cat <<'EOF'
  feat(store-sqlite): GraphQueries::get_entity_by_name (#190)

  Name lookup goes through normalize_entity_name (Plan B). The response
  echoes the caller's literal input string per §2.1.0 — never reads
  entity_nodes.name, which is shared across scopes.

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

## Task 5: `get_neighbors` — id-only edges with relation/confidence filter

**Files:**
- `crates/cairn-store-sqlite/src/entity_graph/queries.rs` (Modify)
- `crates/cairn-store-sqlite/tests/graph_queries.rs` (Modify)

Spec: §3.2. One-hop, id-only edges, optional `relation` / `min_confidence`.

- [ ] **Write failing test.**
  ```rust
  #[tokio::test(flavor = "current_thread")]
  async fn get_neighbors_filters_by_relation_and_confidence() {
      let f = tiny_graph().await;
      let q = GraphQueries::new(f.store.clone(), vec![f.scope_a.clone()], f.now);
      let edges = q
          .get_neighbors(&f.node_a, Some("calls"), Some(0.7))
          .unwrap();
      assert!(edges.iter().all(|e| e.relation == "calls"));
      assert!(edges.iter().all(|e| e.confidence_score >= 0.7));
  }
  ```
- [ ] **Run-fail.** `cargo nextest run -p cairn-store-sqlite --test graph_queries -- get_neighbors`
- [ ] **Implement.**
  ```rust
  pub struct GraphEdge {
      pub id: String,
      pub source_id: String,
      pub target_id: String,
      pub relation: String,
      pub confidence_score: f64,
      pub valid_at: i64,
  }

  impl GraphQueries {
      pub fn get_neighbors(
          &self,
          seed: &str,
          relation: Option<&str>,
          min_confidence: Option<f64>,
      ) -> rusqlite::Result<Vec<GraphEdge>> {
          let (prefix, _binds) = Self::cte_prefix(&self.allowed_scopes);
          let sql = format!(
              "{prefix}
               SELECT e.id, e.source_id, e.target_id, e.relation,
                      e.confidence_score, e.valid_at
               FROM visible_edges e
               WHERE (e.source_id = ? OR e.target_id = ?)
                 AND (?  IS NULL OR e.relation = ?)
                 AND (?  IS NULL OR e.confidence_score >= ?)"
          );
          let conn = self.store.read_conn()?;
          let mut stmt = conn.prepare(&sql)?;
          let mut binds: Vec<SqlValue> = Vec::new();
          self.push_prefix_binds(&mut binds);
          binds.push(SqlValue::Text(seed.to_string()));
          binds.push(SqlValue::Text(seed.to_string()));
          let rel_param = relation.map(|s| SqlValue::Text(s.to_string()))
              .unwrap_or(SqlValue::Null);
          binds.push(rel_param.clone());
          binds.push(rel_param);
          let conf_param = min_confidence.map(SqlValue::Real)
              .unwrap_or(SqlValue::Null);
          binds.push(conf_param.clone());
          binds.push(conf_param);
          let mut out = Vec::new();
          let mut rows = stmt.query(params_from_iter(binds))?;
          while let Some(row) = rows.next()? {
              out.push(GraphEdge {
                  id: row.get(0)?,
                  source_id: row.get(1)?,
                  target_id: row.get(2)?,
                  relation: row.get(3)?,
                  confidence_score: row.get(4)?,
                  valid_at: row.get(5)?,
              });
          }
          Ok(out)
      }
  }
  ```
- [ ] **Run-pass.** `cargo nextest run -p cairn-store-sqlite --test graph_queries -- get_neighbors`
- [ ] **Commit.**
  ```bash
  git commit -m "$(cat <<'EOF'
  feat(store-sqlite): GraphQueries::get_neighbors (#190)

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

## Task 6: BFS per-wave SQL + Rust loop (no DFS yet, no token budget yet)

**Files:**
- `crates/cairn-store-sqlite/src/entity_graph/queries.rs` (Modify)
- `crates/cairn-store-sqlite/tests/graph_queries.rs` (Modify)

Spec: §3.3 algorithm + per-wave SQL with `ROW_NUMBER()` partitioning. `IN (?,?,...)` for `:frontier` and `:visited`; first-wave omits the `WHERE other_id NOT IN (...)` clause.

- [ ] **Write failing test.**
  ```rust
  #[tokio::test(flavor = "current_thread")]
  async fn bfs_two_hops_returns_depth_stratified_set() {
      let f = tiny_graph().await;
      let q = GraphQueries::new(f.store.clone(), vec![f.scope_a.clone()], f.now);
      let res = q.query_bfs(&f.node_a, 2, 64).unwrap();
      assert_eq!(res.nodes[0].id, f.node_a); // seed first
      assert!(res.nodes.iter().any(|n| n.id == f.node_b));
      // depth_of cap respected
      assert!(res.depth_of.values().all(|&d| d <= 2));
  }
  ```
- [ ] **Run-fail.** `cargo nextest run -p cairn-store-sqlite --test graph_queries -- bfs_two_hops`
- [ ] **Implement.**
  ```rust
  use indexmap::IndexMap;
  use std::collections::HashMap;

  pub struct GraphSubgraph {
      pub nodes: Vec<GraphNode>,
      pub edges: Vec<GraphEdge>,
      pub parent_of: HashMap<String, String>, // node_id -> edge_id
      pub depth_of:  HashMap<String, u32>,
  }

  pub struct GraphNode { pub id: String }

  impl GraphQueries {
      pub fn query_bfs(
          &self,
          seed: &str,
          max_hops: u32,
          node_budget: usize,
      ) -> rusqlite::Result<GraphSubgraph> {
          let mut visited: IndexMap<String, GraphNode> = IndexMap::new();
          let mut edges:  Vec<GraphEdge> = Vec::new();
          let mut parent_of: HashMap<String, String> = HashMap::new();
          let mut depth_of:  HashMap<String, u32>    = HashMap::new();

          // Seed visibility check — falls through visible_nodes.
          let Some(seed_hit) = self.get_entity_by_id(seed)? else {
              return Ok(GraphSubgraph {
                  nodes: vec![], edges: vec![],
                  parent_of, depth_of,
              });
          };
          visited.insert(seed_hit.id.clone(), GraphNode { id: seed_hit.id.clone() });
          depth_of.insert(seed_hit.id.clone(), 0);

          let mut frontier: Vec<String> = vec![seed.to_string()];
          for depth in 1..=max_hops {
              if frontier.is_empty() { break; }
              if visited.len() >= node_budget { break; }
              let wave_cap = node_budget - visited.len();
              let visited_ids: Vec<String> =
                  visited.keys().cloned().collect();
              let wave = self.bfs_wave_sql(
                  &frontier, &visited_ids, wave_cap,
              )?;
              let mut next_frontier = Vec::new();
              for e in wave {
                  if visited.len() >= node_budget { break; }
                  let other = if frontier.iter().any(|f| f == &e.source_id) {
                      e.target_id.clone()
                  } else {
                      e.source_id.clone()
                  };
                  if visited.contains_key(&other) { continue; }
                  visited.insert(other.clone(), GraphNode { id: other.clone() });
                  parent_of.insert(other.clone(), e.id.clone());
                  depth_of.insert(other.clone(), depth);
                  edges.push(e);
                  next_frontier.push(other);
              }
              frontier = next_frontier;
          }
          Ok(GraphSubgraph {
              nodes: visited.into_iter().map(|(_, n)| n).collect(),
              edges, parent_of, depth_of,
          })
      }

      fn bfs_wave_sql(
          &self,
          frontier: &[String],
          visited:  &[String],
          wave_cap: usize,
      ) -> rusqlite::Result<Vec<GraphEdge>> {
          let (prefix, _binds) = Self::cte_prefix(&self.allowed_scopes);
          let frontier_in = Self::placeholders(frontier.len());
          let visited_filter = if visited.is_empty() {
              String::new()
          } else {
              format!(
                  "WHERE other_id NOT IN {}",
                  Self::placeholders(visited.len())
              )
          };
          let sql = format!(
              "{prefix}
               edge_with_other AS (
                 SELECT e.id, e.source_id, e.target_id, e.relation,
                        e.confidence_score, e.valid_at,
                        CASE WHEN e.source_id IN {frontier_in}
                             THEN e.target_id ELSE e.source_id END AS other_id
                 FROM visible_edges e
                 WHERE e.source_id IN {frontier_in}
                    OR e.target_id IN {frontier_in}
               ),
               candidate AS (
                 SELECT id, source_id, target_id, relation,
                        confidence_score, valid_at, other_id,
                        ROW_NUMBER() OVER (
                          PARTITION BY other_id
                          ORDER BY confidence_score DESC, id ASC
                        ) AS rn
                 FROM edge_with_other
                 {visited_filter}
               )
               SELECT id, source_id, target_id, relation,
                      confidence_score, valid_at
               FROM candidate
               WHERE rn = 1
               ORDER BY confidence_score DESC, id ASC
               LIMIT ?"
          );
          let conn = self.store.read_conn()?;
          let mut stmt = conn.prepare(&sql)?;
          let mut binds: Vec<SqlValue> = Vec::new();
          self.push_prefix_binds(&mut binds);
          // frontier appears 3× in SQL above
          for _ in 0..3 {
              for f in frontier {
                  binds.push(SqlValue::Text(f.clone()));
              }
          }
          for v in visited {
              binds.push(SqlValue::Text(v.clone()));
          }
          binds.push(SqlValue::Integer(wave_cap as i64));
          let mut out = Vec::new();
          let mut rows = stmt.query(params_from_iter(binds))?;
          // Stream — break the moment visited_len would exceed budget.
          while let Some(row) = rows.next()? {
              out.push(GraphEdge {
                  id: row.get(0)?,
                  source_id: row.get(1)?,
                  target_id: row.get(2)?,
                  relation: row.get(3)?,
                  confidence_score: row.get(4)?,
                  valid_at: row.get(5)?,
              });
              if out.len() >= wave_cap { break; }
          }
          Ok(out)
      }
  }
  ```
- [ ] **Run-pass.** `cargo nextest run -p cairn-store-sqlite --test graph_queries -- bfs_two_hops`
- [ ] **Commit.**
  ```bash
  git commit -m "$(cat <<'EOF'
  feat(store-sqlite): GraphQueries BFS traversal with per-wave window CTE (#190)

  Drives BFS from Rust over the §3.0 visible_edges primitive. The per-wave
  query partitions edges by distinct unseen neighbor before LIMIT applies,
  preventing reachable nodes from being displaced by duplicate edges.

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

## Task 7: DFS reorder via `parent_of` post-order walk

**Files:**
- `crates/cairn-store-sqlite/src/entity_graph/queries.rs` (Modify)
- `crates/cairn-store-sqlite/tests/graph_queries.rs` (Modify)

Spec: §3.3 property 5. Reuse the BFS edge set; reorder via DFS over `parent_of`.

- [ ] **Write failing test.**
  ```rust
  #[tokio::test(flavor = "current_thread")]
  async fn query_dfs_reorders_bfs_via_parent_of() {
      let f = tiny_graph().await;
      let q = GraphQueries::new(f.store.clone(), vec![f.scope_a.clone()], f.now);
      let bfs = q.query_bfs(&f.node_a, 3, 64).unwrap();
      let dfs = q.query_dfs(&f.node_a, 3, 64).unwrap();
      // Same edge set, possibly different order
      assert_eq!(
          bfs.edges.iter().map(|e| &e.id).collect::<std::collections::HashSet<_>>(),
          dfs.edges.iter().map(|e| &e.id).collect::<std::collections::HashSet<_>>(),
      );
      // DFS visits a child before any sibling's subtree
      assert_eq!(dfs.nodes[0].id, f.node_a);
  }
  ```
- [ ] **Run-fail.** `cargo nextest run -p cairn-store-sqlite --test graph_queries -- query_dfs_reorders`
- [ ] **Implement.**
  ```rust
  impl GraphQueries {
      pub fn query_dfs(
          &self,
          seed: &str,
          max_hops: u32,
          node_budget: usize,
      ) -> rusqlite::Result<GraphSubgraph> {
          let bfs = self.query_bfs(seed, max_hops, node_budget)?;
          // Build child map from parent_of.
          let mut children: HashMap<String, Vec<String>> = HashMap::new();
          for (child, edge_id) in &bfs.parent_of {
              // Find parent for this child by inspecting the edge.
              let edge = bfs.edges.iter().find(|e| &e.id == edge_id);
              let Some(edge) = edge else { continue };
              let parent = if edge.source_id == *child {
                  edge.target_id.clone()
              } else {
                  edge.source_id.clone()
              };
              children.entry(parent).or_default().push(child.clone());
          }
          // Stable child ordering: BFS insertion order of the children
          // themselves.
          let bfs_order: HashMap<&str, usize> = bfs.nodes.iter()
              .enumerate()
              .map(|(i, n)| (n.id.as_str(), i))
              .collect();
          for kids in children.values_mut() {
              kids.sort_by_key(|c| bfs_order.get(c.as_str()).copied().unwrap_or(usize::MAX));
          }
          let mut order: Vec<GraphNode> = Vec::with_capacity(bfs.nodes.len());
          let mut stack: Vec<String> = vec![seed.to_string()];
          let mut emitted: std::collections::HashSet<String> =
              std::collections::HashSet::new();
          while let Some(top) = stack.pop() {
              if !emitted.insert(top.clone()) { continue; }
              order.push(GraphNode { id: top.clone() });
              if let Some(kids) = children.get(&top) {
                  // Reverse so the first child is processed first.
                  for k in kids.iter().rev() {
                      stack.push(k.clone());
                  }
              }
          }
          Ok(GraphSubgraph {
              nodes: order,
              edges: bfs.edges,
              parent_of: bfs.parent_of,
              depth_of:  bfs.depth_of,
          })
      }
  }
  ```
- [ ] **Run-pass.** `cargo nextest run -p cairn-store-sqlite --test graph_queries -- query_dfs_reorders`
- [ ] **Commit.**
  ```bash
  git commit -m "$(cat <<'EOF'
  feat(store-sqlite): DFS reorder via parent_of post-order walk (#190)

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

## Task 8: Token-budget accumulator during serialization

**Files:**
- `crates/cairn-store-sqlite/src/entity_graph/queries.rs` (Modify)
- `crates/cairn-store-sqlite/tests/graph_queries.rs` (Modify)

Spec: §3.3 property 6. Layered defense — apply during serialization, not query.

- [ ] **Write failing test.**
  ```rust
  #[test]
  fn token_budget_truncates_at_byte_threshold() {
      let edges: Vec<GraphEdge> = (0..100).map(|i| GraphEdge {
          id: format!("e{i}"), source_id: "s".into(),
          target_id: "t".into(), relation: "r".into(),
          confidence_score: 1.0, valid_at: 0,
      }).collect();
      let truncated = GraphQueries::truncate_to_token_budget(&edges, 200);
      let s = serde_json::to_string(&truncated).unwrap();
      assert!(s.len() <= 200 + 64); // soft cap with single-element overshoot
      assert!(truncated.len() < edges.len());
  }
  ```
- [ ] **Run-fail.** `cargo nextest run -p cairn-store-sqlite --test graph_queries -- token_budget`
- [ ] **Implement.**
  ```rust
  use serde::Serialize;

  // Also derive Serialize on GraphEdge / GraphNode at this point.
  impl GraphQueries {
      pub fn truncate_to_token_budget<T: Serialize>(
          rows: &[T],
          byte_budget: usize,
      ) -> Vec<&T> {
          let mut out: Vec<&T> = Vec::new();
          let mut used = 2usize; // "[]"
          for r in rows {
              let s = match serde_json::to_string(r) {
                  Ok(s) => s,
                  Err(_) => continue,
              };
              let cost = s.len() + 1; // comma
              if used + cost > byte_budget && !out.is_empty() { break; }
              out.push(r);
              used += cost;
          }
          out
      }
  }
  ```
- [ ] **Run-pass.** `cargo nextest run -p cairn-store-sqlite --test graph_queries -- token_budget`
- [ ] **Commit.**
  ```bash
  git commit -m "$(cat <<'EOF'
  feat(store-sqlite): token-budget accumulator for graph serialization (#190)

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

## Task 9: `timeline` — `include_history` / `include_expired` flags

**Files:**
- `crates/cairn-store-sqlite/src/entity_graph/queries.rs` (Modify)
- `crates/cairn-store-sqlite/tests/graph_queries.rs` (Modify)

Spec: §3.4. Defaults match active-at-now. Scope filter unconditional. Endpoint liveness composes with `visible_nodes`.

- [ ] **Write failing test.**
  ```rust
  #[tokio::test(flavor = "current_thread")]
  async fn timeline_default_excludes_future_and_expired() {
      let f = timeline_fixture().await;
      let q = GraphQueries::new(f.store.clone(), vec![f.scope_a.clone()], f.now);
      let entries = q.timeline(&f.node_a, false, false).unwrap();
      assert!(entries.iter().all(|e| e.expired_at.is_none()));
      assert!(entries.iter().all(|e| e.valid_at <= f.now));
  }

  #[tokio::test(flavor = "current_thread")]
  async fn timeline_include_history_surfaces_future_dated() {
      let f = timeline_fixture().await;
      let q = GraphQueries::new(f.store.clone(), vec![f.scope_a.clone()], f.now);
      let entries = q.timeline(&f.node_a, true, false).unwrap();
      assert!(entries.iter().any(|e| e.valid_at > f.now));
  }
  ```
- [ ] **Run-fail.** `cargo nextest run -p cairn-store-sqlite --test graph_queries -- timeline`
- [ ] **Implement.**
  ```rust
  pub struct TimelineEntry {
      pub id: String,
      pub source_id: String,
      pub target_id: String,
      pub relation: String,
      pub confidence_score: f64,
      pub valid_at: i64,
      pub invalid_at: Option<i64>,
      pub created_at: i64,
      pub expired_at: Option<i64>,
      pub tombstone_reason: Option<String>,
      pub source_record_id: Option<String>,
  }

  impl GraphQueries {
      pub fn timeline(
          &self,
          seed: &str,
          include_history: bool,
          include_expired: bool,
      ) -> rusqlite::Result<Vec<TimelineEntry>> {
          // Note: we re-emit the scope_filtered CTE here rather than reusing
          // visible_edges_raw, because timeline relaxes temporal predicates
          // independently. visible_nodes is reused for endpoint liveness.
          let (scope_clause, _) = Self::scope_match_clause(&self.allowed_scopes);
          let (prefix, _binds) = Self::cte_prefix(&self.allowed_scopes);
          let sql = format!(
              "{prefix}
               scope_filtered AS (
                 SELECT e.* FROM entity_edges e
                 WHERE e.source_record_id IS NOT NULL
                   AND EXISTS (
                     SELECT 1 FROM records r_src
                     JOIN records r_active
                       ON r_active.target_id = r_src.target_id
                     WHERE r_src.record_id = e.source_record_id
                       AND ({scope_clause})
                       AND r_active.active     = 1
                       AND r_active.tombstoned = 0
                   )
               )
               SELECT id, source_id, target_id, relation, confidence_score,
                      valid_at, invalid_at, created_at, expired_at,
                      tombstone_reason, source_record_id
               FROM scope_filtered
               WHERE (source_id = ? OR target_id = ?)
                 AND source_id IN (SELECT id FROM visible_nodes)
                 AND target_id IN (SELECT id FROM visible_nodes)
                 AND (? = 1 OR expired_at IS NULL)
                 AND (
                   ? = 1
                   OR (valid_at <= ?
                       AND (invalid_at IS NULL OR invalid_at > ?))
                 )
               ORDER BY valid_at ASC, created_at ASC"
          );
          let conn = self.store.read_conn()?;
          let mut stmt = conn.prepare(&sql)?;
          let mut binds: Vec<SqlValue> = Vec::new();
          self.push_prefix_binds(&mut binds);
          // scope_filtered CTE uses one extra block of scope params.
          for tup in &self.allowed_scopes {
              for v in tup.dimension_iter() {
                  binds.push(match v {
                      Some(s) => SqlValue::Text(s.to_string()),
                      None    => SqlValue::Null,
                  });
              }
          }
          binds.push(SqlValue::Text(seed.to_string()));
          binds.push(SqlValue::Text(seed.to_string()));
          binds.push(SqlValue::Integer(if include_expired { 1 } else { 0 }));
          binds.push(SqlValue::Integer(if include_history { 1 } else { 0 }));
          binds.push(SqlValue::Integer(self.now));
          binds.push(SqlValue::Integer(self.now));
          let mut out = Vec::new();
          let mut rows = stmt.query(params_from_iter(binds))?;
          while let Some(row) = rows.next()? {
              out.push(TimelineEntry {
                  id: row.get(0)?, source_id: row.get(1)?,
                  target_id: row.get(2)?, relation: row.get(3)?,
                  confidence_score: row.get(4)?, valid_at: row.get(5)?,
                  invalid_at: row.get(6)?, created_at: row.get(7)?,
                  expired_at: row.get(8)?, tombstone_reason: row.get(9)?,
                  source_record_id: row.get(10)?,
              });
          }
          Ok(out)
      }
  }
  ```
- [ ] **Run-pass.** `cargo nextest run -p cairn-store-sqlite --test graph_queries -- timeline`
- [ ] **Commit.**
  ```bash
  git commit -m "$(cat <<'EOF'
  feat(store-sqlite): GraphQueries::timeline with include_history/include_expired (#190)

  Default temporal envelope matches active-at-now. Scope predicate is
  unconditional. Endpoint liveness composes with visible_nodes so a
  tombstoned-node edge cannot leak through the audit view.

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

## Task 10: `surprising_connections` — modal-scope CTE keyed on `records.scope`

**Files:**
- `crates/cairn-store-sqlite/src/entity_graph/queries.rs` (Modify)
- `crates/cairn-store-sqlite/tests/graph_queries.rs` (Modify)

Spec: §3.5. Bonus keyed on actual scope id, not `source_record_id`.

- [ ] **Write failing test.**
  ```rust
  #[tokio::test(flavor = "current_thread")]
  async fn surprising_connections_scores_cross_scope_above_same_scope() {
      // 3 in-scope edges in S_a, 1 cross-scope edge in S_b — both
      // resolver entries authorize the caller.
      let f = surprise_fixture().await;
      let q = GraphQueries::new(
          f.store.clone(),
          vec![f.scope_a.clone(), f.scope_b.clone()],
          f.now,
      );
      let hits = q.surprising_connections(&[
          f.node_a.clone(), f.node_b.clone(), f.node_c.clone(),
      ], 10).unwrap();
      // Highest score must be the S_b edge (modal is S_a).
      assert_eq!(hits[0].edge.source_record_id.as_deref(), Some(&f.rec_b[..]));
      assert!((hits[0].score - 2.0 * hits[0].edge.confidence_score).abs() < 1e-9);
  }
  ```
- [ ] **Run-fail.** `cargo nextest run -p cairn-store-sqlite --test graph_queries -- surprising`
- [ ] **Implement.**
  ```rust
  pub struct SurpriseHit {
      pub edge: GraphEdge,
      pub scope_id: String,
      pub score: f64,
  }

  impl GraphQueries {
      pub fn surprising_connections(
          &self,
          input: &[String],
          limit: usize,
      ) -> rusqlite::Result<Vec<SurpriseHit>> {
          if input.is_empty() { return Ok(vec![]); }
          let (prefix, _binds) = Self::cte_prefix(&self.allowed_scopes);
          let input_in = Self::placeholders(input.len());
          let sql = format!(
              "{prefix}
               input(id) AS (SELECT value FROM json_each(?)),
               edge_scope AS (
                 SELECT e.id AS edge_id, r.scope AS scope_id, e.*
                 FROM visible_edges e
                 JOIN records r ON r.record_id = e.source_record_id
               ),
               modal_scope AS (
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
                                 WHEN es.scope_id <>
                                      (SELECT scope_id FROM modal_scope)
                                 THEN 1.0 ELSE 0.0
                               END) AS score
                 FROM edge_scope es
                 WHERE es.source_id IN {input_in}
                   AND es.target_id IN {input_in}
               )
               SELECT id, source_id, target_id, relation, confidence_score,
                      valid_at, scope_id, score
               FROM scored
               ORDER BY score DESC, edge_id ASC
               LIMIT ?"
          );
          let conn = self.store.read_conn()?;
          let mut stmt = conn.prepare(&sql)?;
          let mut binds: Vec<SqlValue> = Vec::new();
          self.push_prefix_binds(&mut binds);
          let json_array = serde_json::to_string(input)
              .unwrap_or_else(|_| "[]".to_string());
          binds.push(SqlValue::Text(json_array));
          for v in input.iter() {
              binds.push(SqlValue::Text(v.clone()));
          }
          for v in input.iter() {
              binds.push(SqlValue::Text(v.clone()));
          }
          binds.push(SqlValue::Integer(limit as i64));
          let mut out = Vec::new();
          let mut rows = stmt.query(params_from_iter(binds))?;
          while let Some(row) = rows.next()? {
              out.push(SurpriseHit {
                  edge: GraphEdge {
                      id: row.get(0)?, source_id: row.get(1)?,
                      target_id: row.get(2)?, relation: row.get(3)?,
                      confidence_score: row.get(4)?, valid_at: row.get(5)?,
                  },
                  scope_id: row.get(6)?,
                  score: row.get(7)?,
              });
          }
          Ok(out)
      }
  }
  ```
- [ ] **Run-pass.** `cargo nextest run -p cairn-store-sqlite --test graph_queries -- surprising`
- [ ] **Commit.**
  ```bash
  git commit -m "$(cat <<'EOF'
  feat(store-sqlite): GraphQueries::surprising_connections (#190)

  Modal-scope CTE keys the cross-scope bonus on actual records.scope,
  not source_record_id, so same-scope edges from different records
  cannot falsely score as cross-scope.

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

## Task 11: schemars input types + `GRAPH_TOOLS` registry

**Files:**
- `crates/cairn-mcp/src/graph_tools.rs` (Create)
- `crates/cairn-mcp/src/lib.rs` (Modify)

Spec: §3.1 (`GetEntityArgs` `untagged + deny_unknown_fields`), §4.1 (registry shape).

- [ ] **Write failing test.**
  ```rust
  // crates/cairn-mcp/tests/graph_tools_manifest.rs (initial bit)
  use cairn_mcp::graph_tools::{GRAPH_TOOLS, GetEntityArgs};

  #[test]
  fn graph_tools_registry_lists_five_tools() {
      let names: Vec<_> = GRAPH_TOOLS.iter().map(|d| d.name).collect();
      assert_eq!(names, vec![
          "graph.query", "graph.get_entity", "graph.get_neighbors",
          "graph.timeline", "graph.surprising_connections",
      ]);
  }

  #[test]
  fn get_entity_args_rejects_both_id_and_name() {
      let v = serde_json::json!({"id": "x", "name": "y"});
      assert!(serde_json::from_value::<GetEntityArgs>(v).is_err());
  }

  #[test]
  fn get_entity_args_accepts_id_only() {
      let v = serde_json::json!({"id": "x"});
      let parsed: GetEntityArgs = serde_json::from_value(v).unwrap();
      matches!(parsed, GetEntityArgs::ById { .. });
  }
  ```
- [ ] **Run-fail.** `cargo nextest run -p cairn-mcp --test graph_tools_manifest -- registry`
- [ ] **Implement.**
  ```rust
  // crates/cairn-mcp/src/graph_tools.rs
  use schemars::JsonSchema;
  use serde::{Deserialize, Serialize};
  use std::sync::OnceLock;

  pub struct GraphToolDecl {
      pub name: &'static str,
      pub description: &'static str,
      pub schema: &'static OnceLock<Vec<u8>>,
      pub schema_for: fn() -> Vec<u8>,
  }

  fn schema_bytes<T: JsonSchema>() -> Vec<u8> {
      let s = schemars::schema_for!(T);
      serde_json::to_vec(&s).unwrap_or_default()
  }

  // ----- per-tool input types ---------------------------------------------

  #[derive(Deserialize, Serialize, JsonSchema)]
  #[serde(untagged, deny_unknown_fields)]
  pub enum GetEntityArgs {
      ById  { id:   String },
      ByName { name: String },
  }

  #[derive(Deserialize, Serialize, JsonSchema)]
  #[serde(deny_unknown_fields)]
  pub struct GetNeighborsArgs {
      pub id: String,
      #[serde(default)] pub relation: Option<String>,
      #[serde(default)] pub min_confidence: Option<f64>,
  }

  #[derive(Deserialize, Serialize, JsonSchema)]
  #[serde(deny_unknown_fields)]
  pub struct QueryGraphArgs {
      pub seed: String,
      #[serde(default = "default_max_hops")]    pub max_hops:    u32,
      #[serde(default = "default_node_budget")] pub node_budget: u32,
      #[serde(default = "default_token_budget")]pub token_budget:u32,
      #[serde(default)] pub mode: TraversalMode,
  }
  fn default_max_hops() -> u32 { 3 }
  fn default_node_budget() -> u32 { 64 }
  fn default_token_budget() -> u32 { 8192 }

  #[derive(Default, Deserialize, Serialize, JsonSchema)]
  #[serde(rename_all = "lowercase")]
  pub enum TraversalMode { #[default] Bfs, Dfs }

  #[derive(Deserialize, Serialize, JsonSchema)]
  #[serde(deny_unknown_fields)]
  pub struct TimelineArgs {
      pub id: String,
      #[serde(default)] pub include_history: bool,
      #[serde(default)] pub include_expired: bool,
  }

  #[derive(Deserialize, Serialize, JsonSchema)]
  #[serde(deny_unknown_fields)]
  pub struct SurprisingArgs {
      pub ids: Vec<String>,
      #[serde(default = "default_surprise_limit")] pub limit: u32,
  }
  fn default_surprise_limit() -> u32 { 16 }

  // ----- registry ---------------------------------------------------------

  macro_rules! tool {
      ($name:literal, $desc:literal, $args:ty) => {{
          static SCHEMA: OnceLock<Vec<u8>> = OnceLock::new();
          GraphToolDecl {
              name: $name,
              description: $desc,
              schema: &SCHEMA,
              schema_for: || schema_bytes::<$args>(),
          }
      }};
  }

  pub static GRAPH_TOOLS: &[GraphToolDecl] = &[
      tool!("graph.query",
        "BFS/DFS from a seed entity within hop and token budget. \
         Discovered nodes are returned id-only to avoid cross-scope name leaks.",
        QueryGraphArgs),
      tool!("graph.get_entity",
        "Look up an entity by id (returns {id, edge_count}) or name \
         (returns {id, echoed_name, edge_count}).",
        GetEntityArgs),
      tool!("graph.get_neighbors",
        "Return one-hop in-scope edges incident to the given id. Optional \
         relation and min_confidence filters.",
        GetNeighborsArgs),
      tool!("graph.timeline",
        "Return all currently-visible edges for an entity ordered by valid_at. \
         include_history and include_expired flags opt into broader windows; \
         scope filter is unconditional.",
        TimelineArgs),
      tool!("graph.surprising_connections",
        "Score in-scope edges between input entities; cross-scope (relative \
         to the modal records.scope) gets a 2× confidence bonus.",
        SurprisingArgs),
  ];

  pub fn schema_of(decl: &GraphToolDecl) -> &'static [u8] {
      decl.schema.get_or_init(|| (decl.schema_for)())
  }
  ```
  ```rust
  // crates/cairn-mcp/src/lib.rs (add)
  pub mod graph_tools;
  ```
- [ ] **Run-pass.** `cargo nextest run -p cairn-mcp --test graph_tools_manifest -- registry`
- [ ] **Commit.**
  ```bash
  git commit -m "$(cat <<'EOF'
  feat(mcp): GRAPH_TOOLS registry with schemars-derived input types (#190)

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

## Task 12: `dispatch` fn — parse args, invoke `GraphQueries`, serialize result

**Files:**
- `crates/cairn-mcp/src/graph_tools.rs` (Modify)
- `crates/cairn-mcp/tests/graph_tools_manifest.rs` (Modify)

Spec: §4.2.

- [ ] **Write failing test.**
  ```rust
  #[tokio::test(flavor = "current_thread")]
  async fn dispatch_get_entity_by_id_returns_callable_result() {
      let f = tiny_graph_async().await;
      let q = GraphQueries::new(f.store.clone(), vec![f.scope_a.clone()], f.now);
      let args = serde_json::json!({"id": f.node_a});
      let res = cairn_mcp::graph_tools::dispatch(
          &q, "graph.get_entity",
          Some(args.as_object().unwrap().clone()),
      ).await;
      assert!(!res.is_error.unwrap_or(false));
  }
  ```
- [ ] **Run-fail.** `cargo nextest run -p cairn-mcp --test graph_tools_manifest -- dispatch_`
- [ ] **Implement.**
  ```rust
  use rmcp::model::{CallToolResult, Content, RawContent, RawTextContent};
  use serde_json::{Map, Value};
  use cairn_store_sqlite::entity_graph::queries::GraphQueries;

  pub async fn dispatch(
      queries: &GraphQueries,
      name: &str,
      arguments: Option<Map<String, Value>>,
  ) -> CallToolResult {
      let args = arguments.unwrap_or_default();
      let result: Result<Value, String> = match name {
          "graph.get_entity" => match serde_json::from_value::<GetEntityArgs>(
              Value::Object(args)) {
              Ok(GetEntityArgs::ById { id }) => queries.get_entity_by_id(&id)
                  .map_err(|e| e.to_string())
                  .and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string())),
              Ok(GetEntityArgs::ByName { name }) => queries.get_entity_by_name(&name)
                  .map_err(|e| e.to_string())
                  .and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string())),
              Err(e) => Err(format!("invalid args: {e}")),
          },
          "graph.get_neighbors" => match serde_json::from_value::<GetNeighborsArgs>(
              Value::Object(args)) {
              Ok(a) => queries.get_neighbors(&a.id, a.relation.as_deref(), a.min_confidence)
                  .map_err(|e| e.to_string())
                  .and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string())),
              Err(e) => Err(format!("invalid args: {e}")),
          },
          "graph.query" => match serde_json::from_value::<QueryGraphArgs>(
              Value::Object(args)) {
              Ok(a) => {
                  let max_hops = a.max_hops.min(5);
                  let res = match a.mode {
                      TraversalMode::Bfs => queries.query_bfs(&a.seed, max_hops, a.node_budget as usize),
                      TraversalMode::Dfs => queries.query_dfs(&a.seed, max_hops, a.node_budget as usize),
                  };
                  res.map_err(|e| e.to_string())
                      .and_then(|sg| {
                          // Apply token budget at serialization time.
                          let trimmed = GraphQueries::truncate_to_token_budget(
                              &sg.edges, a.token_budget as usize);
                          serde_json::to_value(serde_json::json!({
                              "nodes": sg.nodes,
                              "edges": trimmed,
                          })).map_err(|e| e.to_string())
                      })
              }
              Err(e) => Err(format!("invalid args: {e}")),
          },
          "graph.timeline" => match serde_json::from_value::<TimelineArgs>(
              Value::Object(args)) {
              Ok(a) => queries.timeline(&a.id, a.include_history, a.include_expired)
                  .map_err(|e| e.to_string())
                  .and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string())),
              Err(e) => Err(format!("invalid args: {e}")),
          },
          "graph.surprising_connections" => match serde_json::from_value::<SurprisingArgs>(
              Value::Object(args)) {
              Ok(a) => queries.surprising_connections(&a.ids, a.limit as usize)
                  .map_err(|e| e.to_string())
                  .and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string())),
              Err(e) => Err(format!("invalid args: {e}")),
          },
          _ => Err(format!("unknown graph tool: {name}")),
      };
      match result {
          Ok(v) => CallToolResult {
              content: vec![Content { raw: RawContent::Text(
                  RawTextContent { text: serde_json::to_string(&v).unwrap_or_default() }
              ), annotations: None }],
              is_error: Some(false),
              meta: None,
              structured_content: Some(v),
          },
          Err(e) => CallToolResult {
              content: vec![Content { raw: RawContent::Text(
                  RawTextContent { text: e }
              ), annotations: None }],
              is_error: Some(true),
              meta: None,
              structured_content: None,
          },
      }
  }
  ```
- [ ] **Run-pass.** `cargo nextest run -p cairn-mcp --test graph_tools_manifest -- dispatch_`
- [ ] **Commit.**
  ```bash
  git commit -m "$(cat <<'EOF'
  feat(mcp): graph_tools::dispatch routes 5 tools to GraphQueries (#190)

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

## Task 13: Wire `dispatch` into `handler::call_tool`

**Files:**
- `crates/cairn-mcp/src/handler.rs` (Modify)
- `crates/cairn-mcp/tests/graph_tools_manifest.rs` (Modify)

Spec: §4.2. Defense-in-depth: re-evaluate `graph_tools_available` (Plan A) inside `call_tool` before constructing `GraphQueries`.

- [ ] **Write failing test.**
  ```rust
  #[tokio::test(flavor = "current_thread")]
  async fn call_tool_routes_graph_tool_when_available() {
      let h = build_handler_single_tenant_graph_capable();
      let req = call_tool_request("graph.get_entity", serde_json::json!({"id":"a"}));
      let res = h.call_tool(req, ctx_with_principal()).await.unwrap();
      assert!(!res.is_error.unwrap_or(true));
  }

  #[tokio::test(flavor = "current_thread")]
  async fn call_tool_returns_capability_unavailable_when_resolver_empty() {
      let h = build_handler_resolver_returns_empty();
      let req = call_tool_request("graph.get_entity", serde_json::json!({"id":"a"}));
      let res = h.call_tool(req, ctx_with_principal()).await.unwrap();
      assert_eq!(res.is_error, Some(true));
  }
  ```
- [ ] **Run-fail.** `cargo nextest run -p cairn-mcp --test graph_tools_manifest -- call_tool`
- [ ] **Implement.** In `handler.rs::call_tool`, after the IDL `TOOLS` lookup but before falling through to "unknown tool":
  ```rust
  if cairn_mcp::graph_tools::GRAPH_TOOLS.iter().any(|d| d.name == name.as_ref()) {
      let ctx = self.auth_context_from(&request_context)?;
      if !self.graph_tools_available(&ctx) {
          return Ok(capability_unavailable_result(&name));
      }
      let store = self.store.as_ref().expect("checked by graph_tools_available");
      let scope = self.scope.as_ref().expect("checked by graph_tools_available");
      let allowed = scope.allowed_scopes(&ctx).expect("checked above");
      let now = chrono::Utc::now().timestamp_millis();
      let queries = GraphQueries::new(store.clone(), allowed, now);
      return Ok(cairn_mcp::graph_tools::dispatch(&queries, &name, arguments).await);
  }
  ```
  Add `fn graph_tools_available(&self, ctx: &McpAuthContext) -> bool` that delegates to Plan A's free function with the handler's owned fields.
- [ ] **Run-pass.** `cargo nextest run -p cairn-mcp --test graph_tools_manifest -- call_tool`
- [ ] **Commit.**
  ```bash
  git commit -m "$(cat <<'EOF'
  feat(mcp): route graph.* tools through handler::call_tool (#190)

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

## Task 14: Wire `GRAPH_TOOLS` into `handler::list_tools` (gated)

**Files:**
- `crates/cairn-mcp/src/handler.rs` (Modify)
- `crates/cairn-mcp/tests/graph_tools_manifest.rs` (Modify)

Spec: §2.1. Same predicate as `call_tool`. `tools/list` resolves the current request's `McpAuthContext` and either appends the 5 graph tools or omits them entirely.

- [ ] **Write failing test.**
  ```rust
  #[tokio::test(flavor = "current_thread")]
  async fn list_tools_lists_graph_when_resolver_returns_non_empty() {
      let h = build_handler_single_tenant_graph_capable();
      let res = h.list_tools(Default::default(), ctx_with_principal()).await.unwrap();
      assert!(res.tools.iter().any(|t| t.name == "graph.query"));
  }
  ```
- [ ] **Run-fail.** `cargo nextest run -p cairn-mcp --test graph_tools_manifest -- list_tools_lists_graph`
- [ ] **Implement.** In `handler.rs::list_tools`, after assembling the IDL-generated `TOOLS` list:
  ```rust
  let ctx = self.auth_context_from(&request_context)?;
  if self.graph_tools_available(&ctx) {
      for decl in cairn_mcp::graph_tools::GRAPH_TOOLS {
          tools.push(rmcp::model::Tool {
              name: decl.name.into(),
              description: Some(decl.description.into()),
              input_schema: serde_json::from_slice(
                  cairn_mcp::graph_tools::schema_of(decl)
              ).unwrap_or_default(),
              annotations: None,
              ..Default::default()
          });
      }
  }
  ```
- [ ] **Run-pass.** `cargo nextest run -p cairn-mcp --test graph_tools_manifest -- list_tools_lists_graph`
- [ ] **Commit.**
  ```bash
  git commit -m "$(cat <<'EOF'
  feat(mcp): list_tools advertises graph.* iff graph_tools_available (#190)

  Same predicate as call_tool — single source of truth, no drift between
  manifest discovery and dispatch.

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

## Task 15: Flip `mcp_graph_tools_available` to `Available { tool_count: 5 }`

**Files:**
- `crates/cairn-core/src/config/mod.rs` (Modify)

Spec: §6. Plan A landed the enum and stub; this task fills in the `Available` arm.

- [ ] **Write failing test.**
  ```rust
  #[test]
  fn mcp_graph_tools_available_returns_five_when_all_conditions_hold() {
      let cfg = config_with_single_tenant_stdio();
      let store_caps = cairn_core::contract::memory_store::MemoryStoreCapabilities {
          graph_edges: true, ..Default::default()
      };
      let scope = static_scope_resolver(vec![ScopeTuple::default()]);
      let ctx = McpAuthContext { principal: &PrincipalId::for_test(), request_id: &Default::default() };
      let av = cfg.mcp_graph_tools_available(&store_caps, Some(&scope), McpTransport::Stdio, &ctx);
      assert!(matches!(av, McpGraphAvailability::Available { tool_count: 5 }));
  }
  ```
- [ ] **Run-fail.** `cargo nextest run -p cairn-core -- mcp_graph_tools_available_returns_five`
- [ ] **Implement.** Replace Plan A's `Available { tool_count: 0 }` stub:
  ```rust
  // crates/cairn-core/src/config/mod.rs
  impl CairnConfig {
      pub fn mcp_graph_tools_available(
          &self,
          store_caps: &MemoryStoreCapabilities,
          scope: Option<&dyn McpSessionScope>,
          transport: McpTransport,
          ctx: &McpAuthContext<'_>,
      ) -> McpGraphAvailability {
          if transport == McpTransport::Stdio && !self.mcp.stdio.single_tenant {
              return McpGraphAvailability::UnavailableSingleTenantOff;
          }
          if !store_caps.graph_edges {
              return McpGraphAvailability::UnavailableNoStoreCapability;
          }
          let Some(s) = scope else {
              return McpGraphAvailability::UnavailableNoScopeResolver;
          };
          match s.allowed_scopes(ctx) {
              Ok(v) if !v.is_empty() => McpGraphAvailability::Available { tool_count: 5 },
              _ => McpGraphAvailability::UnavailableNoScopeResolver,
          }
      }
  }
  ```
- [ ] **Run-pass.** `cargo nextest run -p cairn-core -- mcp_graph_tools_available_returns_five`
- [ ] **Commit.**
  ```bash
  git commit -m "$(cat <<'EOF'
  feat(core): mcp_graph_tools_available returns Available{5} (#190)

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

## Task 16: Byte-oriented stdio relay — basic blank-line drop, CRLF/LF preserve

**Files:**
- `crates/cairn-mcp/src/relay.rs` (Create)
- `crates/cairn-mcp/src/lib.rs` (Modify — `pub mod relay;`)
- `crates/cairn-mcp/tests/relay.rs` (Create)

Spec: §5.

- [ ] **Write failing test.**
  ```rust
  // crates/cairn-mcp/tests/relay.rs
  use cairn_mcp::relay::run_relay;
  use tokio::io::{AsyncReadExt, AsyncWriteExt};

  #[tokio::test(flavor = "current_thread")]
  async fn relay_drops_blank_lines_and_preserves_crlf() {
      let input: &[u8] = b"\n\r\n{\"a\":1}\r\n\n{\"b\":2}\n";
      let (mut tx, rx) = tokio::io::duplex(8192);
      let (out_tx, mut out_rx) = tokio::io::duplex(8192);
      tokio::spawn(async move { run_relay(rx, out_tx).await.ok(); });
      tx.write_all(input).await.unwrap();
      drop(tx);
      let mut got = Vec::new();
      out_rx.read_to_end(&mut got).await.unwrap();
      assert_eq!(got, b"{\"a\":1}\r\n{\"b\":2}\n");
  }
  ```
- [ ] **Run-fail.** `cargo nextest run -p cairn-mcp --test relay -- relay_drops_blank_lines`
- [ ] **Implement.**
  ```rust
  // crates/cairn-mcp/src/relay.rs
  use thiserror::Error;
  use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

  pub const FRAME_CAP_BYTES: usize = 16 * 1024 * 1024;

  #[derive(Debug, Error)]
  pub enum TransportError {
      #[error("frame exceeded {FRAME_CAP_BYTES}-byte cap without newline")]
      FrameTooLarge,
      #[error("io: {0}")] Io(#[from] std::io::Error),
  }

  pub async fn run_relay<R, W>(mut input: R, mut output: W) -> Result<(), TransportError>
  where R: AsyncRead + Unpin, W: AsyncWrite + Unpin {
      let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
      let mut tmp = [0u8; 8192];
      loop {
          let n = input.read(&mut tmp).await?;
          if n == 0 { break; }
          buf.extend_from_slice(&tmp[..n]);
          while let Some(nl_at) = memchr::memchr(b'\n', &buf) {
              let frame = &buf[..=nl_at];
              if frame_is_blank(frame) {
                  // skip — don't forward
              } else {
                  output.write_all(frame).await?;
              }
              buf.drain(..=nl_at);
          }
          if buf.len() > FRAME_CAP_BYTES {
              return Err(TransportError::FrameTooLarge);
          }
      }
      // EOF tail — see Task 18.
      handle_eof_tail(&buf, &mut output).await?;
      Ok(())
  }

  fn frame_is_blank(bytes: &[u8]) -> bool {
      // Strip trailing \r\n / \n, check whether anything non-whitespace remains.
      let mut end = bytes.len();
      if end > 0 && bytes[end-1] == b'\n' { end -= 1; }
      if end > 0 && bytes[end-1] == b'\r' { end -= 1; }
      bytes[..end].iter().all(|b| matches!(*b, b' ' | b'\t'))
  }

  async fn handle_eof_tail<W: AsyncWrite + Unpin>(_tail: &[u8], _out: &mut W) -> std::io::Result<()> {
      // Filled in Task 18.
      Ok(())
  }
  ```
  ```rust
  // crates/cairn-mcp/src/lib.rs
  pub mod relay;
  ```
- [ ] **Run-pass.** `cargo nextest run -p cairn-mcp --test relay -- relay_drops_blank_lines`
- [ ] **Commit.**
  ```bash
  git commit -m "$(cat <<'EOF'
  feat(mcp): byte-oriented stdio relay drops blank lines, preserves CRLF (#190)

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

## Task 17: Relay 16 MiB cap + `FrameTooLarge`

**Files:**
- `crates/cairn-mcp/src/relay.rs` (Modify)
- `crates/cairn-mcp/tests/relay.rs` (Modify)

Spec: §5.3 invariant 2.

- [ ] **Write failing test.**
  ```rust
  #[tokio::test(flavor = "current_thread")]
  async fn relay_rejects_oversized_frame() {
      let mut input = vec![b'x'; 17 * 1024 * 1024]; // > 16 MiB, no newline
      let (mut tx, rx) = tokio::io::duplex(64 * 1024);
      let (out_tx, _out_rx) = tokio::io::duplex(64 * 1024);
      let h = tokio::spawn(async move { run_relay(rx, out_tx).await });
      tx.write_all(&input).await.ok();
      drop(tx);
      let res = h.await.unwrap();
      assert!(matches!(res, Err(TransportError::FrameTooLarge)));
  }

  #[tokio::test(flavor = "current_thread")]
  async fn relay_passes_8mib_frame_intact() {
      let mut frame = vec![b'x'; 8 * 1024 * 1024];
      frame.push(b'\n');
      let (mut tx, rx) = tokio::io::duplex(64 * 1024);
      let (out_tx, mut out_rx) = tokio::io::duplex(64 * 1024);
      tokio::spawn(async move { run_relay(rx, out_tx).await.ok(); });
      tx.write_all(&frame).await.unwrap();
      drop(tx);
      let mut got = Vec::new();
      out_rx.read_to_end(&mut got).await.unwrap();
      assert_eq!(got.len(), frame.len());
  }
  ```
- [ ] **Run-fail.** `cargo nextest run -p cairn-mcp --test relay -- relay_rejects_oversized`
- [ ] **Implement.** The cap check is already in Task 16's loop; tighten to fail on read past cap:
  ```rust
  if buf.len() > FRAME_CAP_BYTES {
      tracing::error!(
          buffered = buf.len(),
          cap = FRAME_CAP_BYTES,
          "stdio relay frame exceeded 16 MiB cap"
      );
      return Err(TransportError::FrameTooLarge);
  }
  ```
- [ ] **Run-pass.** `cargo nextest run -p cairn-mcp --test relay -- relay_rejects_oversized relay_passes_8mib`
- [ ] **Commit.**
  ```bash
  git commit -m "$(cat <<'EOF'
  feat(mcp): stdio relay enforces 16 MiB frame cap (#190)

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

## Task 18: Relay `decode_eof` tail handling

**Files:**
- `crates/cairn-mcp/src/relay.rs` (Modify)
- `crates/cairn-mcp/tests/relay.rs` (Modify)

Spec: §5.3 EOF tail. Mirror `rmcp::JsonRpcMessageCodec::decode_eof`: if the trailing bytes parse as JSON, forward with synthetic `\n`; otherwise drop with `tracing::warn!`.

- [ ] **Write failing test.**
  ```rust
  #[tokio::test(flavor = "current_thread")]
  async fn relay_eof_with_valid_trailing_frame_appends_newline() {
      let input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}";
      let (mut tx, rx) = tokio::io::duplex(8192);
      let (out_tx, mut out_rx) = tokio::io::duplex(8192);
      tokio::spawn(async move { run_relay(rx, out_tx).await.ok(); });
      tx.write_all(input).await.unwrap();
      drop(tx);
      let mut got = Vec::new();
      out_rx.read_to_end(&mut got).await.unwrap();
      assert_eq!(got, [input.as_slice(), b"\n"].concat());
  }

  #[tokio::test(flavor = "current_thread")]
  async fn relay_eof_with_malformed_trailing_bytes_drops_them() {
      let input = b"{\"jsonrpc\":\"2.0\",";
      let (mut tx, rx) = tokio::io::duplex(8192);
      let (out_tx, mut out_rx) = tokio::io::duplex(8192);
      tokio::spawn(async move { run_relay(rx, out_tx).await.ok(); });
      tx.write_all(input).await.unwrap();
      drop(tx);
      let mut got = Vec::new();
      out_rx.read_to_end(&mut got).await.unwrap();
      assert!(got.is_empty());
  }
  ```
- [ ] **Run-fail.** `cargo nextest run -p cairn-mcp --test relay -- relay_eof`
- [ ] **Implement.** Replace the Task 16 stub:
  ```rust
  async fn handle_eof_tail<W: AsyncWrite + Unpin>(
      tail: &[u8],
      out: &mut W,
  ) -> std::io::Result<()> {
      if tail.is_empty() { return Ok(()); }
      let trimmed = trim_ascii_ws(tail);
      if trimmed.is_empty() { return Ok(()); }
      match serde_json::from_slice::<serde_json::Value>(trimmed) {
          Ok(_) => {
              out.write_all(tail).await?;
              out.write_all(b"\n").await?;
          }
          Err(e) => {
              tracing::warn!(
                  bytes = tail.len(),
                  error = %e,
                  "stdio relay: dropping malformed EOF tail"
              );
          }
      }
      Ok(())
  }

  fn trim_ascii_ws(b: &[u8]) -> &[u8] {
      let mut start = 0;
      let mut end = b.len();
      while start < end && b[start].is_ascii_whitespace() { start += 1; }
      while end > start && b[end-1].is_ascii_whitespace() { end -= 1; }
      &b[start..end]
  }
  ```
- [ ] **Run-pass.** `cargo nextest run -p cairn-mcp --test relay -- relay_eof`
- [ ] **Commit.**
  ```bash
  git commit -m "$(cat <<'EOF'
  feat(mcp): stdio relay decode_eof preserves valid trailing frames (#190)

  Mirrors rmcp::JsonRpcMessageCodec::decode_eof — a client that writes
  one final request and closes stdin without a newline does not lose the
  request, while malformed trailing bytes are dropped with a warn log.

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

## Task 19: Wire relay into `serve_stdio_with_store`

**Files:**
- `crates/cairn-mcp/src/lib.rs` (Modify)

Plan A's `serve_stdio_with_store(store, scope, config, principal)` exists; today it hands `tokio::io::stdin()` straight to rmcp. Insert the relay between them.

- [ ] **Write failing test.** (Already covered by Tasks 16/17/18; no new test, but add an integration smoke test that constructs the server and verifies it reads from a piped duplex.) Skip dedicated test — integration covered downstream.
- [ ] **Run-fail.** `cargo check -p cairn-mcp --locked`
- [ ] **Implement.**
  ```rust
  pub async fn serve_stdio_with_store(
      store: Arc<SqliteMemoryStore>,
      scope: Option<Arc<dyn McpSessionScope>>,
      config: CairnConfig,
      principal: Option<PrincipalId>,
  ) -> anyhow::Result<()> {
      let (relay_writer, framer_reader) = tokio::io::duplex(64 * 1024);
      tokio::spawn(async move {
          if let Err(e) = relay::run_relay(tokio::io::stdin(), relay_writer).await {
              tracing::error!(error = %e, "stdio relay terminated");
          }
      });
      let server = build_server(store, scope, config, principal);
      let stdout = tokio::io::stdout();
      // rmcp serve over (framer_reader, stdout)
      rmcp::serve_server(server, (framer_reader, stdout)).await?;
      Ok(())
  }
  ```
- [ ] **Run-pass.** `cargo check -p cairn-mcp --locked && cargo nextest run -p cairn-mcp --locked`
- [ ] **Commit.**
  ```bash
  git commit -m "$(cat <<'EOF'
  feat(mcp): serve_stdio_with_store routes stdin through blank-line relay (#190)

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

## Task 20: Manifest snapshot test (insta) — full transport × single_tenant × store × resolver matrix

**Files:**
- `crates/cairn-mcp/tests/graph_tools_manifest.rs` (Modify)

Spec: §2.1 matrix (six rows). Six snapshots; only the bottom row contains `graph.*` entries.

- [ ] **Write failing test.**
  ```rust
  #[tokio::test(flavor = "current_thread")]
  async fn manifest_matrix_stdio_single_tenant_off() {
      let h = build_handler(MatrixCell {
          transport: McpTransport::Stdio,
          single_tenant: false, graph_edges: true,
          resolver: ResolverOutcome::OkNonEmpty,
      });
      let res = h.list_tools(Default::default(), ctx_with_principal()).await.unwrap();
      insta::assert_json_snapshot!("manifest_stdio_single_tenant_off",
          res.tools.iter().map(|t| &t.name).collect::<Vec<_>>());
  }
  // ... five more cells per the §2.1 table
  ```
- [ ] **Run-fail.** `cargo nextest run -p cairn-mcp --test graph_tools_manifest -- manifest_matrix`
- [ ] **Implement.** Add helper enums:
  ```rust
  enum ResolverOutcome { Absent, Err, OkEmpty, OkNonEmpty }
  struct MatrixCell {
      transport: McpTransport,
      single_tenant: bool,
      graph_edges: bool,
      resolver: ResolverOutcome,
  }
  ```
  Six tests, one per row, each reviewed once via `cargo insta review` and committed under `crates/cairn-mcp/tests/snapshots/`.
- [ ] **Run-pass.** `cargo nextest run -p cairn-mcp --test graph_tools_manifest -- manifest_matrix`
- [ ] **Commit.**
  ```bash
  git commit -m "$(cat <<'EOF'
  test(mcp): manifest snapshot covers 6-state graph-tools availability matrix (#190)

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

## Task 21: Cross-tool adversarial tests

**Files:**
- `crates/cairn-store-sqlite/tests/graph_queries.rs` (Modify)
- `crates/cairn-mcp/tests/graph_tools_manifest.rs` (Modify)

Spec covers: tombstoned-endpoint (§3.0a-bis test 6), lineage re-scope (§3.0a-bis test 5, §3.4 test 7), future-only provenance (§3.1 test 4), episode-via-tombstoned-record (§3.1 test 5), `ScopeTuple` no-wildcard (§3.0a-bis test 3).

- [ ] **Write failing test.**
  ```rust
  #[tokio::test(flavor = "current_thread")]
  async fn tombstoned_endpoint_hidden_from_all_five_tools() {
      let f = endpoint_tombstone_fixture().await;
      let q = GraphQueries::new(f.store.clone(), vec![f.scope_a.clone()], f.now);
      assert!(q.get_neighbors(&f.node_a, None, None).unwrap().is_empty());
      assert!(q.timeline(&f.node_a, true, true).unwrap().is_empty());
      let bfs = q.query_bfs(&f.node_a, 5, 64).unwrap();
      assert!(bfs.nodes.iter().all(|n| n.id != f.tombstoned_b));
      assert!(q.surprising_connections(&[f.node_a.clone(), f.tombstoned_b.clone()], 16).unwrap().is_empty());
      assert!(q.get_entity_by_id(&f.tombstoned_b).unwrap().is_none());
  }

  #[tokio::test(flavor = "current_thread")]
  async fn lineage_rescope_keys_off_immutable_provenance() {
      let f = lineage_rescope_fixture().await; // S_a original, S_b active head
      let q_a = GraphQueries::new(f.store.clone(), vec![f.scope_a.clone()], f.now);
      assert!(!q_a.get_neighbors(&f.node_a, None, None).unwrap().is_empty()); // S_a sees the edge
      let q_b = GraphQueries::new(f.store.clone(), vec![f.scope_b.clone()], f.now);
      assert!(q_b.get_neighbors(&f.node_a, None, None).unwrap().is_empty());  // S_b does not
  }

  #[tokio::test(flavor = "current_thread")]
  async fn future_only_provenance_is_not_found_until_clock_advances() {
      let f = future_provenance_fixture().await;
      let q = GraphQueries::new(f.store.clone(), vec![f.scope_a.clone()], f.now);
      assert!(q.get_entity_by_id(&f.node_future).unwrap().is_none());
      let q2 = GraphQueries::new(f.store.clone(), vec![f.scope_a.clone()], f.future_valid_at + 1);
      assert!(q2.get_entity_by_id(&f.node_future).unwrap().is_some());
  }

  #[tokio::test(flavor = "current_thread")]
  async fn episode_via_tombstoned_record_hides_entity() {
      let f = episode_tombstone_fixture().await;
      let q = GraphQueries::new(f.store.clone(), vec![f.scope_a.clone()], f.now);
      assert!(q.get_entity_by_id(&f.node_via_episode).unwrap().is_none());
  }

  #[tokio::test(flavor = "current_thread")]
  async fn scope_tuple_has_no_wildcard_user_dimension() {
      // tenant=Some("a"), user=None matches ONLY records with user IS NULL.
      let f = scope_user_fixture().await;
      let scope = ScopeTuple { tenant: Some("a".into()), user: None, ..Default::default() };
      let q = GraphQueries::new(f.store.clone(), vec![scope], f.now);
      // Edge sourced from a record with user="bob" must NOT be visible.
      let edges = q.get_neighbors(&f.node_a, None, None).unwrap();
      assert!(edges.iter().all(|e| e.id != f.edge_with_user_bob));
  }
  ```
- [ ] **Run-fail.** `cargo nextest run -p cairn-store-sqlite --test graph_queries -- tombstoned_endpoint lineage_rescope future_only_provenance episode_via_tombstoned scope_tuple_no_wildcard`
- [ ] **Implement.** Each fixture lives in `cairn-test-fixtures::graph` (dev-dep). Add helpers there if needed (no production code change for this task; the queries from Tasks 1-10 already enforce the invariants).
- [ ] **Run-pass.** Same nextest invocation.
- [ ] **Commit.**
  ```bash
  git commit -m "$(cat <<'EOF'
  test(store-sqlite): adversarial cross-tool tests for graph authorization (#190)

  Locks in the §3.0a-bis (immutable provenance, no wildcard) and §2.1.0
  (no node-level cross-scope leak) invariants against the SQL.

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

## Task 22: Final verification

**Files:** none modified.

Run the CLAUDE.md §8 checklist before opening the PR:

- [ ] **fmt + clippy + check + nextest.**
  ```bash
  cargo fmt --all --check
  cargo clippy --workspace --all-targets --locked -- -D warnings
  cargo check --workspace --all-targets --locked
  cargo nextest run --workspace --locked --no-fail-fast
  cargo test --doc --workspace --locked
  ./scripts/check-core-boundary.sh
  ```
- [ ] **Codegen check.**
  ```bash
  cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
  ```
- [ ] **Docgen.** Five new MCP tool descriptions land in `docs/site/src/reference/generated/`. Re-run docgen and commit:
  ```bash
  cargo run -p cairn-cli --bin cairn-docgen --locked -- --write
  cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
  mdbook build docs/site
  ```
- [ ] **Supply chain.**
  ```bash
  cargo deny check
  cargo audit --deny warnings
  cargo machete
  ```
- [ ] **Commit any docgen output.**
  ```bash
  git add docs/site/src/reference/generated/
  git commit -m "$(cat <<'EOF'
  docs(mcp): regenerate manifest reference for graph tools (#190)

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

## Invariants touched

- **§4 Seven contracts.** No new contract — `GraphQueries` is a struct in the existing `cairn-store-sqlite` crate; `McpSessionScope` is from Plan A.
- **§5 WAL + two-phase apply.** Untouched (read-only path).
- **§6 Fail closed on capability.** Plan A's `graph_tools_available` predicate is now exercised by both `list_tools` and `call_tool`. `mcp_graph_tools_available` returns `Available { tool_count: 5 }` only when every condition holds.
- **§9 Privacy by construction.** Discovered nodes are id-only; `entity_nodes.name` and `summary` never leave the store. `tracing::warn!` calls in the relay carry byte counts only, never frame contents.
- **§10 Vault layers.** No vault writes — graph queries are pure reads.
