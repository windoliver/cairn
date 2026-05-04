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
                let mut sql = String::from(
                    "SELECT id, source_id, target_id, relation, confidence, \
                     confidence_score, valid_at, invalid_at, created_at, source_record_id \
                     FROM entity_edges WHERE 1=1",
                );
                let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

                match direction {
                    EdgeDir::Out => {
                        sql.push_str(" AND source_id = ?");
                        params.push(Box::new(node_id.clone()));
                    }
                    EdgeDir::In => {
                        sql.push_str(" AND target_id = ?");
                        params.push(Box::new(node_id.clone()));
                    }
                    EdgeDir::Both => {
                        sql.push_str(" AND (source_id = ? OR target_id = ?)");
                        params.push(Box::new(node_id.clone()));
                        params.push(Box::new(node_id.clone()));
                    }
                }
                if let Some(rel) = relation {
                    sql.push_str(" AND relation = ?");
                    params.push(Box::new(rel));
                }
                // The "live now" filter applies ONLY when no as-of slice
                // is requested. With an as-of predicate, the time-slice
                // window itself is the source of truth: an edge that was
                // valid at event-time T but later contradicted (invalid_at
                // set) MUST still surface for as_of_event_time = T-or-earlier.
                // Pre-fix this clause AND'd `invalid_at IS NULL` with the
                // as-of predicate, silently hiding contradicted history.
                if !include_invalid && as_event.is_none() && as_ingest.is_none() {
                    sql.push_str(" AND invalid_at IS NULL AND expired_at IS NULL");
                }
                if let Some(t) = as_event {
                    sql.push_str(" AND valid_at <= ? AND (invalid_at IS NULL OR invalid_at > ?)");
                    params.push(Box::new(t));
                    params.push(Box::new(t));
                }
                if let Some(t) = as_ingest {
                    sql.push_str(" AND created_at <= ? AND (expired_at IS NULL OR expired_at > ?)");
                    params.push(Box::new(t));
                    params.push(Box::new(t));
                }

                let mut stmt = c.prepare(&sql)?;
                let param_refs: Vec<&dyn rusqlite::ToSql> =
                    params.iter().map(std::convert::AsRef::as_ref).collect();
                let rows = stmt.query_map(rusqlite::params_from_iter(param_refs), |r| {
                    let conf_str: String = r.get(4)?;
                    let confidence = EdgeConfidence::from_db_str(&conf_str).ok_or_else(|| {
                        rusqlite::Error::FromSqlConversionFailure(
                            4,
                            rusqlite::types::Type::Text,
                            Box::new(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!("unknown confidence tier: {conf_str}"),
                            )),
                        )
                    })?;
                    let src_rec: Option<String> = r.get(9)?;
                    let source_record_id = match src_rec {
                        Some(s) => Some(RecordId::parse(&s).map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                9,
                                rusqlite::types::Type::Text,
                                Box::new(std::io::Error::new(
                                    std::io::ErrorKind::InvalidData,
                                    format!("invalid stored RecordId: {e}"),
                                )),
                            )
                        })?),
                        None => None,
                    };
                    Ok(EntityEdge {
                        id: EntityEdgeId::from(r.get::<_, String>(0)?),
                        source_id: EntityId::from(r.get::<_, String>(1)?),
                        target_id: EntityId::from(r.get::<_, String>(2)?),
                        relation: r.get(3)?,
                        confidence,
                        confidence_score: r.get(5)?,
                        valid_at: r.get(6)?,
                        invalid_at: r.get(7)?,
                        created_at: r.get(8)?,
                        source_record_id,
                    })
                })?;

                let mut out = Vec::new();
                for row in rows {
                    out.push(row?);
                }
                Ok(out)
            })
            .await?;

        Ok(edges)
    }
}
