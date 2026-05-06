use cairn_core::domain::scope::ScopeTuple;
use rusqlite::{params_from_iter, types::Value as SqlValue};
use std::sync::Arc;

use crate::error::StoreError;
use crate::store::SqliteMemoryStore;

/// An entity returned by one of the `get_entity_*` query arms.
///
/// Per §2.1.0: only `id` and an in-scope edge count are ever returned;
/// `name` and `summary` are cross-scope columns and must not be echoed
/// back from the DB. `echoed_name` carries the caller's literal input
/// string when the `ByName` arm was used — it is never read from the DB.
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
    pub fn new(
        store: Arc<SqliteMemoryStore>,
        allowed_scopes: Vec<ScopeTuple>,
        now: i64,
    ) -> Self {
        debug_assert!(
            !allowed_scopes.is_empty(),
            "invariant: GraphQueries requires non-empty allowed_scopes; \
             materialize_graph_request must short-circuit upstream"
        );
        Self { store, allowed_scopes, now }
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

    /// Id-only entity lookup with an in-scope edge count (§2.1.0, §3.1).
    ///
    /// Returns `None` when the entity is unknown or not visible under
    /// `allowed_scopes` — the §2.1.0 anti-leak contract.
    pub async fn get_entity_by_id(
        &self,
        id: String,
    ) -> Result<Option<EntityHit>, StoreError> {
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
}
