//! Store helpers for the task-trace canvas substrate.
//!
//! These methods are adapter-local scaffolding for issue #134's addendum.
//! The cross-surface verb contract can promote them later once workflow
//! materialization and hot-memory selection are settled.

use rusqlite::{OptionalExtension, params};

use crate::error::StoreError;
use crate::store::{SqliteMemoryStore, current_unix_ms};

/// Draft row for inserting or replaying a trace step.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceStepDraft {
    /// Stable step id minted by the projector.
    pub step_id: String,
    /// Source trace id.
    pub trace_id: String,
    /// Session containing the step.
    pub session_id: String,
    /// Turn containing the step.
    pub turn_id: String,
    /// Optional tool-call id, when the step came from tool execution.
    pub tool_call_id: Option<String>,
    /// Event timestamp in Unix epoch milliseconds.
    pub timestamp_ms: i64,
    /// Optional tool name.
    pub tool_name: Option<String>,
    /// Body-free call summary.
    pub call_summary: String,
    /// Body-free result summary.
    pub result_summary: String,
    /// Optional result record reference for exact lookup.
    pub result_ref: Option<String>,
    /// Relative hot-memory salience score.
    pub salience: f64,
    /// How safely the step can be replaced by a summary.
    pub replaceability_score: f64,
    /// Optional projected canvas node id.
    pub node_id: Option<String>,
    /// Replay dedupe hash derived from body-free source material.
    pub source_hash: String,
}

/// Stored trace-step row.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceStepRow {
    /// Stable step id.
    pub step_id: String,
    /// Source trace id.
    pub trace_id: String,
    /// Session containing the step.
    pub session_id: String,
    /// Turn containing the step.
    pub turn_id: String,
    /// Optional tool-call id.
    pub tool_call_id: Option<String>,
    /// Event timestamp in Unix epoch milliseconds.
    pub timestamp_ms: i64,
    /// Optional tool name.
    pub tool_name: Option<String>,
    /// Body-free call summary.
    pub call_summary: String,
    /// Body-free result summary.
    pub result_summary: String,
    /// Optional result record reference.
    pub result_ref: Option<String>,
    /// Relative hot-memory salience score.
    pub salience: f64,
    /// How safely the step can be replaced by a summary.
    pub replaceability_score: f64,
    /// Optional projected canvas node id.
    pub node_id: Option<String>,
    /// Replay dedupe hash.
    pub source_hash: String,
}

/// Outcome of [`SqliteMemoryStore::upsert_trace_step`].
#[derive(Debug, Clone, PartialEq)]
pub struct TraceStepUpsert {
    /// Stored row, either newly inserted or pre-existing replay target.
    pub row: TraceStepRow,
    /// True when this call inserted the row.
    pub inserted: bool,
}

/// Canvas lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceCanvasStatus {
    /// Canvas is still the current task context.
    Active,
    /// Canvas is completed.
    Completed,
    /// Canvas was abandoned.
    Abandoned,
}

impl TraceCanvasStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Completed => "completed",
            Self::Abandoned => "abandoned",
        }
    }
}

impl TryFrom<String> for TraceCanvasStatus {
    type Error = StoreError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "active" => Ok(Self::Active),
            "completed" => Ok(Self::Completed),
            "abandoned" => Ok(Self::Abandoned),
            _ => Err(StoreError::Invariant {
                what: format!("unknown trace canvas status `{value}`"),
            }),
        }
    }
}

/// Draft row for inserting or updating a task-trace canvas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceCanvasDraft {
    /// Stable canvas id.
    pub canvas_id: String,
    /// Session the canvas belongs to.
    pub session_id: String,
    /// Short canvas title.
    pub title: String,
    /// Current task goal.
    pub goal: String,
    /// Canvas lifecycle status.
    pub status: TraceCanvasStatus,
    /// Body-free current canvas summary.
    pub summary: String,
    /// Optional active node id.
    pub active_node_id: Option<String>,
    /// Byte budget for rendering this canvas into hot memory.
    pub max_bytes: i64,
}

