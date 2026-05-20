//! `SQLite` record store for Cairn.
//!
//! This crate keeps `.cairn/cairn.db` authoritative and stores only local
//! record rows plus rebuildable projection ledger state.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use cairn_core::contract::memory_store::{
    Bm25sPreference, CONTRACT_VERSION, MemoryStore, MemoryStoreCapabilities, MemoryStoreError,
    ProjectionApplyItem, ProjectionRecord, RankingSignal, RankingSignalName, SearchHit, SearchMode,
    SearchRequest, SearchResponse,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::projection::{
    ParserProjectionKind, ProjectionCursor, ProjectionItemState, ProjectionLedgerRow,
    ProjectionSummary, ProjectionTarget,
};
use cairn_core::domain::record::RecordId;
use cairn_core::register_plugin;
use rusqlite::{Connection, OptionalExtension, params};

/// Stable plugin name. Matches `name = ...` in `plugin.toml`.
pub const PLUGIN_NAME: &str = "cairn-store-sqlite";

/// Plugin capability manifest TOML (parsed at registration time).
pub const MANIFEST_TOML: &str = include_str!("../plugin.toml");

/// Contract-version range this crate accepts. Shared by the trait impl and
/// the compile-time guard below so the manifest range and the trait surface
/// derive from one binding.
pub const ACCEPTED_RANGE: VersionRange =
    VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));

const SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS records (
    record_id TEXT PRIMARY KEY,
    body TEXT NOT NULL,
    wal_sequence INTEGER NOT NULL,
    record_hash TEXT NOT NULL,
    source_path TEXT,
    source_hash TEXT,
    active INTEGER NOT NULL DEFAULT 1,
    tombstoned INTEGER NOT NULL DEFAULT 0
);
CREATE VIRTUAL TABLE IF NOT EXISTS records_fts USING fts5(record_id UNINDEXED, body);
CREATE TABLE IF NOT EXISTS projection_ledger (
    target TEXT NOT NULL,
    record_id TEXT NOT NULL,
    wal_sequence INTEGER NOT NULL,
    record_hash TEXT NOT NULL,
    source_hash TEXT NOT NULL DEFAULT '',
    state TEXT NOT NULL,
    reason TEXT,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (target, record_id, record_hash, source_hash)
);
";

/// `SQLite` backed `MemoryStore`.
///
/// The default value is unopened so plugin registration can construct the
/// adapter without choosing a vault path.
#[derive(Default)]
pub struct SqliteMemoryStore {
    conn: Option<Mutex<Connection>>,
}

impl SqliteMemoryStore {
    /// Open a `SQLite` store and create the minimal projection/search schema.
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        ensure_optional_column(&conn, "records", "source_path", "TEXT")?;
        ensure_optional_column(&conn, "records", "source_hash", "TEXT")?;
        Ok(Self {
            conn: Some(Mutex::new(conn)),
        })
    }

    /// Test helper for deterministic fixture records.
    pub fn insert_test_record(
        &self,
        record_id: &str,
        body: &str,
        wal_sequence: u64,
        record_hash: &str,
    ) -> Result<(), rusqlite::Error> {
        let wal_sequence =
            i64::try_from(wal_sequence).map_err(|_| rusqlite::Error::InvalidQuery)?;
        let conn = self
            .conn
            .as_ref()
            .ok_or_else(|| rusqlite::Error::InvalidQuery)?
            .lock()
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        conn.execute(
            "INSERT OR REPLACE INTO records(record_id, body, wal_sequence, record_hash, active, tombstoned)
             VALUES (?1, ?2, ?3, ?4, 1, 0)",
            params![record_id, body, wal_sequence, record_hash],
        )?;
        conn.execute(
            "DELETE FROM records_fts WHERE record_id = ?1",
            params![record_id],
        )?;
        conn.execute(
            "INSERT INTO records_fts(record_id, body) VALUES (?1, ?2)",
            params![record_id, body],
        )?;
        Ok(())
    }

    /// Test helper for deterministic fixture records with source metadata.
    pub fn insert_test_record_with_source(
        &self,
        record_id: &str,
        body: &str,
        wal_sequence: u64,
        record_hash: &str,
        source_path: &str,
        source_hash: &str,
    ) -> Result<(), rusqlite::Error> {
        self.insert_test_record(record_id, body, wal_sequence, record_hash)?;
        let conn = self
            .conn
            .as_ref()
            .ok_or_else(|| rusqlite::Error::InvalidQuery)?
            .lock()
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        conn.execute(
            "UPDATE records SET source_path = ?2, source_hash = ?3 WHERE record_id = ?1",
            params![record_id, source_path, source_hash],
        )?;
        Ok(())
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, MemoryStoreError> {
        self.conn
            .as_ref()
            .ok_or_else(|| {
                MemoryStoreError::CapabilityUnavailable("sqlite store is not opened".to_owned())
            })?
            .lock()
            .map_err(|_| MemoryStoreError::Store("sqlite mutex poisoned".to_owned()))
    }
}

