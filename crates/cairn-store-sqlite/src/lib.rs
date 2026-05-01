//! `SQLite` record store for Cairn (P0 scaffold).
//!
//! Full record CRUD, FTS5 and sqlite-vec integration arrive in follow-up
//! issues (#46 and later). This crate owns the SQLite-backed session registry
//! for §8.1 so hooks and verbs can share stable session creation semantics.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use cairn_core::contract::memory_store::{CONTRACT_VERSION, MemoryStore, MemoryStoreCapabilities};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::register_plugin;
use cairn_core::session::{
    ResolveSessionRequest, ResolvedSession, SelectedSessionId, SessionError, SessionRecord,
    SessionResolutionSource,
};
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, params};

/// Stable plugin name. Matches `name = ...` in `plugin.toml`.
pub const PLUGIN_NAME: &str = "cairn-store-sqlite";

/// Plugin capability manifest TOML (parsed at registration time).
pub const MANIFEST_TOML: &str = include_str!("../plugin.toml");

/// Contract-version range this crate accepts. Shared by the trait impl and
/// the compile-time guard below so the manifest range and the trait surface
/// derive from one binding.
pub const ACCEPTED_RANGE: VersionRange =
    VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));

/// Errors raised while opening or initializing the `SQLite` store.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SqliteStoreError {
    /// `SQLite` returned an error.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// P0 `MemoryStore` with a SQLite-backed session registry.
///
/// `Default` intentionally leaves the connection unopened for plugin
/// registration. Hosts open a concrete vault path before calling mutating
/// methods.
#[derive(Default)]
pub struct SqliteMemoryStore {
    conn: Option<Mutex<Connection>>,
}

impl SqliteMemoryStore {
    /// Open an in-memory `SQLite` store.
    ///
    /// This is primarily useful for conformance and fixture tests.
    ///
    /// # Errors
    /// Returns any `SQLite` error from opening the database or creating schema.
    pub fn open_in_memory() -> Result<Self, SqliteStoreError> {
        let conn = Connection::open_in_memory()?;
        initialize_schema(&conn)?;
        Ok(Self {
            conn: Some(Mutex::new(conn)),
        })
    }

    /// Open a `SQLite` store at `db_path`.
    ///
    /// The parent directory must already exist; vault initialization owns
    /// directory creation.
    ///
    /// # Errors
    /// Returns any `SQLite` error from opening the database or creating schema.
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self, SqliteStoreError> {
        let conn = Connection::open(db_path)?;
        initialize_schema(&conn)?;
        Ok(Self {
            conn: Some(Mutex::new(conn)),
        })
    }

