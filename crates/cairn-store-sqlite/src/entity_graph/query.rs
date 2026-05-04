//! `graph_edges` — read entity edges with direction / relation / as-of filters.
//!
//! Pure read. No WAL writes.

use cairn_core::contract::memory_store::EdgeDir;
use cairn_core::domain::graph::{
    EdgeConfidence, EntityEdge, EntityEdgeId, EntityId, GraphEdgesArgs,
};
use cairn_core::domain::record::RecordId;
use tracing::instrument;

use crate::error::StoreError;
use crate::store::SqliteMemoryStore;

/// Build the dynamic SELECT for `do_graph_edges`. Pulled out to keep the
/// async worker closure under the workspace `clippy::too_many_lines` cap.
///
/// Two independent dimensions:
///   - `include_invalidated`: history-view flag. true = also return rows
///     whose end-bound has fired. false = only rows currently valid in
///     the requested time-slice.
///   - `as_of_*`: which point in time to view. None = "now".
///
/// The "live now" filter applies only when no as-of slice was requested
/// AND the caller wants the production view. For each as-of dimension we
/// always apply the start-bound (`valid_at`/`created_at` ≤ T), but the
/// end-bound (`invalid_at`/`expired_at` not yet fired) is conditional on
/// `!include_invalidated`: history-view callers (lint --fix-graph,
/// audit) need to see edges that were already invalidated by T as well
/// as edges that survived past T.
fn build_query(
    node_id: &str,
    direction: EdgeDir,
    relation: Option<&str>,
    as_event: Option<i64>,
    as_ingest: Option<i64>,
    include_invalid: bool,
) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
    let mut sql = String::from(
        "SELECT id, source_id, target_id, relation, confidence, \
         confidence_score, valid_at, invalid_at, created_at, source_record_id \
         FROM entity_edges WHERE 1=1",
    );
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    match direction {
        EdgeDir::Out => {
            sql.push_str(" AND source_id = ?");
            params.push(Box::new(node_id.to_owned()));
        }
        EdgeDir::In => {
            sql.push_str(" AND target_id = ?");
            params.push(Box::new(node_id.to_owned()));
        }
        EdgeDir::Both => {
            sql.push_str(" AND (source_id = ? OR target_id = ?)");
            params.push(Box::new(node_id.to_owned()));
            params.push(Box::new(node_id.to_owned()));
        }
    }
    if let Some(rel) = relation {
        sql.push_str(" AND relation = ?");
        params.push(Box::new(rel.to_owned()));
    }
    // Per-dimension predicates are independent. Each dimension contributes
    // a start-bound (only when as-of given) and an end-bound (only in the
    // production view, !include_invalid):
    //   * with as-of T: end-bound is `(end_col IS NULL OR end_col > T)`
    //   * without as-of: end-bound is `end_col IS NULL` (live now in
    //     this dimension)
    // Pre-fix this branch skipped the end-bound entirely for a dimension
    // when no as-of was provided AND the OTHER dimension had as-of, which
    // could leak event-invalidated rows into ingest-only as-of queries
    // and ingest-expired rows into event-only as-of queries.
    if let Some(t) = as_event {
        sql.push_str(" AND valid_at <= ?");
        params.push(Box::new(t));
        if !include_invalid {
            sql.push_str(" AND (invalid_at IS NULL OR invalid_at > ?)");
            params.push(Box::new(t));
        }
    } else if !include_invalid {
        sql.push_str(" AND invalid_at IS NULL");
    }
    if let Some(t) = as_ingest {
        sql.push_str(" AND created_at <= ?");
        params.push(Box::new(t));
        if !include_invalid {
            sql.push_str(" AND (expired_at IS NULL OR expired_at > ?)");
            params.push(Box::new(t));
        }
    } else if !include_invalid {
        sql.push_str(" AND expired_at IS NULL");
    }
    (sql, params)
}

