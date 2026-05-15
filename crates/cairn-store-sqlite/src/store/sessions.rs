//! Session storage (brief §8.1).
//!
//! The pure resolver lives in [`cairn_core::domain::session`]. This module
//! is the persistence half: locating the most recent active session for an
//! identity, minting new ones, bumping `last_activity_at`, and ending them.
//!
//! Session lifecycle methods remain inherent on [`SqliteMemoryStore`].
//! Session-tree metadata is also exposed through the optional
//! [`MemoryStore`] trait methods added for the v0.3 substrate; adapters that
//! do not implement those methods keep the default capability-unavailable
//! behavior.
//!
//! [`MemoryStore`]: cairn_core::contract::memory_store::MemoryStore

use std::collections::BTreeSet;

use cairn_core::domain::session::{LastActiveSession, Session, SessionId, SessionIdentity};
use cairn_core::domain::{
    BranchKind, MergeStrategy, RecordId, SessionMerge, SessionTree, SessionTreeError,
};
use rusqlite::{OptionalExtension, params};
use tracing::instrument;
use ulid::Ulid;

use crate::error::StoreError;
use crate::store::{SqliteMemoryStore, current_unix_ms};

/// Outcome of [`SqliteMemoryStore::resolve_or_create_session`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResolveOutcome {
    /// An existing active session within the idle window was reused.
    /// `last_activity_at` has been bumped to "now".
    Reused(Session),
    /// No active session within the idle window existed; a fresh row was
    /// inserted and is returned. If a stale active row was found past the
    /// idle window, it has been marked `ended_at = now` in the same
    /// transaction so it cannot be revived by [`SqliteMemoryStore::touch_session`].
    Created(Session),
}

impl ResolveOutcome {
    /// Borrow the underlying session, regardless of whether it was reused
    /// or freshly created.
    #[must_use]
    pub fn session(&self) -> &Session {
        match self {
            Self::Reused(s) | Self::Created(s) => s,
        }
    }

    /// Consume the outcome and return the underlying session.
    #[must_use]
    pub fn into_session(self) -> Session {
        match self {
            Self::Reused(s) | Self::Created(s) => s,
        }
    }
}

/// Wall-clock deadline for retrying transient conflicts in
/// [`SqliteMemoryStore::resolve_or_create_session`].
///
/// Sized to be well past `busy_timeout=5s` (set in `open.rs`) so a single
/// long writer pinning the lock can't repeatedly trip both. After this
/// deadline, the operation surfaces [`StoreError::Busy`] and the caller
/// can decide whether to retry on the next user action.
pub const RESOLVE_BUSY_DEADLINE_MS: u64 = 7_500;

/// Constants for the truncated exponential backoff in
/// [`SqliteMemoryStore::resolve_or_create_session`]. Kept private — the
/// only knob external callers see is [`RESOLVE_BUSY_DEADLINE_MS`].
const RESOLVE_BUSY_DEADLINE: std::time::Duration =
    std::time::Duration::from_millis(RESOLVE_BUSY_DEADLINE_MS);
