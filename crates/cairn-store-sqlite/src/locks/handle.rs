//! `LockHandle` — owned acquired-lock state with epoch-aware fencing.
//!
//! `with_fencing` is the brief §5.6 per-holder fencing CAS, executed as the
//! first statement inside the caller's `BEGIN IMMEDIATE` so a stale writer's
//! `COMMIT` fails closed.

use std::sync::Arc;

use rusqlite::params;
use tokio_rusqlite::Connection;

use super::error::{LockError, default_fenced_retry};
use super::kinds::ResourceKey;

/// Handle for an acquired lock. Drop releases (best-effort); TTL reclaim is
/// the safety net for crashed holders.
///
/// `acquired_epoch` and `owner_incarnation` are cached at acquisition; the
/// fencing CAS run by `with_fencing` re-asserts both before any side-effect.
#[must_use = "drop the LockHandle to release; holding it idle keeps the lock"]
pub struct LockHandle {
    resource: String,
    holder_id: String,
    acquired_at: i64,
    acquired_epoch: i64,
    owner_incarnation: Arc<str>,
    conn: Arc<Connection>,
}

impl std::fmt::Debug for LockHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LockHandle")
            .field("resource", &self.resource)
            .field("holder_id", &self.holder_id)
            .field("acquired_at", &self.acquired_at)
            .field("acquired_epoch", &self.acquired_epoch)
            .finish_non_exhaustive()
    }
}

impl LockHandle {
    /// Construct directly. `pub(super)` so only `acquire.rs` can mint handles.
    #[allow(dead_code, reason = "wired by acquire.rs in Task 6")]
    pub(super) fn new(
        resource: String,
        holder_id: String,
        acquired_at: i64,
        acquired_epoch: i64,
        owner_incarnation: Arc<str>,
        conn: Arc<Connection>,
    ) -> Self {
        Self {
            resource,
            holder_id,
            acquired_at,
            acquired_epoch,
            owner_incarnation,
            conn,
        }
    }

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

    /// Acquired-at wall-clock millisecond timestamp.
    #[must_use]
    pub fn acquired_at(&self) -> i64 {
        self.acquired_at
    }

    /// Epoch this holder cached at acquisition.
    #[must_use]
    pub fn acquired_epoch(&self) -> i64 {
        self.acquired_epoch
    }

    /// Best-effort liveness check; NOT a substitute for `with_fencing`.
    ///
    /// Returns true iff a `lock_holders` row matching `(resource, holder_id,
    /// acquired_epoch, owner_incarnation)` still exists. Suitable for read-only
    /// diagnostics and tests; write paths MUST use `with_fencing`.
    ///
    /// # Errors
    /// `LockError::Db` on connection failure.
    pub async fn is_still_held(&self) -> Result<bool, LockError> {
        let resource = self.resource.clone();
        let holder_id = self.holder_id.clone();
        let acquired_epoch = self.acquired_epoch;
        let inc = self.owner_incarnation.to_string();
        let count: i64 = self
            .conn
            .call(move |c| {
                Ok::<i64, tokio_rusqlite::Error>(c.query_row(
                    "SELECT COUNT(*) FROM lock_holders \
                      WHERE resource = ?1 AND holder_id = ?2 \
                        AND acquired_epoch = ?3 AND owner_incarnation = ?4",
                    params![resource, holder_id, acquired_epoch, inc],
                    |row| row.get(0),
                )?)
            })
            .await?;
        Ok(count > 0)
    }

