use cairn_core::domain::graph::normalize::normalize_entity_name;
use cairn_core::domain::scope::ScopeTuple;
use indexmap::IndexMap;
use rusqlite::{params_from_iter, types::Value as SqlValue};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;

use serde_json;

use crate::error::StoreError;
use crate::store::SqliteMemoryStore;

/// A single directed edge between two entities, returned by graph traversal
/// queries. All fields are id-only per §2.1.0 — no cross-scope name columns.
#[derive(Serialize)]
pub struct GraphEdge {
    /// The edge's canonical id.
    pub id: String,
    /// The source entity id.
    pub source_id: String,
    /// The target entity id.
    pub target_id: String,
    /// The relation label (e.g. `"calls"`, `"depends_on"`).
    pub relation: String,
    /// Confidence score in `[0.0, 1.0]`.
    pub confidence_score: f64,
    /// Epoch-millisecond timestamp when the edge became valid.
    pub valid_at: i64,
}

/// A node returned by a graph traversal query (§3.3).
#[derive(Serialize)]
pub struct GraphNode {
    /// The entity's canonical id.
    pub id: String,
}

/// A surprising-connection hit: an edge scored by cross-scope bonus (§3.5).
#[derive(Serialize)]
pub struct SurpriseHit {
    /// The edge that was scored.
    pub edge: GraphEdge,
    /// Provenance record id (the immutable `r_src` lineage from
    /// §3.0a-bis) that this edge was authored against. Surfaced on
    /// `SurpriseHit` rather than `GraphEdge` because the public
    /// edge type is id-only (§3.1) and reused by other tools that
    /// must not leak provenance.
    pub source_record_id: String,
    /// The `records.scope` JSON string of the record that authored this edge.
    pub scope_id: String,
    /// Score: `confidence_score * (1 + cross_scope_bonus)`.
    pub score: f64,
}

/// A traversal result set containing ordered nodes and edges (§3.3).
pub struct GraphSubgraph {
    /// Nodes in traversal order (seed first).
    pub nodes: Vec<GraphNode>,
    /// Edges discovered during traversal.
    pub edges: Vec<GraphEdge>,
    /// Maps each discovered node id to the edge id that first reached it.
    /// The seed is absent (it has no parent edge).
    pub parent_of: HashMap<String, String>,
    /// Maps each discovered node id to its BFS depth (seed = 0).
    pub depth_of: HashMap<String, u32>,
}

/// An entity returned by one of the `get_entity_*` query arms.
///
/// Per §2.1.0: only `id` and an in-scope edge count are ever returned;
/// `name` and `summary` are cross-scope columns and must not be echoed
/// back from the DB. `echoed_name` carries the caller's literal input
/// string when the `ByName` arm was used — it is never read from the DB.
#[derive(Serialize)]
pub struct EntityHit {
    /// The entity's canonical id.
    pub id: String,
    /// The literal name string the caller passed in (`ByName` arm only).
    /// Always `None` from the `ById` arm.
    pub echoed_name: Option<String>,
    /// Count of live edges where this entity is source or target,
    /// scoped to the caller's `allowed_scopes`.
    pub edge_count: i64,
}

/// A single edge entry in the timeline view for a given seed entity (§3.4).
#[derive(Serialize)]
pub struct TimelineEntry {
    /// The edge's canonical id.
    pub id: String,
    /// The source entity id.
    pub source_id: String,
    /// The target entity id.
    pub target_id: String,
    /// The relation label.
    pub relation: String,
    /// Confidence score in `[0.0, 1.0]`.
    pub confidence_score: f64,
    /// Epoch-millisecond timestamp when the edge became valid.
    pub valid_at: i64,
    /// Epoch-millisecond timestamp when the edge became invalid, if any.
    pub invalid_at: Option<i64>,
    /// Epoch-millisecond timestamp when the edge was created.
    pub created_at: i64,
    /// Epoch-millisecond timestamp when the edge was expired (tombstoned), if any.
    pub expired_at: Option<i64>,
    /// Human-readable reason the edge was tombstoned, if any.
    pub tombstone_reason: Option<String>,
    /// The record that authored this edge, if any.
    pub source_record_id: Option<String>,
}