const INITIAL_BACKOFF_MS: u64 = 1;
const MAX_BACKOFF_MS: u64 = 32;

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

    /// Resolve an explicit session id, verifying it belongs to `expected`.
    ///
    /// Companion to [`SqliteMemoryStore::resolve_or_create_session`] for the
    /// `--session` / `CAIRN_SESSION_ID` / harness paths (brief §8.1). The
    /// CLI / SDK should never call into [`SqliteMemoryStore::touch_session`]
    /// or [`SqliteMemoryStore::end_session`] with a raw user-supplied id —
    /// a leaked or copied id from a different `(user, agent, project_root)`
    /// would otherwise let the caller hijack writes from another identity.
    ///
    /// Explicit ids are authoritative: callers who name a session expect
    /// that exact session, not a silently-substituted new one. A typo, a
    /// stale `CAIRN_SESSION_ID`, or a previously-ended row therefore fails
    /// closed (`SessionNotFound` / `SessionEnded`) rather than falling
    /// through to auto-discover.
    ///
    /// Atomicity: the lookup, identity check, and `last_activity_at` bump
    /// run inside a single `BEGIN IMMEDIATE` transaction with the same
    /// CAS-on-`last_activity_at` guard the resolve-or-create path uses.
    /// A concurrent `end_session` between our SELECT and UPDATE causes the
    /// CAS to match zero rows; we restart the tx, observe `ended_at IS NOT
    /// NULL`, and return [`StoreError::SessionEnded`] — never a closed row
    /// dressed up as live.
    ///
    /// # Errors
    ///
    /// - [`StoreError::NotInitialized`] when the store was constructed via
    ///   `Default::default()`.
    /// - [`StoreError::SessionNotFound`] when the id does not exist.
    /// - [`StoreError::SessionEnded`] when the row exists but has already
    ///   been closed.
    /// - [`StoreError::SessionIdentityMismatch`] when the row exists but
    ///   belongs to a different `(user, agent, project_root)`.
    /// - [`StoreError::Busy`] when sustained write contention exceeds the
    ///   retry deadline.
    /// - [`StoreError::Worker`] / [`StoreError::Sqlite`] for SQL failures.
    #[instrument(
        skip(self, expected),
        err,
        fields(
            verb = "resolve_explicit_session",
            session_id = %id.as_str(),
            user = %expected.user,
            agent = %expected.agent,
        ),
    )]
    #[allow(
        tail_expr_drop_order,
        reason = "drop order of the cloned tokio_rusqlite handle relative to the await temporary is benign — both are channel-backed clones with no observable side effects beyond worker shutdown, which the runtime handles regardless of order"
    )]
    pub async fn resolve_explicit_session(
        &self,
        id: &SessionId,
        expected: &SessionIdentity,
    ) -> Result<Session, StoreError> {
        let conn = self.require_conn("resolve_explicit_session")?.clone();
        let id_str = id.as_str().to_owned();
        let expected_clone = expected.clone();

        async_retry_busy("resolve_explicit_session", || {
            let conn = conn.clone();
            let id_str = id_str.clone();
            let expected_clone = expected_clone.clone();
            async move {
                conn.call(move |c| {
                    Ok::<_, tokio_rusqlite::Error>(single_attempt_resolve_explicit(
                        c,
                        &id_str,
                        &expected_clone,
                    ))
                })
                .await
                .map_err(StoreError::from)?
            }
        })
        .await
    }

    /// Atomically resolve-or-create the active session for `identity`.
    ///
    /// Replaces the racy `find_active_session → resolve_session → create_session`
    /// dance with a single transaction:
    ///
    /// 1. `SELECT` the most recent `ended_at IS NULL` row for the identity.
    /// 2. If one exists and is within `idle_window_secs`, bump
    ///    `last_activity_at` and return [`ResolveOutcome::Reused`].
    /// 3. If one exists but is past the window, set `ended_at = now` on it
    ///    so [`SqliteMemoryStore::touch_session`] can never revive it,
    ///    then fall through to step 4.
    /// 4. `INSERT` a fresh row. The partial unique index
    ///    `sessions_one_active_per_identity_idx` makes this fail when a
    ///    concurrent caller won the race; on conflict the whole tx is
    ///    rolled back and retried (bounded), at which point step 1
    ///    observes the winner and we return [`ResolveOutcome::Reused`].
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotInitialized`] when the store was constructed
    /// via `Default::default()`. Returns [`StoreError::Worker`] /
    /// [`StoreError::Sqlite`] for SQL failures, or [`StoreError::Busy`]
    /// when sustained write contention exceeds the operation deadline
    /// ([`RESOLVE_BUSY_DEADLINE_MS`]). `Busy` is retriable on the caller's
    /// next user action; it is distinct from invariant violations so
    /// dispatchers don't conflate availability with corruption.
    #[instrument(
        skip(self, identity, metadata),
        err,
        fields(
            verb = "resolve_or_create_session",
            user = %identity.user,
            agent = %identity.agent,
            idle_window_secs,
        ),
    )]
    #[allow(
        tail_expr_drop_order,
        reason = "drop order of the cloned tokio_rusqlite handle relative to the await temporary is benign — both are channel-backed clones with no observable side effects beyond worker shutdown, which the runtime handles regardless of order"
    )]
    pub async fn resolve_or_create_session(
        &self,
        identity: &SessionIdentity,
        idle_window_secs: u64,
        metadata: NewSessionMetadata,
    ) -> Result<ResolveOutcome, StoreError> {
        let conn = self.require_conn("resolve_or_create_session")?.clone();
        let user = identity.user.as_str().to_owned();
        let agent = identity.agent.as_str().to_owned();
        let project_root = identity.project_root.clone();
        let identity_clone = identity.clone();
        let metadata_clone = metadata.clone();

        async_retry_busy("resolve_or_create_session", || {
            let conn = conn.clone();
            let user = user.clone();
            let agent = agent.clone();
            let project_root = project_root.clone();
            let identity_clone = identity_clone.clone();
            let metadata_clone = metadata_clone.clone();
            async move {
                conn.call(move |c| {
                    Ok::<_, tokio_rusqlite::Error>(single_attempt_resolve_or_create(
                        c,
                        &user,
                        &agent,
                        project_root.as_deref(),
                        idle_window_secs,
                        &identity_clone,
                        &metadata_clone,
                    ))
                })
                .await
                .map_err(StoreError::from)?
            }
        })
        .await
    }

    /// Bump `last_activity_at` on the named session to "now". Returns
    /// `Ok(false)` if the session id does not exist, has already ended,
    /// or belongs to a different `(user, agent, project_root)` than
    /// `expected`; `Ok(true)` when a row was updated.
    ///
    /// `expected` enforces the cross-identity tampering guard at the
    /// store layer rather than relying on call-site discipline. A leaked
    /// or guessed session id cannot be used to bump activity on a row
    /// belonging to another identity, even if a higher layer's
    /// authorization check is bypassed.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotInitialized`] when the store was constructed
    /// via `Default::default()`. Returns [`StoreError::Worker`] /
    /// [`StoreError::Sqlite`] for SQL failures.
    #[instrument(
        skip(self, expected),
        err,
        fields(
            verb = "touch_session",
            session_id = %id.as_str(),
            user = %expected.user,
            agent = %expected.agent,
        ),
    )]
    pub async fn touch_session(
        &self,
        id: &SessionId,
        expected: &SessionIdentity,
    ) -> Result<bool, StoreError> {
        let conn = self.require_conn("touch_session")?.clone();
        let key = id.as_str().to_owned();
        let user = expected.user.as_str().to_owned();
        let agent = expected.agent.as_str().to_owned();
        let project_root = expected.project_root.clone();
        let n = async_retry_busy("touch_session", || {
            let conn = conn.clone();
            let key = key.clone();
            let user = user.clone();
            let agent = agent.clone();
            let project_root = project_root.clone();
            async move {
                let res: Result<usize, rusqlite::Error> = conn
                    .call(move |c| {
                        let now_ms = current_unix_ms();
                        let r = c.execute(
                            "UPDATE sessions SET last_activity_at = ?1 \
                             WHERE session_id = ?2 \
                               AND user_id = ?3 \
                               AND agent_id = ?4 \
                               AND project_root IS ?5 \
                               AND ended_at IS NULL",
                            params![now_ms, key, user, agent, project_root],
                        );
                        Ok::<_, tokio_rusqlite::Error>(r)
                    })
                    .await
                    .map_err(StoreError::from)?;
                match res {
                    Ok(n) => Ok(AttemptOutcome::Ok(n)),
                    Err(e) if is_busy_error(&e) => Ok(AttemptOutcome::Transient),
                    Err(e) => Err(StoreError::Sqlite(e)),
                }
            }
        })
        .await?;
        Ok(n > 0)
    }

    /// Mark the session `ended_at = now`. Idempotent: ending an
    /// already-ended session is a no-op (`Ok(false)`). Also returns
    /// `Ok(false)` when the row exists but belongs to a different
    /// `(user, agent, project_root)` than `expected` — see
    /// [`SqliteMemoryStore::touch_session`] for why the identity guard
    /// lives at the store layer.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotInitialized`] when the store was constructed
    /// via `Default::default()`. Returns [`StoreError::Worker`] /
    /// [`StoreError::Sqlite`] for SQL failures.
    #[instrument(
        skip(self, expected),
        err,
        fields(
            verb = "end_session",
            session_id = %id.as_str(),
            user = %expected.user,
            agent = %expected.agent,
        ),
    )]
    pub async fn end_session(
        &self,
        id: &SessionId,
        expected: &SessionIdentity,
    ) -> Result<bool, StoreError> {
        let conn = self.require_conn("end_session")?.clone();
        let key = id.as_str().to_owned();
        let user = expected.user.as_str().to_owned();
        let agent = expected.agent.as_str().to_owned();
        let project_root = expected.project_root.clone();
        let n = async_retry_busy("end_session", || {
            let conn = conn.clone();
            let key = key.clone();
            let user = user.clone();
            let agent = agent.clone();
            let project_root = project_root.clone();
            async move {
                let res: Result<usize, rusqlite::Error> = conn
                    .call(move |c| {
                        let now_ms = current_unix_ms();
                        let r = c.execute(
                            "UPDATE sessions SET ended_at = ?1 \
                             WHERE session_id = ?2 \
                               AND user_id = ?3 \
                               AND agent_id = ?4 \
                               AND project_root IS ?5 \
                               AND ended_at IS NULL",
                            params![now_ms, key, user, agent, project_root],
                        );
                        Ok::<_, tokio_rusqlite::Error>(r)
                    })
                    .await
                    .map_err(StoreError::from)?;
                match res {
                    Ok(n) => Ok(AttemptOutcome::Ok(n)),
                    Err(e) if is_busy_error(&e) => Ok(AttemptOutcome::Transient),
                    Err(e) => Err(StoreError::Sqlite(e)),
                }
            }
        })
        .await?;
        Ok(n > 0)
    }

    /// Fetch a single session by id, regardless of `ended_at` state,
    /// after enforcing identity equality.
    ///
    /// Returns `Ok(None)` when the row does not exist OR when it
    /// belongs to a different `(user, agent, project_root)` than
    /// `expected`. Treating a foreign-id read as "not found" matches
    /// the behavior of [`SqliteMemoryStore::touch_session`] /
    /// [`SqliteMemoryStore::end_session`] and prevents metadata
    /// disclosure: a leaked or guessed id can't be used to read
    /// another identity's `tags`, `channel`, `last_activity_at`, or
    /// `ended_at` state.
    ///
    /// For internal store paths that need to read by id without an
    /// identity check (e.g. integration tests asserting on a seeded
    /// row's stored shape), use `get_session_unchecked` (gated behind
    /// the `test-helpers` feature).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotInitialized`] when the store was constructed
    /// via `Default::default()`. Returns [`StoreError::Worker`] /
    /// [`StoreError::Sqlite`] for SQL failures.
    #[instrument(
        skip(self, expected),
        err,
        fields(
            verb = "get_session",
            session_id = %id.as_str(),
            user = %expected.user,
            agent = %expected.agent,
        ),
    )]
    pub async fn get_session(
        &self,
        id: &SessionId,
        expected: &SessionIdentity,
    ) -> Result<Option<Session>, StoreError> {
        let row = self.fetch_session_row(id).await?;
        Ok(row.filter(|s| s.identity == *expected))
    }

    /// Load session-tree metadata rooted at `root`.
    ///
    /// Existing v0.1 sessions have no `session_tree_nodes` row; those are
    /// synthesized as a one-node flat tree so old sessions keep retrieving
    /// normally after the v0.3 schema lands.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotInitialized`] when the store is unconnected,
    /// [`StoreError::Sqlite`] / [`StoreError::Worker`] for storage failures,
    /// and [`StoreError::InvalidSessionTree`] when persisted metadata violates
    /// the pure session-tree invariants.
    #[instrument(
        skip(self),
        err,
        fields(verb = "get_session_tree", session_id = %root.as_str()),
    )]
    #[allow(
        tail_expr_drop_order,
        reason = "drop order of the cloned tokio_rusqlite handle relative to the await temporary is benign — both are channel-backed clones with no observable side effects beyond worker shutdown, which the runtime handles regardless of order"
    )]
    pub async fn get_session_tree(
        &self,
        root: &SessionId,
    ) -> Result<Option<SessionTree>, StoreError> {
        let conn = self.require_conn("get_session_tree")?.clone();
        let root_key = root.as_str().to_owned();
        conn.call(move |c| Ok::<_, tokio_rusqlite::Error>(load_session_tree_sync(c, &root_key)))
            .await
            .map_err(StoreError::from)?
    }

    /// Persist copy-on-write branch metadata between two existing sessions.
    pub async fn record_session_fork(
        &self,
        from: &SessionId,
        child: &SessionId,
        at_turn_id: impl Into<String>,
    ) -> Result<(), StoreError> {
        self.record_session_branch(from, child, BranchKind::Fork, at_turn_id.into(), None)
            .await
    }

    /// Persist full-copy branch metadata between two existing sessions.
    pub async fn record_session_clone(
        &self,
        from: &SessionId,
        child: &SessionId,
    ) -> Result<(), StoreError> {
        self.record_session_branch(from, child, BranchKind::Clone, "latest".to_owned(), None)
            .await
    }

    /// Persist tool-spawned branch metadata between two existing sessions.
    pub async fn record_session_tool_spawn(
        &self,
        from: &SessionId,
        child: &SessionId,
        at_turn_id: impl Into<String>,
        tool_call_id: impl Into<String>,
    ) -> Result<(), StoreError> {
        self.record_session_branch(
            from,
            child,
            BranchKind::ToolSpawned,
            at_turn_id.into(),
            Some(tool_call_id.into()),
        )
        .await
    }

    async fn record_session_branch(
        &self,
        from: &SessionId,
        child: &SessionId,
        kind: BranchKind,
        at_turn_id: String,
        tool_call_id: Option<String>,
    ) -> Result<(), StoreError> {
        let mut check = SessionTree::flat(from.clone());
        match kind {
            BranchKind::Fork => check.fork(from, child.clone(), at_turn_id.clone())?,
            BranchKind::Clone => check.clone_session(from, child.clone())?,
            BranchKind::ToolSpawned => check.tool_spawn(
                from,
                child.clone(),
                at_turn_id.clone(),
                tool_call_id.clone().ok_or(SessionTreeError::EmptyField {
                    field: "tool_call_id",
                })?,
            )?,
            _ => {
                return Err(StoreError::Invariant {
                    what: "unknown future BranchKind cannot be persisted by this store".to_owned(),
                });
            }
        }

        let conn = self.require_conn("record_session_branch")?.clone();
        let from_key = from.as_str().to_owned();
        let child_key = child.as_str().to_owned();
        let kind_wire = branch_kind_wire(kind).to_owned();
        conn.call(move |c| {
            let now_ms = current_unix_ms();
            c.execute(
                "INSERT OR IGNORE INTO session_tree_nodes \
                   (session_id, parent_session_id, at_turn_id, branch_kind, tool_call_id, created_at) \
                 VALUES (?1, NULL, NULL, NULL, NULL, ?2)",
                params![from_key, now_ms],
            )?;
            c.execute(
                "INSERT INTO session_tree_nodes \
                   (session_id, parent_session_id, at_turn_id, branch_kind, tool_call_id, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![child_key, from_key, at_turn_id, kind_wire, tool_call_id, now_ms],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await?;
        Ok(())
    }

    /// Persist an explicit, auditable session-tree merge between sessions.
    pub async fn record_session_merge(
        &self,
        source: &SessionId,
        destination: &SessionId,
        strategy: MergeStrategy,
        applied_at_turn_id: impl Into<String>,
    ) -> Result<SessionMerge, StoreError> {
        let applied_at_turn_id = applied_at_turn_id.into();
        if source == destination {
            return Err(SessionTreeError::SelfMerge {
                session_id: source.clone(),
            }
            .into());
        }
        let tree = self.get_session_tree(source).await?.ok_or_else(|| {
            SessionTreeError::UnknownSession {
                session_id: source.clone(),
            }
        })?;
        tree.lineage(destination)?;
        let mut check = tree.clone();
        let merge = check.record_merge(
            source.clone(),
            destination.clone(),
            strategy.clone(),
            applied_at_turn_id.clone(),
        )?;

        let conn = self.require_conn("record_session_merge")?.clone();
        let source_key = source.as_str().to_owned();
        let destination_key = destination.as_str().to_owned();
        let encoded = EncodedMergeStrategy::from_strategy(&strategy);
        conn.call(move |c| {
            c.execute(
                "INSERT INTO session_tree_merges \
                   (source_session_id, destination_session_id, strategy_kind, \
                    summary_record_id, first_turn_id, last_turn_id, applied_at_turn_id, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    source_key,
                    destination_key,
                    encoded.kind,
                    encoded.summary_record_id,
                    encoded.first_turn_id,
                    encoded.last_turn_id,
                    applied_at_turn_id,
                    current_unix_ms(),
                ],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await?;
        Ok(merge)
    }

    /// Test-only accessor that bypasses the identity guard
    /// [`SqliteMemoryStore::get_session`] enforces.
    ///
    /// Gated behind the `test-helpers` feature so production builds
    /// cannot reach it. Integration tests in this crate enable the
    /// feature in `dev-dependencies`; external consumers cannot turn
    /// it on without explicitly opting in. Use only for migration
    /// tests that seed rows by id and need to assert on the stored
    /// shape without reconstructing a [`SessionIdentity`].
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotInitialized`] when the store was constructed
    /// via `Default::default()`. Returns [`StoreError::Worker`] /
    /// [`StoreError::Sqlite`] for SQL failures.
    #[cfg(feature = "test-helpers")]
    pub async fn get_session_unchecked(
        &self,
        id: &SessionId,
    ) -> Result<Option<Session>, StoreError> {
        self.fetch_session_row(id).await
    }

    /// Internal session-row reader used by [`SqliteMemoryStore::get_session`]
    /// (after identity filtering) and the `test-helpers`-gated
    /// `get_session_unchecked`. Crate-private so the raw lookup never
    /// crosses the public API surface.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotInitialized`] when the store was constructed
    /// via `Default::default()`. Returns [`StoreError::Worker`] /
    /// [`StoreError::Sqlite`] for SQL failures.
    #[instrument(
        skip(self),
        err,
        fields(verb = "fetch_session_row", session_id = %id.as_str()),
    )]
    pub(crate) async fn fetch_session_row(
        &self,
        id: &SessionId,
    ) -> Result<Option<Session>, StoreError> {
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
        let identity = SessionIdentity::from_persisted(user, agent, project_root).map_err(|e| {
            StoreError::Invariant {
                what: format!("session identity round-trip failed: {e}"),
            }
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

/// One outcome of a single transaction attempt.
///
/// `Transient` means the attempt observed a retriable conflict
/// (`SQLITE_BUSY` / `SQLITE_BUSY_SNAPSHOT`, partial-unique-index
/// violation, or compare-and-swap stale-snapshot). The caller retries
/// after a backoff. `Ok(T)` is the committed result. Anything else
/// surfaces as a typed `StoreError` from the outer
/// [`async_retry_busy`] loop without retry.
enum AttemptOutcome<T> {
    Ok(T),
    Transient,
}

/// `true` when `e` is a `SQLITE_BUSY` or `SQLITE_BUSY_SNAPSHOT`
/// failure — the two error codes that indicate write-lock contention
/// past `busy_timeout`. Anything else is treated as terminal.
fn is_busy_error(e: &rusqlite::Error) -> bool {
    if let rusqlite::Error::SqliteFailure(err, _) = e {
        let code = err.code as i32;
        code == rusqlite::ffi::SQLITE_BUSY
            || err.extended_code == rusqlite::ffi::SQLITE_BUSY_SNAPSHOT
    } else {
        false
    }
}

/// Async retry loop for store operations whose single-attempt closure
/// returns [`AttemptOutcome`]. Each call to `f` runs one attempt
/// (typically a single `conn.call(...)` round-trip to the
/// `tokio_rusqlite` worker thread); transient failures cause the
/// caller to release the worker and `tokio::time::sleep` here. This
/// keeps backoff sleeps off the dedicated DB thread so unrelated
/// queries queued behind a contended session call are not stalled
/// for up to [`RESOLVE_BUSY_DEADLINE_MS`].
///
/// Backoff is truncated-exponential, jittered with an LCG over the
/// elapsed nanoseconds so we don't pull a `rand` dep, capped by the
/// per-attempt remaining-deadline so the final sleep never overruns.
/// Spread retries across the current backoff window so contending
/// callers desynchronize. Returns `0..backoff_ms` (or 0 when
/// `backoff_ms` is 0). Mixed with a fast hash so the same `elapsed_ns`
/// produces different jitter at different scales.
fn jitter_ms(elapsed_ns: u64, backoff_ms: u64) -> u64 {
    let mixed = elapsed_ns
        .wrapping_mul(6_364_136_223_846_793_005)
        .rotate_left(13);
    mixed % backoff_ms.max(1)
}

async fn async_retry_busy<T, F, Fut>(operation: &'static str, mut f: F) -> Result<T, StoreError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<AttemptOutcome<T>, StoreError>>,
{
    let start = std::time::Instant::now();
    let deadline = start + RESOLVE_BUSY_DEADLINE;
    let mut backoff_ms: u64 = INITIAL_BACKOFF_MS;
    loop {
        match f().await? {
            AttemptOutcome::Ok(v) => return Ok(v),
            AttemptOutcome::Transient => {
                let now = std::time::Instant::now();
                if now >= deadline {
                    let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
                    return Err(StoreError::Busy {
                        operation,
                        elapsed_ms,
                    });
                }
                let elapsed_ns = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
                let raw_sleep_ms = backoff_ms.saturating_add(jitter_ms(elapsed_ns, backoff_ms));
                let remaining_ms = u64::try_from((deadline - now).as_millis()).unwrap_or(u64::MAX);
                let sleep_ms = raw_sleep_ms.min(remaining_ms.max(1));
                tokio::time::sleep(std::time::Duration::from_millis(sleep_ms)).await;
                backoff_ms = backoff_ms.saturating_mul(2).min(MAX_BACKOFF_MS);
            }
        }
    }
}

/// Single transaction attempt for
/// [`SqliteMemoryStore::resolve_or_create_session`]. Runs on the
/// `tokio_rusqlite` worker thread; the outer async loop owns the
/// retry/backoff schedule so this closure never sleeps and never
/// holds the worker past one tx round-trip.
#[allow(
    clippy::too_many_arguments,
    reason = "in-tx attempt threads identity + metadata + lookup keys; collapsing into a struct adds indirection without benefit"
)]
fn single_attempt_resolve_or_create(
    c: &mut rusqlite::Connection,
    user: &str,
    agent: &str,
    project_root: Option<&str>,
    idle_window_secs: u64,
    identity: &SessionIdentity,
    metadata: &NewSessionMetadata,
) -> Result<AttemptOutcome<ResolveOutcome>, StoreError> {
    let tx = match c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate) {
        Ok(tx) => tx,
        Err(e) if is_busy_error(&e) => return Ok(AttemptOutcome::Transient),
        Err(e) => return Err(StoreError::Sqlite(e)),
    };
    let res = resolve_or_create_in_tx(
        &tx,
        user,
        agent,
        project_root,
        idle_window_secs,
        identity,
        metadata,
    );
    match res {
        Ok(outcome) => match tx.commit() {
            Ok(()) => Ok(AttemptOutcome::Ok(outcome)),
            Err(e) if is_busy_error(&e) => Ok(AttemptOutcome::Transient),
            Err(e) => Err(StoreError::Sqlite(e)),
        },
        Err(InTxError::UniqueViolation | InTxError::StaleSnapshot) => {
            drop(tx);
            Ok(AttemptOutcome::Transient)
        }
        Err(InTxError::Sqlite(e)) => Err(StoreError::Sqlite(e)),
        Err(InTxError::Codec(e)) => Err(StoreError::Codec(e)),
        Err(InTxError::Invariant(s)) => Err(StoreError::Invariant { what: s }),
        Err(InTxError::Terminal(e)) => Err(e),
    }
}

/// Single transaction attempt for
/// [`SqliteMemoryStore::resolve_explicit_session`]. Same shape as
/// [`single_attempt_resolve_or_create`] — runs on the DB worker once
/// and lets the outer async loop decide retry vs. surface.
fn single_attempt_resolve_explicit(
    c: &mut rusqlite::Connection,
    id_str: &str,
    expected: &SessionIdentity,
) -> Result<AttemptOutcome<Session>, StoreError> {
    let tx = match c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate) {
        Ok(tx) => tx,
        Err(e) if is_busy_error(&e) => return Ok(AttemptOutcome::Transient),
        Err(e) => return Err(StoreError::Sqlite(e)),
    };
    match resolve_explicit_in_tx(&tx, id_str, expected) {
        Ok(session) => match tx.commit() {
            Ok(()) => Ok(AttemptOutcome::Ok(session)),
            Err(e) if is_busy_error(&e) => Ok(AttemptOutcome::Transient),
            Err(e) => Err(StoreError::Sqlite(e)),
        },
        Err(InTxError::StaleSnapshot) => {
            drop(tx);
            Ok(AttemptOutcome::Transient)
        }
        Err(InTxError::UniqueViolation) => {
            drop(tx);
            Err(StoreError::Invariant {
                what: "resolve_explicit_session: unexpected unique-violation \
                       (read-only path)"
                    .into(),
            })
        }
        Err(InTxError::Sqlite(e)) => Err(StoreError::Sqlite(e)),
        Err(InTxError::Codec(e)) => Err(StoreError::Codec(e)),
        Err(InTxError::Invariant(s)) => Err(StoreError::Invariant { what: s }),
        Err(InTxError::Terminal(e)) => Err(e),
    }
}

/// In-tx body for [`SqliteMemoryStore::resolve_explicit_session`].
///
/// SELECTs the row for `id_str`, validates `(user, agent, project_root)`
/// matches `expected`, then bumps `last_activity_at` with a CAS guard on
/// the snapshotted value. A concurrent `end_session` between SELECT and
/// UPDATE makes the CAS match zero rows; the caller restarts the tx.
///
/// Maps row state to typed errors:
/// - missing row → [`StoreError::SessionNotFound`]
/// - `ended_at IS NOT NULL` → [`StoreError::SessionEnded`]
/// - identity mismatch → [`StoreError::SessionIdentityMismatch`]
fn resolve_explicit_in_tx(
    tx: &rusqlite::Transaction<'_>,
    id_str: &str,
    expected: &SessionIdentity,
) -> Result<Session, InTxError> {
    let row: Option<SessionRow> = tx
        .query_row(
            "SELECT session_id, user_id, agent_id, project_root, \
                    title, channel, priority, tags, \
                    created_at, last_activity_at, ended_at \
             FROM sessions WHERE session_id = ?1",
            params![id_str],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                ))
            },
        )
        .optional()?;

    let Some(row) = row else {
        return Err(InTxError::Terminal(StoreError::SessionNotFound {
            session_id: id_str.to_owned(),
        }));
    };
    let session = session_from_row(row)?;

    if session.identity != *expected {
        return Err(InTxError::Terminal(StoreError::SessionIdentityMismatch {
            session_id: id_str.to_owned(),
        }));
    }
    if let Some(ended) = session.ended_at_unix_ms {
        return Err(InTxError::Terminal(StoreError::SessionEnded {
            session_id: id_str.to_owned(),
            ended_at_unix_ms: ended,
        }));
    }

    // CAS bump: if a concurrent end_session between SELECT and UPDATE has
    // closed the row, last_activity_at no longer matches the snapshot AND
    // ended_at is no longer NULL. Either makes this UPDATE affect zero
    // rows; we surface as StaleSnapshot so the outer loop restarts the tx
    // and observes the closed row on the next pass.
    let now_ms = current_unix_ms();
    let updated = tx.execute(
        "UPDATE sessions SET last_activity_at = ?1 \
         WHERE session_id = ?2 \
           AND ended_at IS NULL \
           AND last_activity_at = ?3",
        params![now_ms, id_str, session.last_activity_at_unix_ms],
    )?;
    if updated == 0 {
        return Err(InTxError::StaleSnapshot);
    }

    Ok(Session {
        last_activity_at_unix_ms: now_ms,
        ..session
    })
}