    /// Return a persisted session by ID.
    ///
    /// # Errors
    /// Returns [`SessionError`] when the store is unopened or `SQLite` fails.
    pub fn session_by_id(&self, session_id: &str) -> Result<Option<SessionRecord>, SessionError> {
        let conn = self.connection()?;
        query_session_by_id(&conn, session_id)
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, SessionError> {
        let Some(conn) = &self.conn else {
            return Err(SessionError::StoreUnavailable {
                store: PLUGIN_NAME.to_owned(),
            });
        };
        conn.lock().map_err(|_| SessionError::Store {
            message: "sqlite connection mutex poisoned".to_owned(),
        })
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

    fn resolve_session(
        &self,
        request: &ResolveSessionRequest,
    ) -> Result<ResolvedSession, SessionError> {
        request.validate()?;

        let mut conn = self.connection()?;
        let tx = conn.transaction().map_err(to_session_store_error)?;

        let resolved = if let Some(selected) = request.candidates.select_direct()? {
            resolve_direct(&tx, request, &selected)?
        } else {
            resolve_auto(&tx, request)?
        };

        tx.commit().map_err(to_session_store_error)?;
        Ok(resolved)
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

fn initialize_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r"
        CREATE TABLE IF NOT EXISTS sessions (
            session_id TEXT PRIMARY KEY NOT NULL,
            user_id TEXT NOT NULL,
            agent_id TEXT NOT NULL,
            vault_id TEXT,
            project_id TEXT,
            cwd TEXT,
            title TEXT NOT NULL,
            channel TEXT,
            priority TEXT,
            tags_json TEXT NOT NULL,
            created_at_unix_millis INTEGER NOT NULL,
            last_activity_at_unix_millis INTEGER NOT NULL,
            ended_at_unix_millis INTEGER
        );

        CREATE INDEX IF NOT EXISTS idx_sessions_active_context
            ON sessions (
                user_id,
                agent_id,
                vault_id,
                project_id,
                ended_at_unix_millis,
                last_activity_at_unix_millis DESC
            );
        ",
    )
}

fn resolve_direct(
    tx: &rusqlite::Transaction<'_>,
    request: &ResolveSessionRequest,
    selected: &SelectedSessionId,
) -> Result<ResolvedSession, SessionError> {
    if query_session_by_id(tx, &selected.session_id)?.is_some() {
        tx.execute(
            r"
            UPDATE sessions
               SET last_activity_at_unix_millis = ?1,
                   ended_at_unix_millis = NULL
             WHERE session_id = ?2
            ",
            params![request.now_unix_millis, selected.session_id],
        )
        .map_err(to_session_store_error)?;

        let record = query_session_by_id(tx, &selected.session_id)?
            .ok_or_else(|| missing_after_write(&selected.session_id))?;
        return Ok(ResolvedSession {
            session_id: record.session_id.clone(),
            created: false,
            source: selected.source,
            record,
        });
    }

    let record = insert_session(tx, request, &selected.session_id, selected.source)?;
    Ok(ResolvedSession {
        session_id: record.session_id.clone(),
        created: true,
        source: selected.source,
        record,
    })
}

fn resolve_auto(
    tx: &rusqlite::Transaction<'_>,
    request: &ResolveSessionRequest,
) -> Result<ResolvedSession, SessionError> {
    let mut candidates = query_active_sessions(tx, request)?;

    if request.context.project_id.is_none() && candidates.len() > 1 {
        return Err(SessionError::AmbiguousContext {
            reason: "multiple active sessions match user_id and agent_id; provide explicit session or project context".to_owned(),
        });
    }

    if let Some(record) = candidates.pop() {
        if request.now_unix_millis - record.last_activity_at_unix_millis
            <= request.idle_window_millis
        {
            touch_session(tx, &record.session_id, request.now_unix_millis)?;
            let record = query_session_by_id(tx, &record.session_id)?
                .ok_or_else(|| missing_after_write(&record.session_id))?;
            return Ok(ResolvedSession {
                session_id: record.session_id.clone(),
                created: false,
                source: SessionResolutionSource::AutoDiscovery,
                record,
            });
        }

        end_session(tx, &record.session_id, request.now_unix_millis)?;
    }

    let session_id = ulid::Ulid::new().to_string();
    let record = insert_session(
        tx,
        request,
        &session_id,
        SessionResolutionSource::AutoCreate,
    )?;
    Ok(ResolvedSession {
        session_id: record.session_id.clone(),
        created: true,
        source: SessionResolutionSource::AutoCreate,
        record,
    })
}

fn query_active_sessions(
    conn: &Connection,
    request: &ResolveSessionRequest,
) -> Result<Vec<SessionRecord>, SessionError> {
    let mut records = Vec::new();
    if let Some(project_id) = &request.context.project_id {
        let mut stmt = conn
            .prepare(
                r"
                SELECT session_id, user_id, agent_id, vault_id, project_id, cwd,
                       title, channel, priority, tags_json, created_at_unix_millis,
                       last_activity_at_unix_millis, ended_at_unix_millis
                  FROM sessions
                 WHERE user_id = ?1
                   AND agent_id = ?2
                   AND ((?3 IS NULL AND vault_id IS NULL) OR vault_id = ?3)
                   AND project_id = ?4
                   AND ended_at_unix_millis IS NULL
                 ORDER BY last_activity_at_unix_millis DESC, created_at_unix_millis DESC
                 LIMIT 1
                ",
            )
            .map_err(to_session_store_error)?;
        let rows = stmt
            .query_map(
                params![
                    request.context.user_id,
                    request.context.agent_id,
                    request.context.vault_id,
                    project_id
                ],
                record_from_row,
            )
            .map_err(to_session_store_error)?;
        for row in rows {
            records.push(row.map_err(to_session_store_error)?);
        }
        return Ok(records);
    }

    let mut stmt = conn
        .prepare(
            r"
            SELECT session_id, user_id, agent_id, vault_id, project_id, cwd,
                   title, channel, priority, tags_json, created_at_unix_millis,
                   last_activity_at_unix_millis, ended_at_unix_millis
              FROM sessions
             WHERE user_id = ?1
               AND agent_id = ?2
               AND ((?3 IS NULL AND vault_id IS NULL) OR vault_id = ?3)
               AND ended_at_unix_millis IS NULL
             ORDER BY last_activity_at_unix_millis DESC, created_at_unix_millis DESC
             LIMIT 2
            ",
        )
        .map_err(to_session_store_error)?;
    let rows = stmt
        .query_map(
            params![
                request.context.user_id,
                request.context.agent_id,
                request.context.vault_id
            ],
            record_from_row,
        )
        .map_err(to_session_store_error)?;
    for row in rows {
        records.push(row.map_err(to_session_store_error)?);
    }
    Ok(records)
}

fn query_session_by_id(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<SessionRecord>, SessionError> {
    conn.query_row(
        r"
        SELECT session_id, user_id, agent_id, vault_id, project_id, cwd,
               title, channel, priority, tags_json, created_at_unix_millis,
               last_activity_at_unix_millis, ended_at_unix_millis
          FROM sessions
         WHERE session_id = ?1
        ",
        params![session_id],
        record_from_row,
    )
    .optional()
    .map_err(to_session_store_error)
}

fn insert_session(
    tx: &rusqlite::Transaction<'_>,
    request: &ResolveSessionRequest,
    session_id: &str,
    _source: SessionResolutionSource,
) -> Result<SessionRecord, SessionError> {
    let tags_json =
        serde_json::to_string(&request.metadata.tags).map_err(to_session_store_error)?;

    tx.execute(
        r"
        INSERT INTO sessions (
            session_id, user_id, agent_id, vault_id, project_id, cwd,
            title, channel, priority, tags_json,
            created_at_unix_millis, last_activity_at_unix_millis,
            ended_at_unix_millis
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', ?7, ?8, ?9, ?10, ?10, NULL)
        ",
        params![
            session_id,
            request.context.user_id,
            request.context.agent_id,
            request.context.vault_id,
            request.context.project_id,
            request.context.cwd,
            request.metadata.channel,
            request.metadata.priority,
            tags_json,
            request.now_unix_millis,
        ],
    )
    .map_err(to_session_store_error)?;

    query_session_by_id(tx, session_id)?.ok_or_else(|| missing_after_write(session_id))
}

fn touch_session(
    tx: &rusqlite::Transaction<'_>,
    session_id: &str,
    now_unix_millis: i64,
) -> Result<(), SessionError> {
    tx.execute(
        r"
        UPDATE sessions
           SET last_activity_at_unix_millis = ?1
         WHERE session_id = ?2
        ",
        params![now_unix_millis, session_id],
    )
    .map_err(to_session_store_error)?;
    Ok(())
}

fn end_session(
    tx: &rusqlite::Transaction<'_>,
    session_id: &str,
    now_unix_millis: i64,
) -> Result<(), SessionError> {
    tx.execute(
        r"
        UPDATE sessions
           SET ended_at_unix_millis = ?1
         WHERE session_id = ?2
        ",
        params![now_unix_millis, session_id],
    )
    .map_err(to_session_store_error)?;
    Ok(())
}

fn record_from_row(row: &Row<'_>) -> rusqlite::Result<SessionRecord> {
    let tags_json: String = row.get(9)?;
    let tags = serde_json::from_str(&tags_json)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(9, Type::Text, Box::new(e)))?;

    Ok(SessionRecord {
        session_id: row.get(0)?,
        user_id: row.get(1)?,
        agent_id: row.get(2)?,
        vault_id: row.get(3)?,
        project_id: row.get(4)?,
        cwd: row.get(5)?,
        title: row.get(6)?,
        channel: row.get(7)?,
        priority: row.get(8)?,
        tags,
        created_at_unix_millis: row.get(10)?,
        last_activity_at_unix_millis: row.get(11)?,
        ended_at_unix_millis: row.get(12)?,
    })
}

fn missing_after_write(session_id: &str) -> SessionError {
    SessionError::Store {
        message: format!("session `{session_id}` missing after write"),
    }
}

fn to_session_store_error(error: impl std::fmt::Display) -> SessionError {
    SessionError::Store {
        message: error.to_string(),
    }
}
