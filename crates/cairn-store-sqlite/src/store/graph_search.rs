//! `MemoryStore::search_graph_neighbors` impl (issue #191, spec §4.3 / §5.1).
//!
//! Pragmatic v1 SQL: 1-hop entity-graph traversal seeded by the lexical
//! legs' record ids, with edge bitemporal predicates, edge confidence
//! floor, neighbor records' tombstone/active gate, visibility allowlist,
//! and the ranked-list dedup set.
//!
//! ## Deferrals from the spec
//!
//! The full §5.1 SQL adds:
//!   - `carray` extension for bind-list expansion. v1 generates `?, ?, ?`
//!     placeholders inline, capped at `SQLite`'s default `MAX_VARIABLE_NUMBER`
//!     (999); seed/ranked lists are bounded by the orchestrator at 400 + 100,
//!     well within the limit.
//!   - Statement-scoped cancellation via `interrupt()` + `progress_handler`.
//!     v1 relies on `tokio::time::timeout` at the orchestrator boundary;
//!     phase 6 bench gates will reveal whether the cooperative-cancellation
//!     primitive is needed in production.

use cairn_core::contract::memory_store::GraphNeighborsArgs;
use cairn_core::domain::RecordId;
use cairn_core::domain::filter::compile_filter;
use cairn_core::search::GraphCandidate;
use rusqlite::types::Value as SqlVal;
use tracing::instrument;

use crate::error::StoreError;
use crate::store::scope_predicate::build_scope_predicate;
use crate::store::search::{SUPERSESSION_NOT_EXISTS_CLAUSE, json_to_sql};
use crate::store::{SqliteMemoryStore, current_unix_ms};

/// Hard cap on either id-list bind count to stay safely under `SQLite`'s
/// default `MAX_VARIABLE_NUMBER = 999` after the bitemporal/confidence
/// scalars are added. Spec calls for 400 seeds + 100 ranked = 500; cap
/// applies an order-preserving truncation if a caller exceeds it.
const SQLITE_BIND_CAP: usize = 480;

/// Hard upper bound on `args.limit` to keep result-set sizes bounded.
const GRAPH_LIMIT_MAX: usize = 1000;

/// Build the bind-expanded SQL + params for the graph 1-hop traversal.
///
/// Param order is documented inline at each `?` in the assembled SQL; the
/// caller in `do_search_graph_neighbors` pushes parameters in the same
/// order.
fn build_query(
    n_seeds: usize,
    n_ranked: usize,
    n_visibilities: usize,
    neighbor_scope_sql: &str,
    provenance_scope_sql: &str,
    filter_sql: &str,
) -> String {
    fn placeholders(n: usize) -> String {
        if n == 0 {
            // SQLite rejects empty `IN ()`; substitute an always-false literal
            // so the predicate evaluates without a syntax error.
            "SELECT NULL WHERE 0".to_owned()
        } else {
            std::iter::repeat_n("?", n).collect::<Vec<_>>().join(",")
        }
    }
    let seed_in = placeholders(n_seeds);
    let ranked_in = placeholders(n_ranked);
    // Empty visibility allowlist means "no visibility filter" — match the
    // keyword/semantic SQL builders. Without this guard, an empty allowlist
    // would translate to `r.visibility IN (SELECT NULL WHERE 0)` (always
    // false) and silently drop every graph candidate.
    let (neighbor_visibility_clause, provenance_visibility_clause) = if n_visibilities == 0 {
        (String::new(), String::new())
    } else {
        let neighbor_vis = placeholders(n_visibilities);
        let provenance_vis = placeholders(n_visibilities);
        (
            format!(" AND r.visibility IN ({neighbor_vis})"),
            format!(" AND pr.visibility IN ({provenance_vis})"),
        )
    };
    let filter_clause = if filter_sql.is_empty() {
        String::new()
    } else {
        format!(" AND ({filter_sql})")
    };
    // Provenance supersession clause — same shape as
    // `SUPERSESSION_NOT_EXISTS_CLAUSE` but rewritten against the `pr`
    // (provenance) alias so an edge whose source record was superseded
    // contributes nothing.
    let provenance_supersession = "NOT EXISTS ( SELECT 1 FROM edges e2 \
        WHERE e2.kind = 'updates' AND e2.dst = pr.record_id )";
    format!(
        "WITH seeds AS ( \
            SELECT DISTINCT entity_node_id \
              FROM entity_episodes \
             WHERE episode_id IN ({seed_in}) \
         ), \
         neighbors AS ( \
            SELECT \
                CASE WHEN e.source_id IN (SELECT entity_node_id FROM seeds) \
                     THEN e.target_id ELSE e.source_id END AS neighbor_id, \
                e.confidence_score \
              FROM entity_edges e \
              JOIN records pr ON pr.record_id = e.source_record_id \
             WHERE (e.source_id IN (SELECT entity_node_id FROM seeds) \
                 OR e.target_id IN (SELECT entity_node_id FROM seeds)) \
               AND e.invalid_at IS NULL AND e.expired_at IS NULL \
               AND e.valid_at <= ? AND e.created_at <= ? \
               AND e.confidence_score >= ? \
               AND pr.active = 1 AND pr.tombstoned = 0\
               {provenance_visibility_clause}{provenance_scope_sql} \
               AND {provenance_supersession} \
         ) \
         SELECT r.record_id, MAX(n.confidence_score) AS conf \
           FROM neighbors n \
           JOIN entity_episodes ep ON ep.entity_node_id = n.neighbor_id \
           JOIN records r ON r.record_id = ep.episode_id \
          WHERE n.neighbor_id NOT IN (SELECT entity_node_id FROM seeds) \
            AND r.tombstoned = 0 AND r.active = 1 \
            AND r.record_id NOT IN ({ranked_in}){neighbor_visibility_clause}\
            {neighbor_scope_sql}{filter_clause} \
            AND {SUPERSESSION_NOT_EXISTS_CLAUSE} \
          GROUP BY r.record_id \
          ORDER BY conf DESC, r.updated_at DESC \
          LIMIT ?"
    )
}