/// Decode a `SessionRow` tuple to the typed [`Session`] domain struct,
/// surfacing structural failures as [`InTxError::Invariant`].
pub(crate) fn session_from_row(row: SessionRow) -> Result<Session, InTxError> {
    let (
        sid,
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
    ) = row;
    let id = SessionId::parse(&sid)
        .map_err(|e| InTxError::Invariant(format!("session_id round-trip failed: {e}")))?;
    let user = cairn_core::domain::Identity::parse(&user)
        .map_err(|e| InTxError::Invariant(format!("session.user_id round-trip failed: {e}")))?;
    let agent = cairn_core::domain::Identity::parse(&agent)
        .map_err(|e| InTxError::Invariant(format!("session.agent_id round-trip failed: {e}")))?;
    let identity = SessionIdentity::from_persisted(user, agent, project_root)
        .map_err(|e| InTxError::Invariant(format!("session identity round-trip failed: {e}")))?;
    let tags: Vec<String> = tags_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()?
        .unwrap_or_default();
    Ok(Session {
        id,
        identity,
        title,
        channel,
        priority,
        tags,
        created_at_unix_ms: created_at,
        last_activity_at_unix_ms: last_activity,
        ended_at_unix_ms: ended,
    })
}

/// Internal error type for the in-tx body of
/// [`SqliteMemoryStore::resolve_or_create_session`]. Distinguishes the
/// retryable conflicts from terminal failures so the outer loop can choose
/// to spin or surface the error.
#[derive(Debug)]
pub(crate) enum InTxError {
    /// Partial unique index `sessions_one_active_per_identity_idx` rejected
    /// the INSERT — a concurrent caller won the race. Caller should
    /// rollback and retry.
    UniqueViolation,
    /// The snapshot used to judge a row stale was invalidated by a
    /// concurrent `touch_session` between our SELECT and the conditional
    /// UPDATE (the compare-and-swap update affected zero rows). Caller
    /// should rollback and retry; the next iteration's SELECT will see
    /// the bumped `last_activity_at` and decide reuse.
    StaleSnapshot,
    /// Other `SQLite` error.
    Sqlite(rusqlite::Error),
    /// Tag JSON serialization failed.
    Codec(serde_json::Error),
    /// Stored row violated a structural invariant (corrupt id, bad identity).
    Invariant(String),
    /// Terminal store-level error (not retriable, surfaced verbatim).
    /// Used by paths that need to return typed errors like
    /// [`StoreError::SessionNotFound`] / [`StoreError::SessionEnded`] /
    /// [`StoreError::SessionIdentityMismatch`] from inside the in-tx body.
    Terminal(StoreError),
}

