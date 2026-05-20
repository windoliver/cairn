//! Nexus projection ledger support.
//!
//! The `records` table remains authoritative. This module exposes current
//! record cursors and records sidecar projection state so stale/missing
//! rebuildable indexes can be reported and repaired.

use std::{collections::HashMap, path::Path};

use cairn_core::contract::memory_store::{ProjectionApplyItem, ProjectionRecord};
use cairn_core::domain::{
    MemoryRecord, RecordId,
    projection::{
        ParserProjectionKind, ProjectionCursor, ProjectionItemState, ProjectionLedgerRow,
        ProjectionSummary, ProjectionTarget,
    },
};
use rusqlite::{OptionalExtension, params};

use crate::{error::StoreError, store::SqliteMemoryStore};

impl SqliteMemoryStore {
    pub(crate) async fn do_projection_records(&self) -> Result<Vec<ProjectionRecord>, StoreError> {
        let conn = self.require_conn("projection_records")?.clone();
        Ok(conn
            .call(move |c| {
                current_projection_records(c).map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))
            })
            .await?)
    }

    pub(crate) async fn do_projection_summaries(
        &self,
    ) -> Result<Vec<ProjectionSummary>, StoreError> {
        let conn = self.require_conn("projection_summaries")?.clone();
        Ok(conn
            .call(move |c| {
                let records = current_projection_records(c)
                    .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                let mut summaries = Vec::new();
                summaries.push(
                    summary_for_target(
                        c,
                        ProjectionTarget::Bm25sLexical,
                        records.iter().map(|record| {
                            (
                                record.cursor.record_id.as_str(),
                                record.cursor.record_hash.as_str(),
                                "",
                            )
                        }),
                    )
                    .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?,
                );

                for target in [
                    ProjectionTarget::Parser(ParserProjectionKind::PdfText),
                    ProjectionTarget::Parser(ParserProjectionKind::DocxText),
                    ProjectionTarget::Parser(ParserProjectionKind::VideoFrameText),
                    ProjectionTarget::Parser(ParserProjectionKind::VisionCaption),
                ] {
                    let target_records = records
                        .iter()
                        .filter_map(|record| {
                            let source_path = record.source_path.as_deref()?;
                            let source_hash = record.source_hash.as_deref()?;
                            (parser_target_for_source(source_path).as_ref() == Some(&target))
                                .then_some((
                                    record.cursor.record_id.as_str(),
                                    record.cursor.record_hash.as_str(),
                                    source_hash,
                                ))
                        })
                        .collect::<Vec<_>>();
                    let summary = summary_for_target(c, target, target_records)
                        .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                    if summary.total_authoritative_items > 0
                        || summary.current_items > 0
                        || summary.lagging_items > 0
                        || summary.failed_items > 0
                    {
                        summaries.push(summary);
                    }
                }
                Ok::<_, tokio_rusqlite::Error>(summaries)
            })
            .await?)
    }

    pub(crate) async fn do_projection_failures(
        &self,
    ) -> Result<Vec<ProjectionLedgerRow>, StoreError> {
        let conn = self.require_conn("projection_failures")?.clone();
        Ok(conn.call(move |c| {
            let records = current_projection_records(c)
                .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
            let records_by_id = records
                .into_iter()
                .map(|record| (record.cursor.record_id.as_str().to_owned(), record))
                .collect::<HashMap<_, _>>();

            let mut stmt = c.prepare(
                "SELECT target, record_id, wal_sequence, record_hash, source_hash, reason, updated_at
                 FROM projection_ledger
                 WHERE state = 'failed'
                 ORDER BY target, record_id, source_hash",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })?;

            let mut failures = Vec::new();
            for row in rows {
                let (target, record_id, wal_sequence, record_hash, source_hash, reason, updated_at) =
                    row?;
                let Some(current) = records_by_id.get(&record_id) else {
                    continue;
                };
                if current.cursor.record_hash != record_hash {
                    continue;
                }
                if !source_hash.is_empty()
                    && current.source_hash.as_deref() != Some(source_hash.as_str())
                {
                    continue;
                }
                let target = ProjectionTarget::from_key(&target).ok_or_else(|| {
                    tokio_rusqlite::Error::Other(Box::new(StoreError::Invariant {
                        what: format!("unknown projection target {target}"),
                    }))
                })?;
                failures.push(ProjectionLedgerRow {
                    target,
                    cursor: ProjectionCursor {
                        record_id: parse_record_id(&record_id)
                            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?,
                        wal_sequence: checked_i64_to_u64(wal_sequence, "wal_sequence")
                            .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?,
                        record_hash,
                        source_hash: (!source_hash.is_empty()).then_some(source_hash),
                    },
                    state: ProjectionItemState::Failed {
                        reason: reason.unwrap_or_else(|| "projection failed".to_owned()),
                    },
                    updated_at,
                });
            }
            Ok::<_, tokio_rusqlite::Error>(failures)
        })
        .await?)
    }

    pub(crate) async fn do_apply_projection_items(
        &self,
        items: Vec<ProjectionApplyItem>,
    ) -> Result<(), StoreError> {
        let conn = self.require_conn("apply_projection_items")?.clone();
        conn.call(move |c| {
            for item in items {
                let row = item.row;
                let target = row.target.as_key();
                let source_hash = row.cursor.source_hash.unwrap_or_default();
                let (state, reason) = projection_state_parts(&row.state);
                let wal_sequence = checked_u64_to_i64(row.cursor.wal_sequence, "wal_sequence")
                    .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                c.execute(
                    "INSERT INTO projection_ledger(
                        target, record_id, wal_sequence, record_hash, source_hash, state, reason, updated_at
                     )
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT(target, record_id, record_hash, source_hash) DO UPDATE SET
                        wal_sequence = excluded.wal_sequence,
                        state = excluded.state,
                        reason = excluded.reason,
                        updated_at = excluded.updated_at",
                    params![
                        target,
                        row.cursor.record_id.as_str(),
                        wal_sequence,
                        row.cursor.record_hash,
                        source_hash,
                        state,
                        reason,
                        row.updated_at,
                    ],
                )?;
            }
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await?;
        Ok(())
    }
}

fn current_projection_records(
    conn: &rusqlite::Connection,
) -> Result<Vec<ProjectionRecord>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT record_id, version, body_hash, body, record_json
         FROM records
         WHERE active = 1
           AND tombstoned = 0
           AND cow_staged = 0
         ORDER BY version, record_id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;

    let mut records = Vec::new();
    for row in rows {
        let (record_id, wal_sequence, record_hash, body, record_json) = row?;
        let record: MemoryRecord = serde_json::from_str(&record_json)?;
        let (source_path, source_hash) = record
            .provenance
            .source_refs
            .first()
            .map_or((None, None), |source| {
                (Some(source.id.clone()), Some(source.hash.clone()))
            });
        records.push(ProjectionRecord {
            cursor: ProjectionCursor {
                record_id: parse_record_id(&record_id)?,
                wal_sequence: checked_i64_to_u64(wal_sequence, "version")?,
                record_hash,
                source_hash: None,
            },
            body,
            source_path,
            source_hash,
        });
    }
    Ok(records)
}