/// Bind-slot accounting for the CTE prefix. Callers bind in this
/// order: `now` (×`now_count`), then scope tuple dimensions
/// (×`scope_count`, in tuple-major / dimension-minor order matching
/// `scope_match_clause`).
pub struct CtePrefixBinds {
    /// Number of `?` slots bound to the `now` timestamp.
    pub now_count: usize,
    /// Number of `?` slots bound to scope-tuple dimensions.
    pub scope_count: usize,
}

/// Read-only graph-traversal driver. Constructed per-request after
/// the MCP handler has resolved the caller's scope set; every
/// method bakes both the §3.0 active-at-now temporal predicate
/// and the §3.0a stable-lineage scope predicate into its SQL.
///
/// `allowed_scopes` is the resolver's `Vec<ScopeTuple>` and MUST be
/// non-empty — empty means deny-all and is the caller's
/// responsibility to short-circuit upstream (`materialize_graph_request`).
pub struct GraphQueries {
    // Fields consumed by Tasks 3–10; unused in this skeleton task.
    #[allow(dead_code)]
    pub(crate) store: Arc<SqliteMemoryStore>,
    #[allow(dead_code)]
    pub(crate) allowed_scopes: Vec<ScopeTuple>,
    #[allow(dead_code)]
    pub(crate) now: i64,
}

impl GraphQueries {
    /// Construct a new [`GraphQueries`] driver.
    ///
    /// # Panics (debug only)
    ///
    /// Panics in debug builds when `allowed_scopes` is empty.
    /// Production callers must short-circuit upstream via
    /// `materialize_graph_request` before reaching this constructor.
    #[must_use]
    pub fn new(store: Arc<SqliteMemoryStore>, allowed_scopes: Vec<ScopeTuple>, now: i64) -> Self {
        debug_assert!(
            !allowed_scopes.is_empty(),
            "invariant: GraphQueries requires non-empty allowed_scopes; \
             materialize_graph_request must short-circuit upstream"
        );
        Self {
            store,
            allowed_scopes,
            now,
        }
    }