impl From<rusqlite::Error> for InTxError {
    fn from(e: rusqlite::Error) -> Self {
        if let rusqlite::Error::SqliteFailure(err, _) = &e {
            // SQLITE_CONSTRAINT_UNIQUE = 2067 — partial unique index conflict.
            if err.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE {
                return Self::UniqueViolation;
            }
            // SQLITE_BUSY (5) and its WAL variant SQLITE_BUSY_SNAPSHOT
            // (517) surface when a reader-turned-writer loses the
            // upgrade race or the busy_timeout window is exhausted by
            // sustained cross-process contention. Treat as transient
            // and retry — the same pattern an external caller would
            // implement around any SQLite write.
            let code = err.code as i32;
            if code == rusqlite::ffi::SQLITE_BUSY
                || err.extended_code == rusqlite::ffi::SQLITE_BUSY_SNAPSHOT
            {
                return Self::StaleSnapshot;
            }
        }
        Self::Sqlite(e)
    }
}

type SessionTreeNodeRow = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

type SessionTreeMergeRow = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
);

struct EncodedMergeStrategy {
    kind: &'static str,
    summary_record_id: Option<String>,
    first_turn_id: Option<String>,
    last_turn_id: Option<String>,
}

impl EncodedMergeStrategy {
    fn from_strategy(strategy: &MergeStrategy) -> Self {
        match strategy {
            MergeStrategy::ReasoningSummary { summary_record_id } => Self {
                kind: "reasoning_summary",
                summary_record_id: Some(summary_record_id.as_str().to_owned()),
                first_turn_id: None,
                last_turn_id: None,
            },
            MergeStrategy::ControlledSplice {
                first_turn_id,
                last_turn_id,
            } => Self {
                kind: "controlled_splice",
                summary_record_id: None,
                first_turn_id: Some(first_turn_id.clone()),
                last_turn_id: Some(last_turn_id.clone()),
            },
            _ => Self {
                kind: "unknown",
                summary_record_id: None,
                first_turn_id: None,
                last_turn_id: None,
            },
        }
    }
}

