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
    pub async fn resolve_explicit_session(
        &self,
        id: &SessionId,
        expected: &SessionIdentity,
    ) -> Result<Session, StoreError> {
        let conn = self.require_conn("resolve_explicit_session")?.clone();
        let id_str = id.as_str().to_owned();
        let expected_clone = expected.clone();

        let outcome = conn
            .call(move |c| {
                let start = std::time::Instant::now();
                let deadline = start + RESOLVE_BUSY_DEADLINE;
                let mut backoff_ms = INITIAL_BACKOFF_MS;
                loop {
                    let tx_result =
                        c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate);
                    let tx = match tx_result {
                        Ok(tx) => tx,
                        Err(e) => {
                            if let rusqlite::Error::SqliteFailure(err, _) = &e {
                                let code = err.code as i32;
                                if code != rusqlite::ffi::SQLITE_BUSY
                                    && err.extended_code != rusqlite::ffi::SQLITE_BUSY_SNAPSHOT
                                {
                                    return Err(tokio_rusqlite::Error::Other(Box::new(e)));
                                }
                                // Transient — backoff + retry below.
                            } else {
                                return Err(tokio_rusqlite::Error::Other(Box::new(e)));
                            }
                            sleep_with_backoff_or_break(start, deadline, &mut backoff_ms);
                            if std::time::Instant::now() >= deadline {
                                return Ok::<_, tokio_rusqlite::Error>(Err(StoreError::Busy {
                                    operation: "resolve_explicit_session",
                                    elapsed_ms: u64::try_from(start.elapsed().as_millis())
                                        .unwrap_or(u64::MAX),
                                }));
                            }
                            continue;
                        }
                    };

                    match resolve_explicit_in_tx(&tx, &id_str, &expected_clone) {
                        Ok(session) => {
                            tx.commit()?;
                            return Ok::<_, tokio_rusqlite::Error>(Ok(session));
                        }
                        Err(InTxError::StaleSnapshot) => {
                            drop(tx);
                            sleep_with_backoff_or_break(start, deadline, &mut backoff_ms);
                            if std::time::Instant::now() >= deadline {
                                return Ok::<_, tokio_rusqlite::Error>(Err(StoreError::Busy {
                                    operation: "resolve_explicit_session",
                                    elapsed_ms: u64::try_from(start.elapsed().as_millis())
                                        .unwrap_or(u64::MAX),
                                }));
                            }
                        }
                        Err(InTxError::UniqueViolation) => {
                            drop(tx);
                            return Ok::<_, tokio_rusqlite::Error>(Err(StoreError::Invariant {
                                what: "resolve_explicit_session: unexpected unique-violation \
                                       (read-only path)"
                                    .into(),
                            }));
                        }
                        Err(InTxError::Sqlite(e)) => {
                            drop(tx);
                            return Err(tokio_rusqlite::Error::Other(Box::new(e)));
                        }
                        Err(InTxError::Codec(e)) => {
                            drop(tx);
                            return Err(tokio_rusqlite::Error::Other(Box::new(e)));
                        }
                        Err(InTxError::Invariant(s)) => {
                            drop(tx);
                            return Ok::<_, tokio_rusqlite::Error>(Err(StoreError::Invariant {
                                what: s,
                            }));
                        }
                        Err(InTxError::Terminal(e)) => {
                            drop(tx);
                            return Ok::<_, tokio_rusqlite::Error>(Err(e));
                        }
                    }
                }
            })
            .await??;

        Ok(outcome)
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

        let outcome = conn
            .call(move |c| {
                let start = std::time::Instant::now();
                let deadline = start + RESOLVE_BUSY_DEADLINE;
                let mut backoff_ms: u64 = INITIAL_BACKOFF_MS;
                loop {
                    // BEGIN IMMEDIATE acquires a RESERVED lock up front so
                    // a concurrent writer can't sneak in between our SELECT
                    // and our UPDATE — under WAL this avoids the
                    // SQLITE_BUSY_SNAPSHOT class of failures that DEFERRED
                    // hits when a reader tries to upgrade after another
                    // connection commits. Cross-process bursts therefore
                    // deterministically converge through the retry loop
                    // instead of escaping as terminal store errors. BEGIN
                    // IMMEDIATE itself can also return SQLITE_BUSY when
                    // another connection holds the write lock past
                    // busy_timeout; we classify that as transient and
                    // retry through the same backoff path the in-tx body
                    // uses for UniqueViolation / StaleSnapshot.
                    let tx_result =
                        c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate);
                    match tx_result {
                        Ok(tx) => {
                            let res = resolve_or_create_in_tx(
                                &tx,
                                &user,
                                &agent,
                                project_root.as_deref(),
                                idle_window_secs,
                                &identity_clone,
                                &metadata_clone,
                            );
                            match res {
                                Ok(outcome) => {
                                    tx.commit()?;
                                    return Ok::<_, tokio_rusqlite::Error>(Ok(outcome));
                                }
                                Err(InTxError::UniqueViolation | InTxError::StaleSnapshot) => {
                                    // Drop tx → ROLLBACK; fall through to
                                    // backoff + retry.
                                    drop(tx);
                                }
                                Err(InTxError::Sqlite(e)) => {
                                    drop(tx);
                                    return Err(tokio_rusqlite::Error::Other(Box::new(e)));
                                }
                                Err(InTxError::Codec(e)) => {
                                    drop(tx);
                                    return Err(tokio_rusqlite::Error::Other(Box::new(e)));
                                }
                                Err(InTxError::Invariant(s)) => {
                                    drop(tx);
                                    return Ok::<_, tokio_rusqlite::Error>(Err(
                                        StoreError::Invariant { what: s },
                                    ));
                                }
                                Err(InTxError::Terminal(e)) => {
                                    drop(tx);
                                    return Ok::<_, tokio_rusqlite::Error>(Err(e));
                                }
                            }
                        }
                        Err(e) => {
                            if let rusqlite::Error::SqliteFailure(err, _) = &e {
                                let code = err.code as i32;
                                if code != rusqlite::ffi::SQLITE_BUSY
                                    && err.extended_code != rusqlite::ffi::SQLITE_BUSY_SNAPSHOT
                                {
                                    return Err(tokio_rusqlite::Error::Other(Box::new(e)));
                                }
                                // Transient — fall through to backoff.
                            } else {
                                return Err(tokio_rusqlite::Error::Other(Box::new(e)));
                            }
                        }
                    }

                    let now = std::time::Instant::now();
                    if now >= deadline {
                        let elapsed_ms =
                            u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
                        return Ok::<_, tokio_rusqlite::Error>(Err(StoreError::Busy {
                            operation: "resolve_or_create_session",
                            elapsed_ms,
                        }));
                    }
                    // Truncated exponential backoff with deterministic jitter
                    // (LCG over the elapsed nanoseconds — no rand dep needed).
                    let elapsed_ns = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
                    let jitter = (elapsed_ns.wrapping_mul(6_364_136_223_846_793_005))
                        .rotate_left(13)
                        & 0x3FF; // 0..1023 ≈ up to ~1 ms when divided by 1024
                    let raw_sleep_ms = backoff_ms.saturating_add(jitter / 1024);
                    let remaining_ms =
                        u64::try_from((deadline - now).as_millis()).unwrap_or(u64::MAX);
                    let sleep_ms = raw_sleep_ms.min(remaining_ms.max(1));
                    std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
                    backoff_ms = (backoff_ms.saturating_mul(2)).min(MAX_BACKOFF_MS);
                }
            })
            .await??;

        Ok(outcome)
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
        let n = conn
            .call(move |c| {
                let n = retry_busy(
                    "touch_session",
                    |c| {
                        let now_ms = current_unix_ms();
                        c.execute(
                            "UPDATE sessions SET last_activity_at = ?1 \
                             WHERE session_id = ?2 \
                               AND user_id = ?3 \
                               AND agent_id = ?4 \
                               AND project_root IS ?5 \
                               AND ended_at IS NULL",
                            params![now_ms, key, user, agent, project_root],
                        )
                        .map_err(BusyOr::Sql)
                    },
                    c,
                );
                match n {
                    Ok(n) => Ok::<_, tokio_rusqlite::Error>(Ok(n)),
                    Err(StoreError::Sqlite(e)) => Err(tokio_rusqlite::Error::Other(Box::new(e))),
                    Err(other) => Ok::<_, tokio_rusqlite::Error>(Err(other)),
                }
            })
            .await??;
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
        let n = conn
            .call(move |c| {
                let n = retry_busy(
                    "end_session",
                    |c| {
                        let now_ms = current_unix_ms();
                        c.execute(
                            "UPDATE sessions SET ended_at = ?1 \
                             WHERE session_id = ?2 \
                               AND user_id = ?3 \
                               AND agent_id = ?4 \
                               AND project_root IS ?5 \
                               AND ended_at IS NULL",
                            params![now_ms, key, user, agent, project_root],
                        )
                        .map_err(BusyOr::Sql)
                    },
                    c,
                );
                match n {
                    Ok(n) => Ok::<_, tokio_rusqlite::Error>(Ok(n)),
                    Err(StoreError::Sqlite(e)) => Err(tokio_rusqlite::Error::Other(Box::new(e))),
                    Err(other) => Ok::<_, tokio_rusqlite::Error>(Err(other)),
                }
            })
            .await??;
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

