//! Lock-table acquisition for §5.6.
//!
//! All locks live as rows in `.cairn/cairn.db`. `SQLite` write serialization
//! plus the schema's triggers (migration 0004) provide the mutual-exclusion
//! guarantee. **No OS advisory locks** (`flock`, `fcntl(F_SETLK)`) — the brief
//! rejects those at line 1815 of §5.6.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::params;
use thiserror::Error;
use tokio_rusqlite::Connection;

/// Lock-table errors.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LockError {
    /// Resource is held by a non-expired holder.
    #[error("lock held: scope_kind={scope_kind} scope_key={scope_key}")]
    Held {
        /// Scope kind.
        scope_kind: String,
        /// Scope key.
        scope_key: String,
    },
    /// Database or worker error.
    #[error("lock db error")]
    Db(#[source] tokio_rusqlite::Error),
    /// Clock pre-epoch.
    #[error("system clock pre-epoch")]
    Clock,
}

impl From<tokio_rusqlite::Error> for LockError {
    fn from(e: tokio_rusqlite::Error) -> Self {
        Self::Db(e)
    }
}

/// Handle for an acquired exclusive lock. Drop releases (best-effort);
/// expired-TTL reclaim is the safety net for crashed holders.
///
/// `acquired_at` is the millisecond timestamp the holder row was inserted
/// — a fencing token. Reclaim deletes the original row and inserts a new
/// one with a different `acquired_at`, so [`LockHandle::is_still_held`]
/// can distinguish "still owned by us" from "reclaimed by someone else".
#[must_use = "drop the LockHandle to release; holding it idle keeps the lock"]
pub struct LockHandle {
    resource: String,
    holder_id: String,
    acquired_at: i64,
    conn: Arc<Connection>,
}

impl std::fmt::Debug for LockHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LockHandle")
            .field("resource", &self.resource)
            .field("holder_id", &self.holder_id)
            .field("acquired_at", &self.acquired_at)
            .finish_non_exhaustive()
    }
}

impl LockHandle {
    /// Resource string for this handle.
    #[must_use]
    pub fn resource(&self) -> &str {
        &self.resource
    }

    /// Holder identifier.
    #[must_use]
    pub fn holder_id(&self) -> &str {
        &self.holder_id
    }

    /// Returns whether the holder row identified by this handle is still
    /// present with its original `acquired_at` fencing token.
    ///
    /// Long stalls (machine sleep, scheduler pause, GC pause) can let
    /// `expires_at` lapse even though the heartbeat task tried to refresh
    /// it. A second caller would then reclaim — deleting our row and
    /// inserting their own. Callers entering destructive phases (file
    /// writes, WAL commit) MUST recheck ownership and abort if `false`.
    ///
    /// # Errors
    /// `LockError::Db` on connection failure.
    pub async fn is_still_held(&self) -> Result<bool, LockError> {
        let resource = self.resource.clone();
        let holder_id = self.holder_id.clone();
        let acquired_at = self.acquired_at;
        let count: i64 = self
            .conn
            .call(move |c| {
                Ok::<i64, tokio_rusqlite::Error>(c.query_row(
                    "SELECT COUNT(*) FROM lock_holders \
                      WHERE resource = ?1 AND holder_id = ?2 AND acquired_at = ?3",
                    params![resource, holder_id, acquired_at],
                    |row| row.get(0),
                )?)
            })
            .await?;
        Ok(count > 0)
    }

    /// Acquired-at fencing token. Required by callers that hand-roll
    /// lock-row mutations (e.g. heartbeat renewals) so they can match
    /// the same `(resource, holder_id, acquired_at)` triple `release`
    /// uses — otherwise a stale handle can mutate a row that has already
    /// been reclaimed by a different caller reusing the same `holder_id`.
    #[must_use]
    pub fn acquired_at(&self) -> i64 {
        self.acquired_at
    }

    /// Explicit release (idempotent). Equivalent to `drop` but reports errors.
    ///
    /// # Errors
    /// `LockError::Db` on connection failure.
    pub async fn release(self) -> Result<(), LockError> {
        let resource = self.resource.clone();
        let holder_id = self.holder_id.clone();
        let acquired_at = self.acquired_at;
        let conn = Arc::clone(&self.conn);
        // Suppress Drop's fire-and-forget by forgetting self (we already cloned).
        std::mem::forget(self);
        release_inner(&conn, &resource, &holder_id, acquired_at).await
    }
}

