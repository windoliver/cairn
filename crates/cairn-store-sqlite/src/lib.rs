//! `SQLite` record store for Cairn.
//!
//! This crate keeps `.cairn/cairn.db` authoritative and stores only local
//! record rows plus rebuildable projection ledger state.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use cairn_core::contract::memory_store::{
    CONTRACT_VERSION, MemoryStore, MemoryStoreCapabilities, MemoryStoreError, ProjectionApplyItem,
    RankingSignal, RankingSignalName, SearchHit, SearchMode, SearchRequest, SearchResponse,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::projection::{
    ProjectionCursor, ProjectionItemState, ProjectionSummary, ProjectionTarget,
};
use cairn_core::domain::record::RecordId;
use cairn_core::register_plugin;
use rusqlite::{Connection, params};

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

#[async_trait::async_trait]
impl MemoryStore for SqliteMemoryStore {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn capabilities(&self) -> &MemoryStoreCapabilities {
        static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
            fts: false,
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
        if matches!(request.mode, SearchMode::Semantic) {
            return Err(MemoryStoreError::CapabilityUnavailable(
                "semantic search".to_owned(),
            ));
        }

        let conn = self.connection()?;
        let mut stmt = conn
            .prepare(
                "SELECT record_id, body, bm25(records_fts) AS score
                 FROM records_fts
                 WHERE records_fts MATCH ?1
                 ORDER BY score
                 LIMIT ?2",
            )
            .map_err(|err| sqlite_error(&err))?;
        let rows = stmt
            .query_map(params![request.query, i64::from(request.limit)], |row| {
                let score = row.get::<_, f64>(2)?;
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, score))
            })
            .map_err(|err| sqlite_error(&err))?;

        let mut hits = Vec::new();
        for row in rows {
            let (record_id, body, score) = row.map_err(|err| sqlite_error(&err))?;
            hits.push(SearchHit {
                record_id: parse_record_id(record_id)?,
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
        let target = ProjectionTarget::Bm25sLexical;
        let target_key = target.as_key();
        let total = conn
            .query_row(
                "SELECT COUNT(*) FROM records WHERE active = 1 AND tombstoned = 0",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|err| sqlite_error(&err))?;
        let mut stmt = conn
            .prepare(
                "SELECT r.record_id, r.record_hash, l.record_hash, l.state, l.reason
                 FROM records AS r
                 LEFT JOIN projection_ledger AS l
                   ON l.target = ?1 AND l.record_id = r.record_id AND l.source_hash = ''
                 WHERE r.active = 1 AND r.tombstoned = 0",
            )
            .map_err(|err| sqlite_error(&err))?;
        let rows = stmt
            .query_map(params![target_key.as_str()], |row| {
                Ok((
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })
            .map_err(|err| sqlite_error(&err))?;
        let mut states = Vec::new();
        for row in rows {
            let (current_hash, ledger_hash, ledger_state, reason) =
                row.map_err(|err| sqlite_error(&err))?;
            let state = match (ledger_hash, ledger_state) {
                (Some(hash), Some(state)) if hash == current_hash => {
                    projection_state_from_row(&state, reason)
                }
                (Some(_), Some(_)) => ProjectionItemState::Stale,
                _ => ProjectionItemState::Missing,
            };
            states.push(state);
        }
        let last_successful_rebuild_at = conn
            .query_row(
                "SELECT MAX(updated_at) FROM projection_ledger WHERE target = ?1 AND state = 'current'",
                params![target_key.as_str()],
                |row| row.get::<_, Option<String>>(0),
            )
            .map_err(|err| sqlite_error(&err))?;

        Ok(vec![ProjectionSummary::from_rows(
            target,
            usize::try_from(checked_i64_to_u64(total, "record count")?)
                .map_err(|_| MemoryStoreError::Store("record count overflow".to_owned()))?,
            states,
            last_successful_rebuild_at,
        )])
    }

    async fn projection_cursors(&self) -> Result<Vec<ProjectionCursor>, MemoryStoreError> {
        let conn = self.connection()?;
        let mut stmt = conn
            .prepare(
                "SELECT record_id, wal_sequence, record_hash
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
                ))
            })
            .map_err(|err| sqlite_error(&err))?;

        let mut cursors = Vec::new();
        for row in rows {
            let (record_id, wal_sequence, record_hash) = row.map_err(|err| sqlite_error(&err))?;
            cursors.push(ProjectionCursor {
                record_id: parse_record_id(record_id)?,
                wal_sequence: checked_i64_to_u64(wal_sequence, "wal_sequence")?,
                record_hash,
                source_hash: None,
            });
        }
        Ok(cursors)
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