fn load_session_tree_sync(
    conn: &rusqlite::Connection,
    root_key: &str,
) -> Result<Option<SessionTree>, StoreError> {
    let root_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) = 1 FROM sessions WHERE session_id = ?1",
            params![root_key],
            |r| r.get::<_, i64>(0).map(|v| v == 1),
        )
        .map_err(StoreError::Sqlite)?;
    if !root_exists {
        return Ok(None);
    }

    let root_key = resolve_session_tree_root_key(conn, root_key)?;
    let root = parse_session_id(&root_key)?;
    let mut tree = SessionTree::flat(root.clone());
    let mut subtree_ids = BTreeSet::from([root.clone()]);
    let rows = load_session_tree_node_rows(conn, &root_key)?;

    for (session_id, parent_session_id, at_turn_id, branch_kind, tool_call_id) in rows {
        let session_id = parse_session_id(&session_id)?;
        subtree_ids.insert(session_id.clone());
        if session_id == root {
            continue;
        }
        let parent = parent_session_id.ok_or_else(|| {
            StoreError::InvalidSessionTree(SessionTreeError::MalformedLink {
                session_id: session_id.clone(),
                message: "non-root node must have a parent",
            })
        })?;
        let parent = parse_session_id(&parent)?;
        let at_turn_id = at_turn_id.ok_or(SessionTreeError::EmptyField {
            field: "at_turn_id",
        })?;
        match parse_branch_kind(branch_kind.as_deref(), &session_id)? {
            BranchKind::Fork => tree.fork(&parent, session_id, at_turn_id)?,
            BranchKind::Clone => tree.clone_session(&parent, session_id)?,
            BranchKind::ToolSpawned => tree.tool_spawn(
                &parent,
                session_id,
                at_turn_id,
                tool_call_id.ok_or(SessionTreeError::EmptyField {
                    field: "tool_call_id",
                })?,
            )?,
            _ => unreachable!("BranchKind is non_exhaustive for forward compatibility"),
        }
    }

    for row in load_session_tree_merge_rows(conn)? {
        let (source, destination, strategy_kind, summary, first, last, applied_at) = row;
        let source = parse_session_id(&source)?;
        let destination = parse_session_id(&destination)?;
        if !(subtree_ids.contains(&source) && subtree_ids.contains(&destination)) {
            continue;
        }
        tree.record_merge(
            source,
            destination,
            parse_merge_strategy(&strategy_kind, summary, first, last)?,
            applied_at,
        )?;
    }
    tree.validate()?;
    Ok(Some(tree))
}

