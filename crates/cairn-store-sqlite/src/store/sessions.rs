//! Session storage (brief §8.1).
//!
//! The pure resolver lives in [`cairn_core::domain::session`]. This module
//! is the persistence half: locating the most recent active session for an
//! identity, minting new ones, bumping `last_activity_at`, and ending them.
//!
//! Methods are inherent on [`SqliteMemoryStore`] rather than on the
//! [`MemoryStore`] trait: P0 deliberately keeps session storage out of the
//! trait surface so future stores (fixture, remote) don't have to implement
//! it. The verb dispatcher reaches into the concrete store, the same way
//! [`SqliteMemoryStore::with_tx`] is reached.
//!
//! [`MemoryStore`]: cairn_core::contract::memory_store::MemoryStore

use cairn_core::domain::session::{LastActiveSession, Session, SessionId, SessionIdentity};
use rusqlite::{OptionalExtension, params};
use tracing::instrument;
use ulid::Ulid;

use crate::error::StoreError;
use crate::store::{SqliteMemoryStore, current_unix_ms};

/// Subset of session metadata accepted at create time. All fields default
/// to "unset" — the resolver / verb layer fills only what it has.
#[derive(Debug, Default, Clone)]
pub struct NewSessionMetadata {
    /// Optional channel hint (`"chat"`, `"voice"`, …).
    pub channel: Option<String>,
    /// Optional priority hint.
    pub priority: Option<String>,
    /// Optional tag list. Empty when unset.
    pub tags: Vec<String>,
}