/// Decode one row from the `entity_edges` SELECT into an `EntityEdge`.
///
/// Stored-state corruption (unknown confidence tier, malformed `RecordId`)
/// is surfaced as a typed [`StoreError::Invariant`] rather than as a
/// generic `rusqlite::Error::FromSqlConversionFailure`. Without this,
/// the caller saw `StoreError::Worker(...)` (an infrastructure failure)
/// for what is actually persistent data corruption — masking it from
/// alerting and recovery code paths. Mirrors the typed-error idiom in
/// `entity_graph/resolve.rs` and `store/upsert.rs`.
fn map_row(r: &rusqlite::Row<'_>) -> Result<EntityEdge, StoreError> {
    let id: String = r.get(0)?;
    let source_id: String = r.get(1)?;
    let target_id: String = r.get(2)?;
    let relation: String = r.get(3)?;
    let conf_str: String = r.get(4)?;
    let confidence_score: f32 = r.get(5)?;
    let valid_at: i64 = r.get(6)?;
    let invalid_at: Option<i64> = r.get(7)?;
    let created_at: i64 = r.get(8)?;
    let src_rec: Option<String> = r.get(9)?;

    let confidence = EdgeConfidence::from_db_str(&conf_str).ok_or_else(|| {
        StoreError::Invariant {
            what: format!("entity_edges row {id}: unknown confidence tier `{conf_str}`"),
        }
    })?;
    let source_record_id = match src_rec {
        Some(s) => Some(RecordId::parse(&s).map_err(|e| StoreError::Invariant {
            what: format!("entity_edges row {id}: invalid source_record_id `{s}`: {e}"),
        })?),
        None => None,
    };
    Ok(EntityEdge {
        id: EntityEdgeId::from(id),
        source_id: EntityId::from(source_id),
        target_id: EntityId::from(target_id),
        relation,
        confidence,
        confidence_score,
        valid_at,
        invalid_at,
        created_at,
        source_record_id,
    })
}

/// Translate a worker-side error into a typed [`StoreError`], preserving
/// any [`StoreError`] previously wrapped via `tokio_rusqlite::Error::Other`.
/// Mirrors `entity_graph::resolve::unpack_worker_err` and `store::search`.
fn unpack_worker_err(err: tokio_rusqlite::Error) -> StoreError {
    match err {
        tokio_rusqlite::Error::Other(boxed) => match boxed.downcast::<StoreError>() {
            Ok(inner) => *inner,
            Err(other) => StoreError::Worker(tokio_rusqlite::Error::Other(other)),
        },
        other => StoreError::from(other),
    }
}

impl SqliteMemoryStore {
    /// Inherent `graph_edges` implementation; the trait method
    /// [`MemoryStore::graph_edges`] guards `self.conn` then delegates here.
    ///
    /// [`MemoryStore::graph_edges`]: cairn_core::contract::memory_store::MemoryStore::graph_edges
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Worker`] when the background `tokio_rusqlite`
    /// worker fails, and [`StoreError::Sqlite`] for SQL errors.
    #[instrument(skip(self, args), fields(verb = "graph_edges"), err)]
    pub(crate) async fn do_graph_edges(
        &self,
        args: &GraphEdgesArgs<'_>,
    ) -> Result<Vec<EntityEdge>, StoreError> {
        let conn = self.require_conn("graph_edges")?.clone();
        let node_id = args.node_id.as_str().to_owned();
        let direction = args.direction;
        let relation = args.relation_filter.map(str::to_owned);
        let as_event = args.as_of_event_time;
        let as_ingest = args.as_of_ingest_time;
        let include_invalid = args.include_invalidated;

        let edges = conn
            .call(move |c| -> Result<Vec<EntityEdge>, tokio_rusqlite::Error> {
                let (sql, params) = build_query(
                    &node_id,
                    direction,
                    relation.as_deref(),
                    as_event,
                    as_ingest,
                    include_invalid,
                );
                let mut stmt = c.prepare(&sql)?;
                let param_refs: Vec<&dyn rusqlite::ToSql> =
                    params.iter().map(std::convert::AsRef::as_ref).collect();
                let mut rows = stmt.query(rusqlite::params_from_iter(param_refs))?;
                let mut out = Vec::new();
                while let Some(row) = rows.next()? {
                    let edge = map_row(row)
                        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                    out.push(edge);
                }
                Ok(out)
            })
            .await
            .map_err(unpack_worker_err)?;

        Ok(edges)
    }
}