/// Wrapper that distinguishes a retriable BUSY from any other error a
/// caller's closure might return. Used by [`retry_busy`].
enum BusyOr {
    /// `SQLITE_BUSY` / `SQLITE_BUSY_SNAPSHOT` — retry within the deadline.
    Sql(rusqlite::Error),
}

/// Run a single-statement write closure under the same deadline-driven
/// busy-retry policy [`SqliteMemoryStore::resolve_or_create_session`] uses.
///
/// `f` returns `Ok(T)` on success, `Err(BusyOr::Sql(e))` for any rusqlite
/// error. The helper itself classifies `e` as transient
/// (`SQLITE_BUSY` / `SQLITE_BUSY_SNAPSHOT`) and retries with truncated
/// exponential backoff + jitter, or surfaces it as
/// `Err(StoreError::Sqlite(_))` on a non-busy failure. After the deadline
/// it returns `Err(StoreError::Busy { operation, elapsed_ms })`.
/// Sleep one truncated-exponential-backoff step (with jitter), bounded by
/// the operation's deadline. Used by paths that interleave multiple
/// retry classes (busy + stale-snapshot + unique-violation) and need
/// inline backoff between attempts. Mutates `backoff_ms` for the next
/// iteration. The caller still has to check `Instant::now() >= deadline`
/// itself before continuing.
fn sleep_with_backoff_or_break(
    start: std::time::Instant,
    deadline: std::time::Instant,
    backoff_ms: &mut u64,
) {
    let now = std::time::Instant::now();
    if now >= deadline {
        return;
    }
    let elapsed_ns = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let jitter = (elapsed_ns.wrapping_mul(6_364_136_223_846_793_005)).rotate_left(13) & 0x3FF;
    let raw_sleep_ms = backoff_ms.saturating_add(jitter / 1024);
    let remaining_ms = u64::try_from((deadline - now).as_millis()).unwrap_or(u64::MAX);
    let sleep_ms = raw_sleep_ms.min(remaining_ms.max(1));
    std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
    *backoff_ms = backoff_ms.saturating_mul(2).min(MAX_BACKOFF_MS);
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
fn session_from_row(row: SessionRow) -> Result<Session, InTxError> {
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

fn retry_busy<T, F>(
    operation: &'static str,
    mut f: F,
    c: &mut rusqlite::Connection,
) -> Result<T, StoreError>
where
    F: FnMut(&mut rusqlite::Connection) -> Result<T, BusyOr>,
{
    let start = std::time::Instant::now();
    let deadline = start + RESOLVE_BUSY_DEADLINE;
    let mut backoff_ms: u64 = INITIAL_BACKOFF_MS;
    loop {
        match f(c) {
            Ok(v) => return Ok(v),
            Err(BusyOr::Sql(e)) => {
                let is_busy = if let rusqlite::Error::SqliteFailure(err, _) = &e {
                    let code = err.code as i32;
                    code == rusqlite::ffi::SQLITE_BUSY
                        || err.extended_code == rusqlite::ffi::SQLITE_BUSY_SNAPSHOT
                } else {
                    false
                };
                if !is_busy {
                    return Err(StoreError::Sqlite(e));
                }
                let now = std::time::Instant::now();
                if now >= deadline {
                    let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
                    return Err(StoreError::Busy {
                        operation,
                        elapsed_ms,
                    });
                }
                let elapsed_ns = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
                let jitter =
                    (elapsed_ns.wrapping_mul(6_364_136_223_846_793_005)).rotate_left(13) & 0x3FF;
                let raw_sleep_ms = backoff_ms.saturating_add(jitter / 1024);
                let remaining_ms = u64::try_from((deadline - now).as_millis()).unwrap_or(u64::MAX);
                let sleep_ms = raw_sleep_ms.min(remaining_ms.max(1));
                std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
                backoff_ms = backoff_ms.saturating_mul(2).min(MAX_BACKOFF_MS);
            }
        }
    }
}

/// Internal error type for the in-tx body of
/// [`SqliteMemoryStore::resolve_or_create_session`]. Distinguishes the
/// retryable conflicts from terminal failures so the outer loop can choose
/// to spin or surface the error.
#[derive(Debug)]
enum InTxError {
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
type SessionRow = (
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

fn read_session_by_id(
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