    /// Open a `BEGIN IMMEDIATE` transaction, run the per-holder fencing CAS
    /// as the first statement, then invoke `f(&mut Transaction)` if both
    /// (a) `locks.epoch == self.acquired_epoch` AND (b) the matching
    /// `lock_holders` row still exists with the same `owner_incarnation`.
    /// `COMMIT` happens after `f` returns Ok; any error or panic rolls back.
    ///
    /// # Errors
    /// - `LockError::Fenced` if the CAS fails (epoch advanced or holder GC'd).
    /// - `LockError::Db` on connection / `SQLite` failure.
    /// - Whatever `f` returns, mapped through `LockError::Db` for non-Lock
    ///   errors (callers wanting custom error types should construct their
    ///   own error inside `f` and rely on rollback).
    pub async fn with_fencing<F, R>(&self, f: F) -> Result<R, LockError>
    where
        F: for<'tx> FnOnce(&mut rusqlite::Transaction<'tx>) -> rusqlite::Result<R> + Send + 'static,
        R: Send + 'static,
    {
        let resource = self.resource.clone();
        let holder_id = self.holder_id.clone();
        let acquired_epoch = self.acquired_epoch;
        let inc = self.owner_incarnation.to_string();

        let outcome: Result<R, LockError> = self
            .conn
            .call(move |c| {
                let mut tx =
                    c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                // CAS: read group epoch + holder liveness in one statement.
                let (group_epoch, holder_alive): (Option<i64>, i64) = tx.query_row(
                    "SELECT \
                       (SELECT epoch FROM locks WHERE resource = ?1) AS group_epoch, \
                       EXISTS (SELECT 1 FROM lock_holders \
                                WHERE resource = ?1 AND holder_id = ?2 \
                                  AND acquired_epoch = ?3 AND owner_incarnation = ?4) \
                            AS holder_alive",
                    params![resource, holder_id, acquired_epoch, inc],
                    |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, i64>(1)?)),
                )?;
                let observed = group_epoch.unwrap_or(-1);
                if observed != acquired_epoch || holder_alive == 0 {
                    // Rollback by dropping tx without commit.
                    return Ok::<Result<R, LockError>, tokio_rusqlite::Error>(Err(
                        LockError::Fenced {
                            resource: resource.clone(),
                            expected_epoch: acquired_epoch,
                            observed_epoch: observed,
                            retry: default_fenced_retry(),
                        },
                    ));
                }
                // CAS passed — run the user's work.
                match f(&mut tx) {
                    Ok(r) => {
                        tx.commit()?;
                        Ok(Ok(r))
                    }
                    Err(e) => {
                        // tx drops without commit -> implicit rollback
                        drop(tx);
                        Ok(Err(LockError::Db(tokio_rusqlite::Error::Rusqlite(e))))
                    }
                }
            })
            .await?;
        outcome
    }

    /// Explicit release (idempotent). Equivalent to `drop` but reports errors.
    ///
    /// # Errors
    /// `LockError::Db` on connection failure.
    pub async fn release(self) -> Result<(), LockError> {
        let resource = self.resource.clone();
        let holder_id = self.holder_id.clone();
        let acquired_epoch = self.acquired_epoch;
        let inc = self.owner_incarnation.to_string();
        let conn = Arc::clone(&self.conn);
        std::mem::forget(self);
        release_inner(&conn, &resource, &holder_id, acquired_epoch, &inc).await
    }
}

impl Drop for LockHandle {
    fn drop(&mut self) {
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let conn = Arc::clone(&self.conn);
        let resource = std::mem::take(&mut self.resource);
        let holder_id = std::mem::take(&mut self.holder_id);
        let acquired_epoch = self.acquired_epoch;
        let inc = self.owner_incarnation.to_string();
        tokio::spawn(async move {
            let _ = release_inner(&conn, &resource, &holder_id, acquired_epoch, &inc).await;
        });
    }
}

/// Release by `(resource, holder_id, acquired_epoch, owner_incarnation)`.
/// Idempotent: a no-op if the row is already gone.
///
/// # Errors
/// `LockError::Db` on connection failure.
pub async fn release_by_holder(
    conn: &Arc<Connection>,
    resource: &ResourceKey,
    holder_id: &str,
    acquired_epoch: i64,
    owner_incarnation: &str,
) -> Result<(), LockError> {
    release_inner(
        conn,
        &resource.as_resource_str(),
        holder_id,
        acquired_epoch,
        owner_incarnation,
    )
    .await
}

async fn release_inner(
    conn: &Arc<Connection>,
    resource: &str,
    holder_id: &str,
    acquired_epoch: i64,
    owner_incarnation: &str,
) -> Result<(), LockError> {
    let resource = resource.to_owned();
    let holder_id = holder_id.to_owned();
    let inc = owner_incarnation.to_owned();
    conn.call(move |c| {
        c.execute(
            "DELETE FROM lock_holders \
              WHERE resource = ?1 AND holder_id = ?2 \
                AND acquired_epoch = ?3 AND owner_incarnation = ?4",
            params![resource, holder_id, acquired_epoch, inc],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await?;
    Ok(())
}