/// Stored task-trace canvas row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceCanvasRow {
    /// Stable canvas id.
    pub canvas_id: String,
    /// Session the canvas belongs to.
    pub session_id: String,
    /// Short canvas title.
    pub title: String,
    /// Current task goal.
    pub goal: String,
    /// Canvas lifecycle status.
    pub status: TraceCanvasStatus,
    /// Body-free current canvas summary.
    pub summary: String,
    /// Optional active node id.
    pub active_node_id: Option<String>,
    /// Byte budget for rendering this canvas into hot memory.
    pub max_bytes: i64,
    /// Monotonic canvas version.
    pub version: i64,
}

/// Draft row for inserting or updating a canvas node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceCanvasNodeDraft {
    /// Stable node id.
    pub node_id: String,
    /// Parent canvas id.
    pub canvas_id: String,
    /// Short node label.
    pub label: String,
    /// Node status string constrained by the `SQLite` schema.
    pub status: String,
    /// Body-free node summary.
    pub summary: String,
    /// Node timestamp in Unix epoch milliseconds.
    pub timestamp_ms: i64,
    /// Source step ids supporting this node.
    pub source_step_ids: Vec<String>,
    /// Evidence record ids supporting this node.
    pub evidence_record_ids: Vec<String>,
}

/// Stored canvas node row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceCanvasNodeRow {
    /// Stable node id.
    pub node_id: String,
    /// Parent canvas id.
    pub canvas_id: String,
    /// Short node label.
    pub label: String,
    /// Node status string.
    pub status: String,
    /// Body-free node summary.
    pub summary: String,
    /// Node timestamp in Unix epoch milliseconds.
    pub timestamp_ms: i64,
    /// Source step ids supporting this node.
    pub source_step_ids: Vec<String>,
    /// Evidence record ids supporting this node.
    pub evidence_record_ids: Vec<String>,
}

/// Active canvas plus its ordered nodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceCanvasContext {
    /// Active canvas row.
    pub canvas: TraceCanvasRow,
    /// Canvas nodes ordered by timestamp then id.
    pub nodes: Vec<TraceCanvasNodeRow>,
}

impl SqliteMemoryStore {
    /// Insert a trace step, returning the existing row on replay.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotInitialized`] when the store is unconnected,
    /// [`StoreError::Worker`] on background-thread failure, or
    /// [`StoreError::Sqlite`] / [`StoreError::Codec`] for storage errors.
    pub async fn upsert_trace_step(
        &self,
        draft: TraceStepDraft,
    ) -> Result<TraceStepUpsert, StoreError> {
        let conn = self.require_conn("upsert_trace_step")?.clone();
        let now_ms = current_unix_ms();
        Ok(conn
            .call(move |c| {
                let inserted = c.execute(
                    "INSERT OR IGNORE INTO trace_steps
                    (step_id, trace_id, session_id, turn_id, tool_call_id, timestamp_ms,
                     tool_name, call_summary, result_summary, result_ref, salience,
                     replaceability_score, node_id, source_hash, created_at_ms, updated_at_ms)
                 VALUES
                    (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?15)",
                    params![
                        draft.step_id,
                        draft.trace_id,
                        draft.session_id,
                        draft.turn_id,
                        draft.tool_call_id,
                        draft.timestamp_ms,
                        draft.tool_name,
                        draft.call_summary,
                        draft.result_summary,
                        draft.result_ref,
                        draft.salience,
                        draft.replaceability_score,
                        draft.node_id,
                        draft.source_hash,
                        now_ms,
                    ],
                )? == 1;

                let tool_call_key = draft.tool_call_id.as_deref().unwrap_or_default();
                let row = c.query_row(
                    "SELECT step_id, trace_id, session_id, turn_id, tool_call_id, timestamp_ms,
                        tool_name, call_summary, result_summary, result_ref, salience,
                        replaceability_score, node_id, source_hash
                   FROM trace_steps
                  WHERE source_hash = ?1
                    AND COALESCE(tool_call_id, '') = ?2",
                    params![draft.source_hash, tool_call_key],
                    trace_step_from_row,
                )?;

                Ok(TraceStepUpsert { row, inserted })
            })
            .await?)
    }