fn summary_for_target<'a, I>(
    conn: &rusqlite::Connection,
    target: ProjectionTarget,
    records: I,
) -> Result<ProjectionSummary, StoreError>
where
    I: IntoIterator<Item = (&'a str, &'a str, &'a str)>,
{
    let target_key = target.as_key();
    let mut states = Vec::new();
    let mut last_successful_rebuild_at: Option<String> = None;
    for (record_id, record_hash, source_hash) in records {
        let ledger = conn
            .query_row(
                "SELECT record_hash, state, reason, updated_at
                 FROM projection_ledger
                 WHERE target = ?1
                   AND record_id = ?2
                 ORDER BY
                   CASE
                     WHEN record_hash = ?3 AND source_hash = ?4 THEN 0
                     WHEN source_hash = ?4 THEN 1
                     ELSE 2
                   END,
                   updated_at DESC
                 LIMIT 1",
                params![target_key.as_str(), record_id, record_hash, source_hash],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        let state = match ledger {
            Some((hash, state, reason, updated_at)) if hash == record_hash => {
                let state = projection_state_from_row(&state, reason);
                if matches!(state, ProjectionItemState::Current) {
                    last_successful_rebuild_at = Some(
                        last_successful_rebuild_at
                            .map_or(updated_at.clone(), |current| current.max(updated_at)),
                    );
                }
                state
            }
            Some(_) => ProjectionItemState::Stale,
            None => ProjectionItemState::Missing,
        };
        states.push(state);
    }

    Ok(ProjectionSummary::from_rows(
        target,
        states.len(),
        states,
        last_successful_rebuild_at,
    ))
}

fn projection_state_parts(state: &ProjectionItemState) -> (&'static str, Option<&str>) {
    match state {
        ProjectionItemState::Current => ("current", None),
        ProjectionItemState::Stale => ("stale", None),
        ProjectionItemState::Missing => ("missing", None),
        ProjectionItemState::Failed { reason } => ("failed", Some(reason.as_str())),
        _ => ("failed", Some("unknown projection state")),
    }
}

fn projection_state_from_row(state: &str, reason: Option<String>) -> ProjectionItemState {
    match state {
        "current" => ProjectionItemState::Current,
        "stale" => ProjectionItemState::Stale,
        "missing" => ProjectionItemState::Missing,
        "failed" => ProjectionItemState::Failed {
            reason: reason.unwrap_or_else(|| "projection failed".to_owned()),
        },
        _ => ProjectionItemState::Failed {
            reason: format!("unknown projection state {state}"),
        },
    }
}

fn checked_i64_to_u64(value: i64, field: &str) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::Invariant {
        what: format!("{field} must be non-negative"),
    })
}

fn checked_u64_to_i64(value: u64, field: &str) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::Invariant {
        what: format!("{field} overflow"),
    })
}

fn parse_record_id(raw: &str) -> Result<RecordId, StoreError> {
    RecordId::parse(raw.to_owned()).map_err(|err| StoreError::Invariant {
        what: format!("invalid projection record_id `{raw}`: {err}"),
    })
}

fn parser_target_for_source(path: &str) -> Option<ProjectionTarget> {
    let extension = Path::new(path).extension()?.to_str()?;
    if extension.eq_ignore_ascii_case("pdf") {
        return Some(ProjectionTarget::Parser(ParserProjectionKind::PdfText));
    }
    if extension.eq_ignore_ascii_case("docx") {
        return Some(ProjectionTarget::Parser(ParserProjectionKind::DocxText));
    }
    if extension.eq_ignore_ascii_case("json") && path.to_ascii_lowercase().contains("frame") {
        return Some(ProjectionTarget::Parser(
            ParserProjectionKind::VideoFrameText,
        ));
    }
    if ["png", "jpg", "jpeg", "webp"]
        .iter()
        .any(|image_ext| extension.eq_ignore_ascii_case(image_ext))
    {
        return Some(ProjectionTarget::Parser(
            ParserProjectionKind::VisionCaption,
        ));
    }
    None
}