impl Drop for LockHandle {
    fn drop(&mut self) {
        // `tokio::spawn` panics outside a tokio runtime. The lock module is
        // public API; callers might drop a handle from sync code or after
        // shutdown. Detect the runtime first; if absent, leave the row in
        // place — TTL reclaim is the documented recovery path. This trades
        // a best-effort early release for crash-safety in non-tokio drops.
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let conn = Arc::clone(&self.conn);
        let resource = std::mem::take(&mut self.resource);
        let holder_id = std::mem::take(&mut self.holder_id);
        let acquired_at = self.acquired_at;
        tokio::spawn(async move {
            let _ = release_inner(&conn, &resource, &holder_id, acquired_at).await;
        });
    }
}

/// Acquire an EXCLUSIVE lock at (`scope_kind`, `scope_key`).
///
/// `holder_id` should uniquely identify the caller (process+task scope, e.g. a
/// UUID or `pid={}-{}`). `ttl` is the maximum hold time before another caller
/// may reclaim — the recovery path for crashed holders.
///
/// # Errors
/// - `LockError::Held` if a non-expired holder exists.
/// - `LockError::Db` on connection failure.
/// - `LockError::Clock` if the system clock is before UNIX epoch.
pub async fn acquire_exclusive(
    conn: &Arc<Connection>,
    scope_kind: &str,
    scope_key: &str,
    holder_id: &str,
    ttl: Duration,
) -> Result<LockHandle, LockError> {
    let resource = format!("{scope_kind}:{scope_key}");
    let scope_kind = scope_kind.to_owned();
    let scope_key = scope_key.to_owned();
    let holder_id = holder_id.to_owned();

    let now_ms = system_time_ms()?;
    let ttl_ms = i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX);
    let expires_at = now_ms.saturating_add(ttl_ms);

    let resource_clone = resource.clone();
    let holder_id_clone = holder_id.clone();

    let result = conn
        .call(move |c| {
            let tx = c.transaction()?;

            // Step 3: reclaim expired holders (triggers update locks.holder_count).
            tx.execute(
                "DELETE FROM lock_holders WHERE resource = ?1 AND expires_at <= ?2",
                params![resource_clone, now_ms],
            )?;

            // Step 4: attempt EXCLUSIVE insert.
            let insert_result = tx.execute(
                "INSERT INTO lock_holders(resource, holder_id, mode_requested, acquired_at, expires_at) \
                 VALUES (?1, ?2, 'EXCLUSIVE', ?3, ?4)",
                params![resource_clone, holder_id_clone, now_ms, expires_at],
            );

            match insert_result {
                Ok(_) => {
                    tx.commit()?;
                    Ok(true)
                }
                Err(rusqlite::Error::SqliteFailure(_, Some(ref msg)))
                    if msg.contains("EXCLUSIVE holder requires no existing holders") =>
                {
                    // Rollback happens when tx is dropped (no commit).
                    Ok(false)
                }
                Err(e) => Err(tokio_rusqlite::Error::Rusqlite(e)),
            }
        })
        .await?;

    if result {
        Ok(LockHandle {
            resource,
            holder_id,
            acquired_at: now_ms,
            conn: Arc::clone(conn),
        })
    } else {
        Err(LockError::Held {
            scope_kind,
            scope_key,
        })
    }
}

/// Direct release by `(resource, holder_id, acquired_at)` — used by
/// callers (e.g. `lint --fix-markdown`'s post-commit retry) that need
/// to re-attempt deletion after `LockHandle::release` consumed the
/// handle on first failure. Idempotent: a no-op if the row is already
/// gone.
///
/// # Errors
/// `LockError::Db` on connection failure.
pub async fn release_by_holder(
    conn: &Arc<Connection>,
    resource: &str,
    holder_id: &str,
    acquired_at: i64,
) -> Result<(), LockError> {
    release_inner(conn, resource, holder_id, acquired_at).await
}

/// Delete this holder's row, releasing the lock.
/// Trigger `lock_holders_count_after_delete` updates `locks.(holder_count, mode)`.
async fn release_inner(
    conn: &Arc<Connection>,
    resource: &str,
    holder_id: &str,
    acquired_at: i64,
) -> Result<(), LockError> {
    let resource = resource.to_owned();
    let holder_id = holder_id.to_owned();
    conn.call(move |c| {
        c.execute(
            "DELETE FROM lock_holders \
              WHERE resource = ?1 AND holder_id = ?2 AND acquired_at = ?3",
            params![resource, holder_id, acquired_at],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await?;
    Ok(())
}

/// Current time as milliseconds since UNIX epoch, or `LockError::Clock` if
/// the system clock is before epoch.
fn system_time_ms() -> Result<i64, LockError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| LockError::Clock)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}