    /// Find trace steps that produced or reference a result record.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotInitialized`] when the store is unconnected,
    /// [`StoreError::Worker`] on background-thread failure, or
    /// [`StoreError::Sqlite`] for query errors.
    pub async fn find_trace_steps_by_result_ref(
        &self,
        result_ref: &str,
    ) -> Result<Vec<TraceStepRow>, StoreError> {
        let conn = self.require_conn("find_trace_steps_by_result_ref")?.clone();
        let result_ref = result_ref.to_owned();
        Ok(conn
            .call(move |c| {
                let mut stmt = c.prepare_cached(
                    "SELECT step_id, trace_id, session_id, turn_id, tool_call_id, timestamp_ms,
                        tool_name, call_summary, result_summary, result_ref, salience,
                        replaceability_score, node_id, source_hash
                   FROM trace_steps
                  WHERE result_ref = ?1
                  ORDER BY timestamp_ms ASC, step_id ASC",
                )?;
                let rows: Result<Vec<_>, rusqlite::Error> =
                    stmt.query_map([result_ref], trace_step_from_row)?.collect();
                Ok(rows?)
            })
            .await?)
    }

    /// Insert or update a task-trace canvas.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotInitialized`] when the store is unconnected,
    /// [`StoreError::Worker`] on background-thread failure, or
    /// [`StoreError::Sqlite`] for storage errors.
    pub async fn upsert_trace_canvas(
        &self,
        draft: TraceCanvasDraft,
    ) -> Result<TraceCanvasRow, StoreError> {
        let conn = self.require_conn("upsert_trace_canvas")?.clone();
        let now_ms = current_unix_ms();
        Ok(conn
            .call(move |c| {
                c.execute(
                    "INSERT INTO trace_canvases
                    (canvas_id, session_id, title, goal, status, summary,
                     active_node_id, max_bytes, version, created_at_ms, updated_at_ms)
                 VALUES
                    (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9, ?9)
                 ON CONFLICT(canvas_id) DO UPDATE SET
                    session_id = excluded.session_id,
                    title = excluded.title,
                    goal = excluded.goal,
                    status = excluded.status,
                    summary = excluded.summary,
                    active_node_id = excluded.active_node_id,
                    max_bytes = excluded.max_bytes,
                    version = trace_canvases.version + 1,
                    updated_at_ms = excluded.updated_at_ms",
                    params![
                        draft.canvas_id,
                        draft.session_id,
                        draft.title,
                        draft.goal,
                        draft.status.as_str(),
                        draft.summary,
                        draft.active_node_id,
                        draft.max_bytes,
                        now_ms,
                    ],
                )?;
                Ok(trace_canvas_by_id(c, &draft.canvas_id)?)
            })
            .await?)
    }