impl SqliteMemoryStore {
    /// Look up the most recent active session for `(user, agent, project_root)`.
    ///
    /// Returns `Ok(None)` when no row matches or all matching rows have
    /// `ended_at IS NOT NULL`. The returned `idle_secs` is computed
    /// against the current wall clock; the pure resolver in
    /// [`cairn_core::domain::session::resolve_session`] consumes it and
    /// decides reuse-vs-create against an idle window.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotInitialized`] when the store was constructed
    /// via `Default::default()`. Returns [`StoreError::Worker`] /
    /// [`StoreError::Sqlite`] for SQL failures.
    #[instrument(
        skip(self, identity),
        err,
        fields(
            verb = "find_active_session",
            user = %identity.user,
            agent = %identity.agent,
        ),
    )]
    pub async fn find_active_session(
        &self,
        identity: &SessionIdentity,
    ) -> Result<Option<LastActiveSession>, StoreError> {
        let conn = self.require_conn("find_active_session")?.clone();
        let user = identity.user.as_str().to_owned();
        let agent = identity.agent.as_str().to_owned();
        let project_root = identity.project_root.clone();
        let now_ms = current_unix_ms();

        let row = conn
            .call(move |c| {
                // `IS` (rather than `=`) so NULL == NULL matches when
                // project_root is unset on both the query and the row.
                let res = c
                    .query_row(
                        "SELECT session_id, last_activity_at FROM sessions \
                         WHERE user_id = ?1 AND agent_id = ?2 \
                           AND project_root IS ?3 \
                           AND ended_at IS NULL \
                         ORDER BY last_activity_at DESC \
                         LIMIT 1",
                        params![user, agent, project_root],
                        |r| {
                            let id: String = r.get(0)?;
                            let last: i64 = r.get(1)?;
                            Ok((id, last))
                        },
                    )
                    .optional()?;
                Ok::<_, tokio_rusqlite::Error>(res)
            })
            .await?;

        let Some((id, last_activity_ms)) = row else {
            return Ok(None);
        };

        let id = SessionId::parse(id).map_err(|e| StoreError::Invariant {
            what: format!("session_id round-trip failed: {e}"),
        })?;
        // Subtract last_activity_at from now; clamp at 0 if clock went
        // backwards (last activity recorded under a future skewed clock).
        let idle_secs =
            u64::try_from((now_ms - last_activity_ms).max(0) / 1000).unwrap_or(u64::MAX);

        Ok(Some(LastActiveSession { id, idle_secs }))
    }

    /// Mint a new session row for `identity` with the given metadata.
    /// Generates a fresh ULID and stamps `created_at = last_activity_at = now`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotInitialized`] when the store was constructed
    /// via `Default::default()`. Returns [`StoreError::Worker`] /
    /// [`StoreError::Sqlite`] for SQL failures.
    #[instrument(
        skip(self, identity, metadata),
        err,
        fields(
            verb = "create_session",
            user = %identity.user,
            agent = %identity.agent,
        ),
    )]
    pub async fn create_session(
        &self,
        identity: &SessionIdentity,
        metadata: NewSessionMetadata,
    ) -> Result<Session, StoreError> {
        let conn = self.require_conn("create_session")?.clone();
        let id_str = Ulid::new().to_string();
        let id = SessionId::parse(&id_str).map_err(|e| StoreError::Invariant {
            what: format!("freshly-minted ULID rejected by SessionId::parse: {e}"),
        })?;
        let now_ms = current_unix_ms();

        let user = identity.user.as_str().to_owned();
        let agent = identity.agent.as_str().to_owned();
        let project_root = identity.project_root.clone();
        let channel = metadata.channel.clone();
        let priority = metadata.priority.clone();
        let tags_json = if metadata.tags.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&metadata.tags)?)
        };
        let id_for_sql = id_str.clone();

        conn.call(move |c| {
            c.execute(
                "INSERT INTO sessions \
                   (session_id, user_id, agent_id, project_root, title, \
                    channel, priority, tags, metadata_json, \
                    created_at, last_activity_at, ended_at) \
                 VALUES (?1, ?2, ?3, ?4, '', ?5, ?6, ?7, NULL, ?8, ?8, NULL)",
                params![
                    id_for_sql,
                    user,
                    agent,
                    project_root,
                    channel,
                    priority,
                    tags_json,
                    now_ms,
                ],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await?;

        Ok(Session {
            id,
            identity: identity.clone(),
            title: String::new(),
            channel: metadata.channel,
            priority: metadata.priority,
            tags: metadata.tags,
            created_at_unix_ms: now_ms,
            last_activity_at_unix_ms: now_ms,
            ended_at_unix_ms: None,
        })
    }

    /// Bump `last_activity_at` on the named session to "now". Returns
    /// `Ok(false)` if the session id does not exist or has already ended;
    /// `Ok(true)` when a row was updated.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotInitialized`] when the store was constructed
    /// via `Default::default()`. Returns [`StoreError::Worker`] /
    /// [`StoreError::Sqlite`] for SQL failures.
    #[instrument(
        skip(self),
        err,
        fields(verb = "touch_session", session_id = %id.as_str()),
    )]
    pub async fn touch_session(&self, id: &SessionId) -> Result<bool, StoreError> {
        let conn = self.require_conn("touch_session")?.clone();
        let key = id.as_str().to_owned();
        let now_ms = current_unix_ms();
        let n = conn
            .call(move |c| {
                let n = c.execute(
                    "UPDATE sessions SET last_activity_at = ?1 \
                     WHERE session_id = ?2 AND ended_at IS NULL",
                    params![now_ms, key],
                )?;
                Ok::<_, tokio_rusqlite::Error>(n)
            })
            .await?;
        Ok(n > 0)
    }

    /// Mark the session `ended_at = now`. Idempotent: ending an
    /// already-ended session is a no-op (`Ok(false)`).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotInitialized`] when the store was constructed
    /// via `Default::default()`. Returns [`StoreError::Worker`] /
    /// [`StoreError::Sqlite`] for SQL failures.
    #[instrument(
        skip(self),
        err,
        fields(verb = "end_session", session_id = %id.as_str()),
    )]
    pub async fn end_session(&self, id: &SessionId) -> Result<bool, StoreError> {
        let conn = self.require_conn("end_session")?.clone();
        let key = id.as_str().to_owned();
        let now_ms = current_unix_ms();
        let n = conn
            .call(move |c| {
                let n = c.execute(
                    "UPDATE sessions SET ended_at = ?1 \
                     WHERE session_id = ?2 AND ended_at IS NULL",
                    params![now_ms, key],
                )?;
                Ok::<_, tokio_rusqlite::Error>(n)
            })
            .await?;
        Ok(n > 0)
    }

    /// Fetch a single session by id, regardless of `ended_at` state.
    /// Returns `Ok(None)` when the row does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotInitialized`] when the store was constructed
    /// via `Default::default()`. Returns [`StoreError::Worker`] /
    /// [`StoreError::Sqlite`] for SQL failures.
    #[instrument(
        skip(self),
        err,
        fields(verb = "get_session", session_id = %id.as_str()),
    )]
    pub async fn get_session(&self, id: &SessionId) -> Result<Option<Session>, StoreError> {
        let conn = self.require_conn("get_session")?.clone();
        let key = id.as_str().to_owned();
        let row = conn
            .call(move |c| {
                let res = c
                    .query_row(
                        "SELECT session_id, user_id, agent_id, project_root, \
                                title, channel, priority, tags, \
                                created_at, last_activity_at, ended_at \
                         FROM sessions WHERE session_id = ?1",
                        params![key],
                        |r| {
                            let id: String = r.get(0)?;
                            let user: String = r.get(1)?;
                            let agent: String = r.get(2)?;
                            let project_root: Option<String> = r.get(3)?;
                            let title: String = r.get(4)?;
                            let channel: Option<String> = r.get(5)?;
                            let priority: Option<String> = r.get(6)?;
                            let tags_json: Option<String> = r.get(7)?;
                            let created_at: i64 = r.get(8)?;
                            let last_activity: i64 = r.get(9)?;
                            let ended: Option<i64> = r.get(10)?;
                            Ok((
                                id,
                                user,
                                agent,
                                project_root,
                                title,
                                channel,
                                priority,
                                tags_json,
                                created_at,
                                last_activity,
                                ended,
                            ))
                        },
                    )
                    .optional()?;
                Ok::<_, tokio_rusqlite::Error>(res)
            })
            .await?;

        let Some((
            id_str,
            user,
            agent,
            project_root,
            title,
            channel,
            priority,
            tags_json,
            created_at,
            last_activity,
            ended,
        )) = row
        else {
            return Ok(None);
        };

        let id = SessionId::parse(&id_str).map_err(|e| StoreError::Invariant {
            what: format!("session_id round-trip failed: {e}"),
        })?;
        let user =
            cairn_core::domain::Identity::parse(&user).map_err(|e| StoreError::Invariant {
                what: format!("session.user_id round-trip failed: {e}"),
            })?;
        let agent =
            cairn_core::domain::Identity::parse(&agent).map_err(|e| StoreError::Invariant {
                what: format!("session.agent_id round-trip failed: {e}"),
            })?;
        let identity =
            SessionIdentity::new(user, agent, project_root).map_err(|e| StoreError::Invariant {
                what: format!("session identity round-trip failed: {e}"),
            })?;
        let tags: Vec<String> = tags_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?
            .unwrap_or_default();

        Ok(Some(Session {
            id,
            identity,
            title,
            channel,
            priority,
            tags,
            created_at_unix_ms: created_at,
            last_activity_at_unix_ms: last_activity,
            ended_at_unix_ms: ended,
        }))
    }
}