fn resolve_session_tree_root_key(
    conn: &rusqlite::Connection,
    session_key: &str,
) -> Result<String, StoreError> {
    let mut stmt = conn.prepare(
        "WITH RECURSIVE ancestors(session_id, parent_session_id) AS ( \
           SELECT session_id, parent_session_id \
             FROM session_tree_nodes \
            WHERE session_id = ?1 \
           UNION \
           SELECT n.session_id, n.parent_session_id \
             FROM session_tree_nodes n \
             JOIN ancestors ON ancestors.parent_session_id = n.session_id \
         ) \
         SELECT session_id \
           FROM ancestors \
          WHERE parent_session_id IS NULL \
          LIMIT 1",
    )?;
    let resolved = stmt
        .query_row(params![session_key], |r| r.get::<_, String>(0))
        .optional()
        .map_err(StoreError::Sqlite)?;
    if let Some(root_key) = resolved {
        return Ok(root_key);
    }

    let has_metadata: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM session_tree_nodes WHERE session_id = ?1",
            params![session_key],
            |r| r.get::<_, i64>(0).map(|v| v > 0),
        )
        .map_err(StoreError::Sqlite)?;
    if !has_metadata {
        return Ok(session_key.to_owned());
    }

    let session_id = parse_session_id(session_key)?;
    Err(StoreError::InvalidSessionTree(
        SessionTreeError::MalformedLink {
            session_id,
            message: "session tree ancestry does not reach a root",
        },
    ))
}