    /// Render the §3.0a six-dimension scope match clause.
    ///
    /// Returns `(sql, bind_count)` where `sql` is intended to drop into
    /// an EXISTS subquery as the `<ScopeTuple-match-clause>` placeholder
    /// in the spec, and `bind_count` is the number of `?` parameters the
    /// caller must bind in order (`tenant`, `workspace`, `session_id`, `entity`,
    /// `user`, `agent` — six per tuple, OR'd between tuples).
    #[must_use]
    pub fn scope_match_clause(tuples: &[ScopeTuple]) -> (String, usize) {
        const DIMS: [&str; 6] = [
            "tenant",
            "workspace",
            "session_id",
            "entity",
            "user",
            "agent",
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

    /// Build the canonical §3.0 CTE prefix used by every public
    /// query. The returned SQL ends with a trailing comma so the
    /// caller appends their own SELECT-feeding CTEs or trailing
    /// statement directly.
    ///
    /// Bind order: `now` (×[`CtePrefixBinds::now_count`]), then scope
    /// tuple dimensions (×[`CtePrefixBinds::scope_count`]) in
    /// tuple-major / dimension-minor order matching `scope_match_clause`.
    #[must_use]
    pub fn cte_prefix(tuples: &[ScopeTuple]) -> (String, CtePrefixBinds) {
        let (scope_clause, scope_per_block) = Self::scope_match_clause(tuples);
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
             )"
        .to_string();
        let sql = format!("WITH {raw}, {nodes}, {edges},");
        (
            sql,
            CtePrefixBinds {
                // visible_edges_raw: valid_at <= ? AND invalid_at > ?  → 2 :now
                // past-invalidated branch: valid_at <= ? AND invalid_at <= ? → 2 :now
                // Total :now = 4.
                now_count: 4,
                // Three scope blocks (raw, episode, past-invalid) × scope_per_block each.
                scope_count: scope_per_block * 3,
            },
        )
    }

    /// Expand a `len`-wide `IN (?,?,...)` placeholder list. `len = 0`
    /// returns `"(NULL)"` so the caller may unconditionally splice it
    /// into SQL — a `NULL` membership test is always false, matching
    /// the spec's "empty visited / empty input → omit clause" guidance
    /// without runtime branching at the SQL level.
    ///
    /// Used by Tasks 3–10; unused in this skeleton task.
    #[allow(dead_code)]
    pub(crate) fn placeholders(len: usize) -> String {
        if len == 0 {
            return "(NULL)".to_string();
        }
        let mut s = String::with_capacity(len * 2 + 1);
        s.push('(');
        for i in 0..len {
            if i > 0 {
                s.push(',');
            }
            s.push('?');
        }
        s.push(')');
        s
    }

    /// Push bind parameters for the CTE prefix in the exact order the SQL
    /// placeholder `?` slots appear in the output of [`Self::cte_prefix`].
    ///
    /// The SQL placeholder order is:
    ///
    /// 1. `now` (`valid_at` <=)          — `visible_edges_raw`
    /// 2. `now` (`invalid_at` >)         — `visible_edges_raw`
    /// 3. scope-block-1 × 6              — `visible_edges_raw` EXISTS scope clause
    /// 4. scope-block-2 × 6              — `visible_nodes` episode arm scope clause
    /// 5. `now` (`valid_at` <=)          — `visible_nodes` past-invalidated arm
    /// 6. `now` (`invalid_at` <=)        — `visible_nodes` past-invalidated arm
    /// 7. scope-block-3 × 6              — `visible_nodes` past-invalidated EXISTS scope clause
    pub(crate) fn push_prefix_binds(&self, binds: &mut Vec<SqlValue>) {
        // Block 1: visible_edges_raw temporal + scope
        binds.push(SqlValue::Integer(self.now)); // valid_at <= ?
        binds.push(SqlValue::Integer(self.now)); // invalid_at > ?
        self.push_scope_block(binds);

        // Block 2: visible_nodes episode arm scope (no now binds here)
        self.push_scope_block(binds);

        // Block 3: visible_nodes past-invalidated arm temporal + scope
        binds.push(SqlValue::Integer(self.now)); // valid_at <= ?
        binds.push(SqlValue::Integer(self.now)); // invalid_at <= ?
        self.push_scope_block(binds);
    }

    /// Push one full tuple-major scope-block expansion. Each tuple emits
    /// six dimension values in the canonical order matching `scope_match_clause`.
    fn push_scope_block(&self, binds: &mut Vec<SqlValue>) {
        for tup in &self.allowed_scopes {
            for (_, v) in tup.dimension_iter() {
                binds.push(match v {
                    Some(s) => SqlValue::Text(s.to_string()),
                    None => SqlValue::Null,
                });
            }
        }
    }

    /// One-hop neighbor edges for `seed`, with optional relation and
    /// confidence filters (§3.2).
    ///
    /// Returns every visible edge where `seed` is source or target, filtered
    /// to `relation` when supplied and to `confidence_score >= min_confidence`
    /// when supplied. `NULL` parameters are treated as "match all".
    pub async fn get_neighbors(
        &self,
        seed: String,
        relation: Option<String>,
        min_confidence: Option<f64>,
    ) -> Result<Vec<GraphEdge>, StoreError> {
        let (prefix, _binds) = Self::cte_prefix(&self.allowed_scopes);
        let sql = format!(
            "{prefix}
             neighbors AS (
               SELECT e.id, e.source_id, e.target_id, e.relation,
                      e.confidence_score, e.valid_at
               FROM visible_edges e
               WHERE (e.source_id = ? OR e.target_id = ?)
                 AND (?  IS NULL OR e.relation = ?)
                 AND (?  IS NULL OR e.confidence_score >= ?)
             )
             SELECT id, source_id, target_id, relation,
                    confidence_score, valid_at
             FROM neighbors"
        );
        let mut binds: Vec<SqlValue> = Vec::new();
        self.push_prefix_binds(&mut binds);
        binds.push(SqlValue::Text(seed.clone()));
        binds.push(SqlValue::Text(seed));
        let rel_param = relation.map_or(SqlValue::Null, SqlValue::Text);
        binds.push(rel_param.clone());
        binds.push(rel_param);
        let conf_param = min_confidence.map_or(SqlValue::Null, SqlValue::Real);
        binds.push(conf_param.clone());
        binds.push(conf_param);

        let conn = self.store.read_conn()?;
        let out = conn
            .call(move |c| {
                let mut stmt = c.prepare(&sql)?;
                let mut rows = stmt.query(params_from_iter(binds))?;
                let mut out = Vec::new();
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
                Ok::<_, tokio_rusqlite::Error>(out)
            })
            .await?;
        Ok(out)
    }

    /// BFS traversal from `seed` up to `max_hops` hops, capped at
    /// `node_budget` nodes (§3.3).
    ///
    /// Returns a [`GraphSubgraph`] with nodes in BFS insertion order (seed
    /// first), edges for each newly-reached node, and `depth_of` /
    /// `parent_of` maps for downstream reordering.
    pub async fn query_bfs(
        &self,
        seed: String,
        max_hops: u32,
        node_budget: usize,
    ) -> Result<GraphSubgraph, StoreError> {
        let mut visited: IndexMap<String, GraphNode> = IndexMap::new();
        let mut edges: Vec<GraphEdge> = Vec::new();
        let mut parent_of: HashMap<String, String> = HashMap::new();
        let mut depth_of: HashMap<String, u32> = HashMap::new();

        // Seed visibility check — falls through visible_nodes.
        let Some(seed_hit) = self.get_entity_by_id(seed.clone()).await? else {
            return Ok(GraphSubgraph {
                nodes: vec![],
                edges: vec![],
                parent_of,
                depth_of,
            });
        };
        visited.insert(
            seed_hit.id.clone(),
            GraphNode {
                id: seed_hit.id.clone(),
            },
        );
        depth_of.insert(seed_hit.id.clone(), 0);

        let mut frontier: Vec<String> = vec![seed];
        for depth in 1..=max_hops {
            if frontier.is_empty() {
                break;
            }
            if visited.len() >= node_budget {
                break;
            }
            let wave_cap = node_budget - visited.len();
            let visited_ids: Vec<String> = visited.keys().cloned().collect();
            let wave = self
                .bfs_wave_sql(frontier.clone(), visited_ids, wave_cap)
                .await?;
            let mut next_frontier = Vec::new();
            for e in wave {
                if visited.len() >= node_budget {
                    break;
                }
                let other = if frontier.iter().any(|f| f == &e.source_id) {
                    e.target_id.clone()
                } else {
                    e.source_id.clone()
                };
                if visited.contains_key(&other) {
                    continue;
                }
                visited.insert(other.clone(), GraphNode { id: other.clone() });
                parent_of.insert(other.clone(), e.id.clone());
                depth_of.insert(other.clone(), depth);
                edges.push(e);
                next_frontier.push(other);
            }
            frontier = next_frontier;
        }
        Ok(GraphSubgraph {
            nodes: visited.into_values().collect(),
            edges,
            parent_of,
            depth_of,
        })
    }

    /// Execute one BFS wave: find all edges incident to `frontier` that lead
    /// to nodes not yet in `visited`, applying a `ROW_NUMBER()` window to
    /// pick the highest-confidence edge per new neighbor. Results are capped
    /// at `wave_cap`.
    async fn bfs_wave_sql(
        &self,
        frontier: Vec<String>,
        visited: Vec<String>,
        wave_cap: usize,
    ) -> Result<Vec<GraphEdge>, StoreError> {
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
        let mut binds: Vec<SqlValue> = Vec::new();
        self.push_prefix_binds(&mut binds);
        // frontier appears 3× in SQL above
        for _ in 0..3 {
            for f in &frontier {
                binds.push(SqlValue::Text(f.clone()));
            }
        }
        for v in &visited {
            binds.push(SqlValue::Text(v.clone()));
        }
        binds.push(SqlValue::Integer(
            i64::try_from(wave_cap).unwrap_or(i64::MAX),
        ));

        let conn = self.store.read_conn()?;
        let out = conn
            .call(move |c| {
                let mut stmt = c.prepare(&sql)?;
                let mut rows = stmt.query(params_from_iter(binds))?;
                let mut out = Vec::new();
                while let Some(row) = rows.next()? {
                    out.push(GraphEdge {
                        id: row.get(0)?,
                        source_id: row.get(1)?,
                        target_id: row.get(2)?,
                        relation: row.get(3)?,
                        confidence_score: row.get(4)?,
                        valid_at: row.get(5)?,
                    });
                    if out.len() >= wave_cap {
                        break;
                    }
                }
                Ok::<_, tokio_rusqlite::Error>(out)
            })
            .await?;
        Ok(out)
    }

    /// DFS traversal reusing BFS edges, reordered via `parent_of` (§3.3
    /// property 5).
    ///
    /// Collects the same edge set as [`Self::query_bfs`] but emits nodes in
    /// depth-first pre-order by walking the `parent_of` tree iteratively.
    pub async fn query_dfs(
        &self,
        seed: String,
        max_hops: u32,
        node_budget: usize,
    ) -> Result<GraphSubgraph, StoreError> {
        let bfs = self.query_bfs(seed.clone(), max_hops, node_budget).await?;
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
        // Stable child ordering: BFS insertion order of the children themselves.
        let bfs_order: HashMap<&str, usize> = bfs
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.as_str(), i))
            .collect();
        for kids in children.values_mut() {
            kids.sort_by_key(|c| bfs_order.get(c.as_str()).copied().unwrap_or(usize::MAX));
        }
        let mut order: Vec<GraphNode> = Vec::with_capacity(bfs.nodes.len());
        let mut stack: Vec<String> = vec![seed];
        let mut emitted: std::collections::HashSet<String> = std::collections::HashSet::new();
        while let Some(top) = stack.pop() {
            if !emitted.insert(top.clone()) {
                continue;
            }
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
            depth_of: bfs.depth_of,
        })
    }

    /// Id-only entity lookup with an in-scope edge count (§2.1.0, §3.1).
    ///
    /// Returns `None` when the entity is unknown or not visible under
    /// `allowed_scopes` — the §2.1.0 anti-leak contract.
    pub async fn get_entity_by_id(&self, id: String) -> Result<Option<EntityHit>, StoreError> {
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
        let mut binds: Vec<SqlValue> = Vec::new();
        self.push_prefix_binds(&mut binds);
        binds.push(SqlValue::Text(id.clone()));
        binds.push(SqlValue::Text(id.clone()));
        binds.push(SqlValue::Text(id));

        let conn = self.store.read_conn()?;
        let hit = conn
            .call(move |c| {
                let mut stmt = c.prepare(&sql)?;
                let mut rows = stmt.query(params_from_iter(binds))?;
                let result = if let Some(row) = rows.next()? {
                    Some(EntityHit {
                        id: row.get(0)?,
                        echoed_name: None,
                        edge_count: row.get(1)?,
                    })
                } else {
                    None
                };
                Ok::<_, tokio_rusqlite::Error>(result)
            })
            .await?;
        Ok(hit)
    }

    /// Name-based entity lookup with an in-scope edge count (§2.1.0, §3.1).
    ///
    /// Normalizes `name` via [`normalize_entity_name`] and probes
    /// `entity_nodes.name_norm` directly. Returns `None` when no entity
    /// with that normalized name is visible under `allowed_scopes`.
    ///
    /// `echoed_name` in the returned [`EntityHit`] carries the caller's
    /// literal input string — never the canonical DB `name` column, which
    /// is a cross-scope field per §2.1.0.
    pub async fn get_entity_by_name(&self, name: String) -> Result<Option<EntityHit>, StoreError> {
        let norm = normalize_entity_name(&name);
        let (prefix, _binds) = Self::cte_prefix(&self.allowed_scopes);
        // The cte_prefix ends with a trailing comma, so we must append at
        // least one more CTE before the final SELECT. Use a named scalar CTE
        // for the name_norm lookup so the prefix's comma is consumed cleanly.
        let sql = format!(
            "{prefix}
             seed AS (
               SELECT v.id AS eid
               FROM visible_nodes v
               WHERE v.name_norm = ?
               LIMIT 1
             )
             SELECT s.eid,
               (SELECT COUNT(*) FROM visible_edges e
                  WHERE e.source_id = s.eid OR e.target_id = s.eid)
             FROM seed s"
        );
        let mut binds: Vec<SqlValue> = Vec::new();
        self.push_prefix_binds(&mut binds);
        binds.push(SqlValue::Text(norm));

        let conn = self.store.read_conn()?;
        // Async wrap (canonical pattern from Task 3): owned binds and SQL
        // are moved into the DB-thread closure; closure returns owned
        // DTOs. `name` is moved in too so we can echo the caller's
        // literal input on the way out.
        let hit = conn
            .call(move |c| {
                let mut stmt = c.prepare(&sql)?;
                let mut rows = stmt.query(params_from_iter(binds))?;
                let result = if let Some(row) = rows.next()? {
                    Some(EntityHit {
                        id: row.get(0)?,
                        // §2.1.0: echo the caller's literal input,
                        // never the canonical row name.
                        echoed_name: Some(name),
                        edge_count: row.get(1)?,
                    })
                } else {
                    None
                };
                Ok::<_, tokio_rusqlite::Error>(result)
            })
            .await?;
        Ok(hit)
    }

    /// Timeline of edges incident to `seed`, with optional history and expired
    /// inclusion flags (§3.4).
    ///
    /// Default (`include_history = false, include_expired = false`) returns only
    /// edges whose temporal window contains `now` and that have not been expired.
    /// Setting `include_history = true` lifts the temporal predicate so
    /// future-dated edges are also returned. Setting `include_expired = true`
    /// lifts the `expired_at IS NULL` predicate.
    ///
    /// Scope filter and endpoint liveness (`visible_nodes`) are always applied.
    pub async fn timeline(
        &self,
        seed: String,
        include_history: bool,
        include_expired: bool,
    ) -> Result<Vec<TimelineEntry>, StoreError> {
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
        let mut binds: Vec<SqlValue> = Vec::new();
        self.push_prefix_binds(&mut binds);
        // scope_filtered CTE uses one extra block of scope params.
        for tup in &self.allowed_scopes {
            for (_, v) in tup.dimension_iter() {
                binds.push(match v {
                    Some(s) => SqlValue::Text(s.to_string()),
                    None => SqlValue::Null,
                });
            }
        }
        binds.push(SqlValue::Text(seed.clone()));
        binds.push(SqlValue::Text(seed));
        binds.push(SqlValue::Integer(i64::from(include_expired)));
        binds.push(SqlValue::Integer(i64::from(include_history)));
        binds.push(SqlValue::Integer(self.now));
        binds.push(SqlValue::Integer(self.now));

        let conn = self.store.read_conn()?;
        // Async wrap (Task 3 canonical pattern).
        let out = conn
            .call(move |c| {
                let mut stmt = c.prepare(&sql)?;
                let mut rows = stmt.query(params_from_iter(binds))?;
                let mut out = Vec::new();
                while let Some(row) = rows.next()? {
                    out.push(TimelineEntry {
                        id: row.get(0)?,
                        source_id: row.get(1)?,
                        target_id: row.get(2)?,
                        relation: row.get(3)?,
                        confidence_score: row.get(4)?,
                        valid_at: row.get(5)?,
                        invalid_at: row.get(6)?,
                        created_at: row.get(7)?,
                        expired_at: row.get(8)?,
                        tombstone_reason: row.get(9)?,
                        source_record_id: row.get(10)?,
                    });
                }
                Ok::<_, tokio_rusqlite::Error>(out)
            })
            .await?;
        Ok(out)
    }

    /// Score and rank edges among `input` nodes by a cross-scope bonus (§3.5).
    ///
    /// The modal scope (most-frequent `records.scope` among edges in the
    /// input subgraph) is determined by a CTE; edges whose author record has
    /// a different scope receive a 1× confidence bonus. Returns the top
    /// `limit` hits ordered by score descending.
    pub async fn surprising_connections(
        &self,
        input: Vec<String>,
        limit: usize,
    ) -> Result<Vec<SurpriseHit>, StoreError> {
        if input.is_empty() {
            return Ok(vec![]);
        }
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
                    valid_at, source_record_id, scope_id, score
             FROM scored
             ORDER BY score DESC, edge_id ASC
             LIMIT ?"
        );
        let mut binds: Vec<SqlValue> = Vec::new();
        self.push_prefix_binds(&mut binds);
        let json_array = serde_json::to_string(&input).unwrap_or_else(|_| "[]".to_string());
        binds.push(SqlValue::Text(json_array));
        for v in &input {
            binds.push(SqlValue::Text(v.clone()));
        }
        for v in &input {
            binds.push(SqlValue::Text(v.clone()));
        }
        binds.push(SqlValue::Integer(i64::try_from(limit).unwrap_or(i64::MAX)));

        let conn = self.store.read_conn()?;
        // Async wrap (Task 3 canonical pattern).
        let out = conn
            .call(move |c| {
                let mut stmt = c.prepare(&sql)?;
                let mut rows = stmt.query(params_from_iter(binds))?;
                let mut out = Vec::new();
                while let Some(row) = rows.next()? {
                    out.push(SurpriseHit {
                        edge: GraphEdge {
                            id: row.get(0)?,
                            source_id: row.get(1)?,
                            target_id: row.get(2)?,
                            relation: row.get(3)?,
                            confidence_score: row.get(4)?,
                            valid_at: row.get(5)?,
                        },
                        source_record_id: row.get(6)?,
                        scope_id: row.get(7)?,
                        score: row.get(8)?,
                    });
                }
                Ok::<_, tokio_rusqlite::Error>(out)
            })
            .await?;
        Ok(out)
    }

    /// Truncate a slice of serializable rows to fit within `byte_budget`
    /// bytes when JSON-serialized (§3.3 property 6).
    ///
    /// Each row is serialized individually to estimate its byte cost. The
    /// accumulator stops adding rows once the running total would exceed
    /// `byte_budget`, with a single-element overshoot tolerance (the last
    /// item that pushes the total over the budget is still included).
    /// Returns an empty `Vec` when `rows` is empty.
    pub fn truncate_to_token_budget<T: Serialize>(rows: &[T], byte_budget: usize) -> Vec<&T> {
        let mut out: Vec<&T> = Vec::new();
        let mut used = 2usize; // "[]"
        for r in rows {
            let Ok(s) = serde_json::to_string(r) else {
                continue;
            };
            let cost = s.len() + 1; // comma
            if used + cost > byte_budget && !out.is_empty() {
                break;
            }
            out.push(r);
            used += cost;
        }
        out
    }
}