impl SqliteMemoryStore {
    /// Inherent `search_graph_neighbors` implementation; the trait method
    /// guards `self.conn` and capability before delegating here.
    ///
    /// # Errors
    /// Propagates [`StoreError::Sqlite`] from worker SQL errors and
    /// [`StoreError::Worker`] from `tokio_rusqlite` infrastructure.
    #[instrument(
        skip(self, args),
        err,
        fields(
            verb = "search_graph_neighbors",
            seeds = args.seed_record_ids.len(),
            ranked = args.ranked_record_ids.len(),
            limit = args.limit
        ),
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "param assembly mirrors SQL structure; splitting reduces clarity"
    )]
    pub(crate) async fn do_search_graph_neighbors(
        &self,
        args: &GraphNeighborsArgs<'_>,
    ) -> Result<Vec<GraphCandidate>, StoreError> {
        // Empty seeds → empty result. The traversal is meaningless without
        // a seed set and short-circuits a wasted round-trip.
        if args.seed_record_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.require_conn("search_graph_neighbors")?.clone();

        // Truncate to the bind cap, preserving caller-supplied order so
        // overfetch overflow is deterministic.
        let seeds: Vec<String> = args
            .seed_record_ids
            .iter()
            .take(SQLITE_BIND_CAP)
            .map(|r| r.as_str().to_owned())
            .collect();
        let ranked: Vec<String> = args
            .ranked_record_ids
            .iter()
            .take(SQLITE_BIND_CAP)
            .map(|r| r.as_str().to_owned())
            .collect();
        let visibilities: Vec<String> = args
            .visibility_allowlist
            .iter()
            .map(|v| v.as_str().to_owned())
            .collect();
        let limit = args.limit.clamp(1, GRAPH_LIMIT_MAX);
        let confidence_min = f64::from(args.confidence_min.clamp(0.0, 1.0));
        let now_ms = current_unix_ms();
        let (neighbor_scope_sql, neighbor_scope_params) =
            build_scope_predicate("r", &args.auth_scope);
        let (provenance_scope_sql, provenance_scope_params) =
            build_scope_predicate("pr", &args.auth_scope);
        let compiled = args.filter.map(compile_filter);
        let filter_sql = compiled
            .as_ref()
            .map(|cf| cf.sql.clone())
            .unwrap_or_default();
        let filter_params: Vec<SqlVal> = compiled
            .as_ref()
            .map(|cf| cf.params.iter().map(json_to_sql).collect())
            .unwrap_or_default();

        let candidates = conn
            .call(
                move |c| -> Result<Vec<GraphCandidate>, tokio_rusqlite::Error> {
                    let sql = build_query(
                        seeds.len(),
                        ranked.len(),
                        visibilities.len(),
                        &neighbor_scope_sql,
                        &provenance_scope_sql,
                        &filter_sql,
                    );
                    let mut params: Vec<SqlVal> = Vec::with_capacity(
                        seeds.len()
                            + 3
                            + (visibilities.len() * 2)
                            + provenance_scope_params.len()
                            + ranked.len()
                            + neighbor_scope_params.len()
                            + filter_params.len()
                            + 1,
                    );
                    for s in &seeds {
                        params.push(SqlVal::Text(s.clone()));
                    }
                    params.push(SqlVal::Integer(now_ms));
                    params.push(SqlVal::Integer(now_ms));
                    params.push(SqlVal::Real(confidence_min));
                    // Provenance-side visibility + scope (applied inside the
                    // `neighbors` CTE before edges contribute).
                    for v in &visibilities {
                        params.push(SqlVal::Text(v.clone()));
                    }
                    params.extend(provenance_scope_params);
                    // Neighbor-side ranked dedup + visibility + scope + filter.
                    for r in &ranked {
                        params.push(SqlVal::Text(r.clone()));
                    }
                    for v in &visibilities {
                        params.push(SqlVal::Text(v.clone()));
                    }
                    params.extend(neighbor_scope_params);
                    params.extend(filter_params);
                    #[allow(clippy::cast_possible_wrap)]
                    params.push(SqlVal::Integer(limit as i64));

                    let mut stmt = c.prepare(&sql)?;
                    let mut rows = stmt.query(rusqlite::params_from_iter(params.iter()))?;
                    let mut out: Vec<GraphCandidate> = Vec::new();
                    let mut rank: usize = 1;
                    while let Some(row) = rows.next()? {
                        let rec_str: String = row.get(0)?;
                        let confidence: f64 = row.get(1)?;
                        let record_id = RecordId::parse(rec_str).map_err(|e| {
                            tokio_rusqlite::Error::Other(Box::new(StoreError::Invariant {
                                what: format!("graph_search: invalid record_id from store: {e}"),
                            }))
                        })?;
                        #[allow(clippy::cast_possible_truncation)]
                        out.push(GraphCandidate {
                            record_id,
                            edge_confidence_score: confidence as f32,
                            graph_rank: rank,
                        });
                        rank += 1;
                    }
                    Ok(out)
                },
            )
            .await
            .map_err(StoreError::from)?;

        Ok(candidates)
    }
}