fn load_session_tree_node_rows(
    conn: &rusqlite::Connection,
    root_key: &str,
) -> Result<Vec<SessionTreeNodeRow>, StoreError> {
    let mut stmt = conn.prepare(
        "WITH RECURSIVE subtree(session_id, depth) AS ( \
           SELECT ?1, 0 \
           UNION ALL \
           SELECT n.session_id, subtree.depth + 1 \
             FROM session_tree_nodes n \
             JOIN subtree ON n.parent_session_id = subtree.session_id \
         ) \
         SELECT n.session_id, n.parent_session_id, n.at_turn_id, n.branch_kind, n.tool_call_id \
           FROM session_tree_nodes n \
           JOIN subtree ON subtree.session_id = n.session_id \
          ORDER BY subtree.depth, n.created_at, n.session_id",
    )?;
    let rows = stmt.query_map(params![root_key], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::Sqlite)
}

fn load_session_tree_merge_rows(
    conn: &rusqlite::Connection,
) -> Result<Vec<SessionTreeMergeRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT source_session_id, destination_session_id, strategy_kind, \
                summary_record_id, first_turn_id, last_turn_id, applied_at_turn_id \
           FROM session_tree_merges \
          ORDER BY created_at, merge_id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
            r.get(6)?,
        ))
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::Sqlite)
}

fn parse_session_id(raw: &str) -> Result<SessionId, StoreError> {
    SessionId::parse(raw).map_err(|e| StoreError::Invariant {
        what: format!("session_tree session_id round-trip failed: {e}"),
    })
}

fn parse_record_id(raw: &str) -> Result<RecordId, StoreError> {
    RecordId::parse(raw).map_err(|e| StoreError::Invariant {
        what: format!("session_tree record_id round-trip failed: {e}"),
    })
}

fn parse_branch_kind(raw: Option<&str>, session_id: &SessionId) -> Result<BranchKind, StoreError> {
    match raw {
        Some("fork") => Ok(BranchKind::Fork),
        Some("clone") => Ok(BranchKind::Clone),
        Some("tool_spawned") => Ok(BranchKind::ToolSpawned),
        Some(_) => Err(StoreError::InvalidSessionTree(
            SessionTreeError::MalformedLink {
                session_id: session_id.clone(),
                message: "unknown branch kind",
            },
        )),
        None => Err(StoreError::InvalidSessionTree(
            SessionTreeError::MalformedLink {
                session_id: session_id.clone(),
                message: "non-root node must have a branch kind",
            },
        )),
    }
}

fn branch_kind_wire(kind: BranchKind) -> &'static str {
    match kind {
        BranchKind::Fork => "fork",
        BranchKind::Clone => "clone",
        BranchKind::ToolSpawned => "tool_spawned",
        _ => unreachable!("BranchKind is non_exhaustive for forward compatibility"),
    }
}

fn parse_merge_strategy(
    kind: &str,
    summary_record_id: Option<String>,
    first_turn_id: Option<String>,
    last_turn_id: Option<String>,
) -> Result<MergeStrategy, StoreError> {
    match kind {
        "reasoning_summary" => Ok(MergeStrategy::ReasoningSummary {
            summary_record_id: parse_record_id(&summary_record_id.ok_or(
                SessionTreeError::EmptyField {
                    field: "summary_record_id",
                },
            )?)?,
        }),
        "controlled_splice" => Ok(MergeStrategy::ControlledSplice {
            first_turn_id: first_turn_id.ok_or(SessionTreeError::EmptyField {
                field: "first_turn_id",
            })?,
            last_turn_id: last_turn_id.ok_or(SessionTreeError::EmptyField {
                field: "last_turn_id",
            })?,
        }),
        _ => Err(StoreError::Invariant {
            what: "session_tree merge strategy kind is unknown".to_owned(),
        }),
    }
}