fn sqlite_error(err: &rusqlite::Error) -> MemoryStoreError {
    MemoryStoreError::Store(err.to_string())
}

fn ensure_optional_column(
    conn: &Connection,
    table: &str,
    column: &str,
    ty: &str,
) -> Result<(), rusqlite::Error> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for row in rows {
        if row? == column {
            return Ok(());
        }
    }
    conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {ty}"), [])?;
    Ok(())
}

fn parse_record_id(raw: String) -> Result<RecordId, MemoryStoreError> {
    RecordId::parse(raw).map_err(|err| MemoryStoreError::Store(err.to_string()))
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

fn checked_i64_to_u64(value: i64, field: &str) -> Result<u64, MemoryStoreError> {
    u64::try_from(value)
        .map_err(|_| MemoryStoreError::Store(format!("{field} must be non-negative")))
}

fn checked_u64_to_i64(value: u64, field: &str) -> Result<i64, MemoryStoreError> {
    i64::try_from(value).map_err(|_| MemoryStoreError::Store(format!("{field} overflow")))
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

fn current_projection_records(
    conn: &Connection,
) -> Result<Vec<ProjectionRecord>, MemoryStoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT record_id, wal_sequence, record_hash, body, source_path, source_hash
             FROM records
             WHERE active = 1 AND tombstoned = 0
             ORDER BY wal_sequence, record_id",
        )
        .map_err(|err| sqlite_error(&err))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })
        .map_err(|err| sqlite_error(&err))?;

    let mut records = Vec::new();
    for row in rows {
        let (record_id, wal_sequence, record_hash, body, source_path, source_hash) =
            row.map_err(|err| sqlite_error(&err))?;
        records.push(ProjectionRecord {
            cursor: ProjectionCursor {
                record_id: parse_record_id(record_id)?,
                wal_sequence: checked_i64_to_u64(wal_sequence, "wal_sequence")?,
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
    conn: &Connection,
    target: ProjectionTarget,
    records: I,
) -> Result<ProjectionSummary, MemoryStoreError>
where
    I: IntoIterator<Item = (&'a str, &'a str, &'a str)>,
{
    let target_key = target.as_key();
    let mut states = Vec::new();
    let mut last_successful_rebuild_at = None;
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
            .optional()
            .map_err(|err| sqlite_error(&err))?;
        let state = match ledger {
            Some((hash, state, reason, updated_at)) if hash == record_hash => {
                let state = projection_state_from_row(&state, reason);
                if matches!(state, ProjectionItemState::Current) {
                    last_successful_rebuild_at = Some(
                        last_successful_rebuild_at.map_or(updated_at.clone(), |current| {
                            std::cmp::max(current, updated_at)
                        }),
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

#[async_trait::async_trait]
impl MemoryStore for SqliteMemoryStore {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn capabilities(&self) -> &MemoryStoreCapabilities {
        static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
            fts: true,
            vector: false,
            graph_edges: false,
            transactions: false,
        };
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        ACCEPTED_RANGE
    }

    async fn search(&self, request: SearchRequest) -> Result<SearchResponse, MemoryStoreError> {
        if matches!(request.bm25s, Bm25sPreference::Required) {
            return Err(MemoryStoreError::CapabilityUnavailable(
                "nexus bm25s ranking".to_owned(),
            ));
        }
        if matches!(request.mode, SearchMode::Semantic) {
            return Err(MemoryStoreError::CapabilityUnavailable(
                "semantic search".to_owned(),
            ));
        }

        let conn = self.connection()?;
        let mut stmt = conn
            .prepare(
                "SELECT records_fts.record_id, r.record_hash, records_fts.body, bm25(records_fts) AS score
                 FROM records_fts
                 JOIN records AS r ON r.record_id = records_fts.record_id
                 WHERE records_fts MATCH ?1
                   AND r.active = 1
                   AND r.tombstoned = 0
                 ORDER BY score
                 LIMIT ?2",
            )
            .map_err(|err| sqlite_error(&err))?;
        let rows = stmt
            .query_map(params![request.query, i64::from(request.limit)], |row| {
                let score = row.get::<_, f64>(3)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    score,
                ))
            })
            .map_err(|err| sqlite_error(&err))?;

        let mut hits = Vec::new();
        for row in rows {
            let (record_id, record_hash, body, score) = row.map_err(|err| sqlite_error(&err))?;
            hits.push(SearchHit {
                record_id: parse_record_id(record_id)?,
                record_hash,
                score: -score,
                snippet: Some(body),
                ranking_signals: vec![RankingSignal {
                    name: RankingSignalName::SqliteFts5,
                    used: true,
                    score: Some(-score),
                    reason: None,
                }],
            });
        }

        Ok(SearchResponse { hits })
    }

    async fn projection_summaries(&self) -> Result<Vec<ProjectionSummary>, MemoryStoreError> {
        let conn = self.connection()?;
        let records = current_projection_records(&conn)?;
        let mut summaries = Vec::new();
        summaries.push(summary_for_target(
            &conn,
            ProjectionTarget::Bm25sLexical,
            records.iter().map(|record| {
                (
                    record.cursor.record_id.as_str(),
                    record.cursor.record_hash.as_str(),
                    "",
                )
            }),
        )?);

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
                    (parser_target_for_source(source_path).as_ref() == Some(&target)).then_some((
                        record.cursor.record_id.as_str(),
                        record.cursor.record_hash.as_str(),
                        source_hash,
                    ))
                })
                .collect::<Vec<_>>();
            let summary = summary_for_target(&conn, target, target_records)?;
            if summary.total_authoritative_items > 0
                || summary.current_items > 0
                || summary.lagging_items > 0
                || summary.failed_items > 0
            {
                summaries.push(summary);
            }
        }

        Ok(summaries)
    }

    async fn projection_cursors(&self) -> Result<Vec<ProjectionCursor>, MemoryStoreError> {
        Ok(self
            .projection_records()
            .await?
            .into_iter()
            .map(|record| record.cursor)
            .collect())
    }

    async fn projection_records(&self) -> Result<Vec<ProjectionRecord>, MemoryStoreError> {
        let conn = self.connection()?;
        current_projection_records(&conn)
    }

    async fn projection_failures(&self) -> Result<Vec<ProjectionLedgerRow>, MemoryStoreError> {
        let conn = self.connection()?;
        let mut stmt = conn
            .prepare(
                "SELECT l.target, l.record_id, l.wal_sequence, l.record_hash, l.source_hash, l.reason, l.updated_at
                 FROM projection_ledger AS l
                 JOIN records AS r
                   ON r.record_id = l.record_id
                  AND r.record_hash = l.record_hash
                  AND (l.source_hash = '' OR COALESCE(r.source_hash, '') = l.source_hash)
                 WHERE l.state = 'failed'
                   AND r.active = 1
                   AND r.tombstoned = 0
                 ORDER BY l.target, l.record_id, l.source_hash",
            )
            .map_err(|err| sqlite_error(&err))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })
            .map_err(|err| sqlite_error(&err))?;

        let mut failures = Vec::new();
        for row in rows {
            let (target, record_id, wal_sequence, record_hash, source_hash, reason, updated_at) =
                row.map_err(|err| sqlite_error(&err))?;
            let target = ProjectionTarget::from_key(&target).ok_or_else(|| {
                MemoryStoreError::Store(format!("unknown projection target {target}"))
            })?;
            failures.push(ProjectionLedgerRow {
                target,
                cursor: ProjectionCursor {
                    record_id: parse_record_id(record_id)?,
                    wal_sequence: checked_i64_to_u64(wal_sequence, "wal_sequence")?,
                    record_hash,
                    source_hash: (!source_hash.is_empty()).then_some(source_hash),
                },
                state: ProjectionItemState::Failed {
                    reason: reason.unwrap_or_else(|| "projection failed".to_owned()),
                },
                updated_at,
            });
        }
        Ok(failures)
    }

    async fn apply_projection_items(
        &self,
        items: Vec<ProjectionApplyItem>,
    ) -> Result<(), MemoryStoreError> {
        let conn = self.connection()?;
        for item in items {
            let row = item.row;
            let target = row.target.as_key();
            let source_hash = row.cursor.source_hash.unwrap_or_default();
            let (state, reason) = projection_state_parts(&row.state);
            let wal_sequence = checked_u64_to_i64(row.cursor.wal_sequence, "wal_sequence")?;
            conn.execute(
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
            )
            .map_err(|err| sqlite_error(&err))?;
        }
        Ok(())
    }
}

// Compile-time guard: this crate's accepted range must include the host
// CONTRACT_VERSION. If we ever bump CONTRACT_VERSION without bumping the
// range, the const evaluation here panics at build.
const _: () = assert!(
    ACCEPTED_RANGE.accepts(CONTRACT_VERSION),
    "host CONTRACT_VERSION outside this crate's declared range"
);

register_plugin!(
    MemoryStore,
    SqliteMemoryStore,
    "cairn-store-sqlite",
    MANIFEST_TOML
);
