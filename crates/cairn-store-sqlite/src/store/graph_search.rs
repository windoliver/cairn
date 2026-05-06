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
//!   - `<auth_scope predicate>` against `records.scope` JSON. v1 leans on
//!     the visibility allowlist as the primary auth boundary. The
//!     `args.auth_scope` field is accepted and validated by the caller,
//!     but is **not** folded into the predicate yet — multi-tenant
//!     deployments must continue to narrow via the visibility allowlist
//!     until a follow-up issue lands the JSON1-based scope predicate.
//!   - Supersession (`NOT EXISTS edges_updates`). v1 relies on the
//!     `active = 1 AND tombstoned = 0` gate, which excludes superseded
//!     versions of a record but does not honor the latest-record
//!     invariant when a record was superseded purely via the edges
//!     mechanism.
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
use cairn_core::search::GraphCandidate;
use rusqlite::types::Value as SqlVal;
use tracing::instrument;

use crate::error::StoreError;
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
/// Returns `(sql, params)`. Param order:
///   1. seeds (`n_seeds` × `RecordId`)
///   2. `as_event_ms` (`i64`) — `valid_at <= ?`
///   3. `as_ingest_ms` (`i64`) — `created_at <= ?`
///   4. `confidence_min` (`f64`)
///   5. ranked exclusion list (`n_ranked` × `RecordId`)
///   6. visibilities (`n_vis` × `String`)
///   7. limit (`i64`)
fn build_query(n_seeds: usize, n_ranked: usize, n_visibilities: usize) -> String {
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
    let vis_in = placeholders(n_visibilities);
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
             WHERE (e.source_id IN (SELECT entity_node_id FROM seeds) \
                 OR e.target_id IN (SELECT entity_node_id FROM seeds)) \
               AND e.invalid_at IS NULL AND e.expired_at IS NULL \
               AND e.valid_at <= ? AND e.created_at <= ? \
               AND e.confidence_score >= ? \
         ) \
         SELECT r.record_id, MAX(n.confidence_score) AS conf \
           FROM neighbors n \
           JOIN entity_episodes ep ON ep.entity_node_id = n.neighbor_id \
           JOIN records r ON r.record_id = ep.episode_id \
          WHERE n.neighbor_id NOT IN (SELECT entity_node_id FROM seeds) \
            AND r.tombstoned = 0 AND r.active = 1 \
            AND r.record_id NOT IN ({ranked_in}) \
            AND r.visibility IN ({vis_in}) \
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

        let candidates = conn
            .call(
                move |c| -> Result<Vec<GraphCandidate>, tokio_rusqlite::Error> {
                    let sql = build_query(seeds.len(), ranked.len(), visibilities.len());
                    let mut params: Vec<SqlVal> =
                        Vec::with_capacity(seeds.len() + 3 + ranked.len() + visibilities.len() + 1);
                    for s in &seeds {
                        params.push(SqlVal::Text(s.clone()));
                    }
                    params.push(SqlVal::Integer(now_ms));
                    params.push(SqlVal::Integer(now_ms));
                    params.push(SqlVal::Real(confidence_min));
                    for r in &ranked {
                        params.push(SqlVal::Text(r.clone()));
                    }
                    for v in &visibilities {
                        params.push(SqlVal::Text(v.clone()));
                    }
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