impl From<serde_json::Error> for InTxError {
    fn from(e: serde_json::Error) -> Self {
        Self::Codec(e)
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "in-tx helper threads identity + metadata + lookup keys; collapsing into a struct adds indirection without benefit"
)]
fn resolve_or_create_in_tx(
    tx: &rusqlite::Transaction<'_>,
    user: &str,
    agent: &str,
    project_root: Option<&str>,
    idle_window_secs: u64,
    identity: &SessionIdentity,
    metadata: &NewSessionMetadata,
) -> Result<ResolveOutcome, InTxError> {
    let now_ms = current_unix_ms();

    // Step 1: locate the most recent active row for this identity.
    let existing: Option<(String, i64)> = tx
        .query_row(
            "SELECT session_id, last_activity_at FROM sessions \
             WHERE user_id = ?1 AND agent_id = ?2 \
               AND project_root IS ?3 \
               AND ended_at IS NULL \
             ORDER BY last_activity_at DESC \
             LIMIT 1",
            params![user, agent, project_root],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?;

    if let Some((existing_id, last_activity_ms)) = existing {
        let idle_ms = (now_ms - last_activity_ms).max(0);
        let idle_secs = u64::try_from(idle_ms / 1000).unwrap_or(u64::MAX);
        if idle_secs <= idle_window_secs {
            // Step 2: reuse — bump last_activity_at with the same CAS guard
            // the stale-close branch uses below. If `end_session` raced in
            // between our SELECT and this UPDATE, the row's `ended_at` is no
            // longer NULL or `last_activity_at` no longer matches the
            // snapshot; in either case zero rows are affected and we
            // restart the whole transaction so we don't return a session id
            // whose row has just been closed.
            let updated = tx.execute(
                "UPDATE sessions SET last_activity_at = ?1 \
                 WHERE session_id = ?2 \
                   AND ended_at IS NULL \
                   AND last_activity_at = ?3",
                params![now_ms, existing_id, last_activity_ms],
            )?;
            if updated == 0 {
                return Err(InTxError::StaleSnapshot);
            }
            let session = read_session_by_id(tx, &existing_id)?.ok_or_else(|| {
                InTxError::Invariant(
                    "resolve_or_create: row vanished between SELECT and UPDATE".into(),
                )
            })?;
            return Ok(ResolveOutcome::Reused(session));
        }
        // Step 3: stale — close it so touch_session can no longer revive
        // this id, then fall through to the INSERT. The compare-and-swap on
        // `last_activity_at` revalidates the staleness snapshot — if a
        // concurrent `touch_session` bumped the row between our SELECT and
        // this UPDATE, zero rows are affected and we restart the whole
        // transaction. The next iteration's SELECT sees the fresh activity
        // and decides reuse instead of erroneously ending a live session.
        let updated = tx.execute(
            "UPDATE sessions SET ended_at = ?1 \
             WHERE session_id = ?2 \
               AND ended_at IS NULL \
               AND last_activity_at = ?3",
            params![now_ms, existing_id, last_activity_ms],
        )?;
        if updated == 0 {
            return Err(InTxError::StaleSnapshot);
        }
    }

    // Step 4: INSERT a fresh row. Partial unique index may reject if a
    // concurrent caller raced ahead — surfaced as `UniqueViolation` so the
    // outer loop retries.
    let id_str = Ulid::new().to_string();
    let session_id = SessionId::parse(&id_str)
        .map_err(|e| InTxError::Invariant(format!("freshly-minted ULID rejected: {e}")))?;
    let tags_json = if metadata.tags.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&metadata.tags)?)
    };
    tx.execute(
        "INSERT INTO sessions \
           (session_id, user_id, agent_id, project_root, title, \
            channel, priority, tags, metadata_json, \
            created_at, last_activity_at, ended_at) \
         VALUES (?1, ?2, ?3, ?4, '', ?5, ?6, ?7, NULL, ?8, ?8, NULL)",
        params![
            id_str,
            user,
            agent,
            project_root,
            metadata.channel,
            metadata.priority,
            tags_json,
            now_ms,
        ],
    )?;

    Ok(ResolveOutcome::Created(Session {
        id: session_id,
        identity: identity.clone(),
        title: String::new(),
        channel: metadata.channel.clone(),
        priority: metadata.priority.clone(),
        tags: metadata.tags.clone(),
        created_at_unix_ms: now_ms,
        last_activity_at_unix_ms: now_ms,
        ended_at_unix_ms: None,
    }))
}

/// Row shape for `SELECT * FROM sessions WHERE session_id = ?` — broken
/// out so [`read_session_by_id`] doesn't trip clippy's `type_complexity`.
pub(crate) type SessionRow = (
    String,         // session_id
    String,         // user_id
    String,         // agent_id
    Option<String>, // project_root
    String,         // title
    Option<String>, // channel
    Option<String>, // priority
    Option<String>, // tags JSON
    i64,            // created_at
    i64,            // last_activity_at
    Option<i64>,    // ended_at
);

pub(crate) fn read_session_by_id(
    tx: &rusqlite::Transaction<'_>,
    id_str: &str,
) -> Result<Option<Session>, InTxError> {
    let row: Option<SessionRow> = tx
        .query_row(
            "SELECT session_id, user_id, agent_id, project_root, \
                    title, channel, priority, tags, \
                    created_at, last_activity_at, ended_at \
             FROM sessions WHERE session_id = ?1",
            params![id_str],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                ))
            },
        )
        .optional()?;
    let Some((
        sid,
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
    let id = SessionId::parse(&sid)
        .map_err(|e| InTxError::Invariant(format!("session_id round-trip failed: {e}")))?;
    let user = cairn_core::domain::Identity::parse(&user)
        .map_err(|e| InTxError::Invariant(format!("session.user_id round-trip failed: {e}")))?;
    let agent = cairn_core::domain::Identity::parse(&agent)
        .map_err(|e| InTxError::Invariant(format!("session.agent_id round-trip failed: {e}")))?;
    let identity = SessionIdentity::from_persisted(user, agent, project_root)
        .map_err(|e| InTxError::Invariant(format!("session identity round-trip failed: {e}")))?;
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

#[cfg(test)]
mod tests {
    use super::jitter_ms;

    #[test]
    fn jitter_is_zero_when_backoff_is_zero() {
        assert_eq!(jitter_ms(12_345, 0), 0);
    }

    #[test]
    fn jitter_stays_below_backoff() {
        for elapsed in [1_u64, 999, 1_000_000, 1_234_567_890, u64::MAX / 2] {
            for backoff in [1_u64, 2, 4, 8, 16, 32, 64, 128, 256] {
                let j = jitter_ms(elapsed, backoff);
                assert!(j < backoff, "jitter {j} not < backoff {backoff}");
            }
        }
    }

    #[test]
    fn jitter_desynchronizes_for_distinct_elapsed_values() {
        // Two contending callers with different elapsed-ns counters
        // must NOT pick the same sleep at every backoff stage. Sample
        // a handful of stages and require at least some divergence —
        // the previous `& 0x3FF / 1024` calc returned 0 for everyone,
        // so any non-zero result here is a regression guard.
        let backoffs = [2_u64, 4, 8, 16, 32];
        let mut diffs = 0_u32;
        for (i, backoff) in backoffs.iter().enumerate() {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "test inputs are small u32-shaped values"
            )]
            let a = jitter_ms(1_000_000 + (i as u64) * 7919, *backoff);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "test inputs are small u32-shaped values"
            )]
            let b = jitter_ms(2_345_678 + (i as u64) * 1009, *backoff);
            if a != b {
                diffs += 1;
            }
        }
        assert!(
            diffs >= 3,
            "jitter must desynchronize most stages, got {diffs}/5",
        );
    }
}