    /// Insert or update a task-trace canvas node.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotInitialized`] when the store is unconnected,
    /// [`StoreError::Worker`] on background-thread failure, or
    /// [`StoreError::Sqlite`] / [`StoreError::Codec`] for storage errors.
    pub async fn upsert_trace_canvas_node(
        &self,
        draft: TraceCanvasNodeDraft,
    ) -> Result<TraceCanvasNodeRow, StoreError> {
        let conn = self.require_conn("upsert_trace_canvas_node")?.clone();
        let source_step_ids = serde_json::to_string(&draft.source_step_ids)?;
        let evidence_record_ids = serde_json::to_string(&draft.evidence_record_ids)?;
        Ok(conn
            .call(move |c| {
                c.execute(
                    "INSERT INTO trace_canvas_nodes
                    (node_id, canvas_id, label, status, summary, timestamp_ms,
                     source_step_ids, evidence_record_ids)
                 VALUES
                    (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(node_id) DO UPDATE SET
                    canvas_id = excluded.canvas_id,
                    label = excluded.label,
                    status = excluded.status,
                    summary = excluded.summary,
                    timestamp_ms = excluded.timestamp_ms,
                    source_step_ids = excluded.source_step_ids,
                    evidence_record_ids = excluded.evidence_record_ids",
                    params![
                        draft.node_id,
                        draft.canvas_id,
                        draft.label,
                        draft.status,
                        draft.summary,
                        draft.timestamp_ms,
                        source_step_ids,
                        evidence_record_ids,
                    ],
                )?;
                Ok(trace_canvas_node_by_id(c, &draft.node_id)?)
            })
            .await?)
    }

    /// Return the latest active trace canvas and its ordered nodes.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotInitialized`] when the store is unconnected,
    /// [`StoreError::Worker`] on background-thread failure, or
    /// [`StoreError::Sqlite`] / [`StoreError::Codec`] for storage errors.
    pub async fn active_trace_canvas_for_session(
        &self,
        session_id: &str,
    ) -> Result<Option<TraceCanvasContext>, StoreError> {
        let conn = self
            .require_conn("active_trace_canvas_for_session")?
            .clone();
        let session_id = session_id.to_owned();
        Ok(conn
            .call(move |c| {
                let Some(canvas) = c
                    .query_row(
                        "SELECT canvas_id, session_id, title, goal, status, summary,
                            active_node_id, max_bytes, version
                       FROM trace_canvases
                      WHERE session_id = ?1
                        AND status = 'active'
                      ORDER BY updated_at_ms DESC, canvas_id ASC
                      LIMIT 1",
                        [session_id],
                        trace_canvas_from_row,
                    )
                    .optional()?
                else {
                    return Ok(None);
                };

                let mut stmt = c.prepare_cached(
                    "SELECT node_id, canvas_id, label, status, summary, timestamp_ms,
                        source_step_ids, evidence_record_ids
                   FROM trace_canvas_nodes
                  WHERE canvas_id = ?1
                  ORDER BY timestamp_ms ASC, node_id ASC",
                )?;
                let nodes: Result<Vec<_>, rusqlite::Error> = stmt
                    .query_map([canvas.canvas_id.as_str()], trace_canvas_node_from_row)?
                    .collect();

                Ok(Some(TraceCanvasContext {
                    canvas,
                    nodes: nodes?,
                }))
            })
            .await?)
    }
}

fn trace_step_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TraceStepRow> {
    Ok(TraceStepRow {
        step_id: row.get(0)?,
        trace_id: row.get(1)?,
        session_id: row.get(2)?,
        turn_id: row.get(3)?,
        tool_call_id: row.get(4)?,
        timestamp_ms: row.get(5)?,
        tool_name: row.get(6)?,
        call_summary: row.get(7)?,
        result_summary: row.get(8)?,
        result_ref: row.get(9)?,
        salience: row.get(10)?,
        replaceability_score: row.get(11)?,
        node_id: row.get(12)?,
        source_hash: row.get(13)?,
    })
}

fn trace_canvas_by_id(
    c: &rusqlite::Connection,
    canvas_id: &str,
) -> rusqlite::Result<TraceCanvasRow> {
    c.query_row(
        "SELECT canvas_id, session_id, title, goal, status, summary,
                active_node_id, max_bytes, version
           FROM trace_canvases
          WHERE canvas_id = ?1",
        [canvas_id],
        trace_canvas_from_row,
    )
}

fn trace_canvas_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TraceCanvasRow> {
    let status: String = row.get(4)?;
    Ok(TraceCanvasRow {
        canvas_id: row.get(0)?,
        session_id: row.get(1)?,
        title: row.get(2)?,
        goal: row.get(3)?,
        status: TraceCanvasStatus::try_from(status).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(err))
        })?,
        summary: row.get(5)?,
        active_node_id: row.get(6)?,
        max_bytes: row.get(7)?,
        version: row.get(8)?,
    })
}

fn trace_canvas_node_by_id(
    c: &rusqlite::Connection,
    node_id: &str,
) -> rusqlite::Result<TraceCanvasNodeRow> {
    c.query_row(
        "SELECT node_id, canvas_id, label, status, summary, timestamp_ms,
                source_step_ids, evidence_record_ids
           FROM trace_canvas_nodes
          WHERE node_id = ?1",
        [node_id],
        trace_canvas_node_from_row,
    )
}

fn trace_canvas_node_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TraceCanvasNodeRow> {
    let source_step_ids: String = row.get(6)?;
    let evidence_record_ids: String = row.get(7)?;
    Ok(TraceCanvasNodeRow {
        node_id: row.get(0)?,
        canvas_id: row.get(1)?,
        label: row.get(2)?,
        status: row.get(3)?,
        summary: row.get(4)?,
        timestamp_ms: row.get(5)?,
        source_step_ids: serde_json::from_str(&source_step_ids).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(err))
        })?,
        evidence_record_ids: serde_json::from_str(&evidence_record_ids).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Text, Box::new(err))
        })?,
    })
}
