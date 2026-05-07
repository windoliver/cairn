//! `LockHandle` — owned acquired-lock state with epoch-aware fencing.
//!
//! `with_fencing` is the brief §5.6 per-holder fencing CAS, executed as the
//! first statement inside the caller's `BEGIN IMMEDIATE` so a stale writer's
//! `COMMIT` fails closed.

use std::sync::Arc;

use rusqlite::params;
use tokio_rusqlite::Connection;

use super::error::{LockError, default_fenced_retry};

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
    /// `SQLite` ROWID of the `lock_holders` row at acquisition. Indexed,
    /// fast for predicate matching. NOT unique on its own across
    /// acquisitions because `SQLite` reuses ROWIDs after delete — the
    /// `acquisition_ulid` below is the authoritative per-acquisition
    /// unique identity.
    acquired_rowid: i64,
    /// Cryptographically unique per-acquisition identifier minted in
    /// `acquire()`. Combined with `rowid` in fence/release predicates
    /// so a stale handle cannot match a later acquisition that landed
    /// at the same ROWID slot after the original was GC'd.
    acquisition_ulid: String,
    conn: Arc<Connection>,
    /// Set when an explicit `release()` succeeds, so `Drop` becomes a no-op.
    /// Avoids leaking the heap fields + `Arc<Connection>` via `mem::forget`.
    released: bool,
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
    #[allow(
        clippy::too_many_arguments,
        reason = "explicit acquisition identity tuple"
    )]
    pub(super) fn new(
        resource: String,
        holder_id: String,
        acquired_at: i64,
        acquired_epoch: i64,
        acquired_rowid: i64,
        acquisition_ulid: String,
        conn: Arc<Connection>,
    ) -> Self {
        Self {
            resource,
            holder_id,
            acquired_at,
            acquired_epoch,
            acquired_rowid,
            acquisition_ulid,
            conn,
            released: false,
        }
    }

    /// `SQLite` ROWID of the `lock_holders` row at acquisition.
    /// Use together with `acquisition_ulid()` for the full identity tuple.
    #[must_use]
    pub fn acquired_rowid(&self) -> i64 {
        self.acquired_rowid
    }

    /// Per-acquisition ULID minted at acquire-time. The authoritative
    /// per-acquisition unique identifier (ROWID can collide after delete).
    #[must_use]
    pub fn acquisition_ulid(&self) -> &str {
        &self.acquisition_ulid
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
        let acq_ulid = self.acquisition_ulid.clone();
        // `acquisition_ulid` is the per-acquisition unique identifier.
        // ROWID alone can collide across delete/insert cycles, so we
        // match on the ULID. Indexed via lock_holders_acquisition_ulid_idx.
        let count: i64 = self
            .conn
            .call(move |c| {
                Ok::<i64, tokio_rusqlite::Error>(c.query_row(
                    "SELECT COUNT(*) FROM lock_holders WHERE acquisition_ulid = ?1",
                    params![acq_ulid],
                    |row| row.get(0),
                )?)
            })
            .await?;
        Ok(count > 0)
    }

    /// Open a `BEGIN IMMEDIATE` transaction, run the per-holder fencing CAS
    /// as the first statement, then invoke `f(&mut Transaction)` if all of:
    ///   (a) `locks.epoch == self.acquired_epoch`
    ///   (b) a `lock_holders` row exists matching the full identity tuple
    ///       (`resource`, `holder_id`, `acquired_epoch`, `owner_incarnation`,
    ///       `acquired_at`) — `acquired_at` is the per-acquisition
    ///       discriminator that prevents a stale handle from aliasing a
    ///       later acquisition that reused `holder_id`.
    ///   (c) the matched row's `expires_at` is strictly in the future —
    ///       a writer whose lease elapsed cannot commit even if the GC
    ///       sweep has not yet observed the expiry.
    ///
    /// `COMMIT` happens after `f` returns Ok; any error or panic rolls back.
    ///
    /// # Errors
    /// - `LockError::Fenced` if the CAS fails (epoch advanced, holder GC'd,
    ///   identity tuple mismatch, or lease expired).
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
        let acquired_epoch = self.acquired_epoch;
        let acq_ulid = self.acquisition_ulid.clone();

        let outcome: Result<R, LockError> = self
            .conn
            .call(move |c| {
                let mut tx =
                    c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                // Capture `now_ms` INSIDE the closure, AFTER `BEGIN IMMEDIATE`,
                // so queueing latency cannot bind a stale timestamp that
                // admits an already-expired lease. SQLite's
                // `strftime('%s','now')` is second-aligned (too coarse for
                // sub-second TTLs), so use Rust's millisecond clock.
                let Ok(now_ms) = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|_| ())
                    .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
                else {
                    drop(tx);
                    return Ok::<Result<R, LockError>, tokio_rusqlite::Error>(Err(
                        LockError::Clock,
                    ));
                };
                // CAS: read group epoch + holder liveness in one statement.
                // EXISTS keys on `acquisition_ulid` (per-acquisition unique,
                // immune to ROWID reuse) AND `expires_at > now_ms`, so a
                // TTL-elapsed handle or a stale handle whose row was GC'd
                // and replaced fails closed deterministically.
                let (group_epoch, holder_alive): (Option<i64>, i64) = tx.query_row(
                    "SELECT \
                       (SELECT epoch FROM locks WHERE resource = ?1) AS group_epoch, \
                       EXISTS (SELECT 1 FROM lock_holders \
                                WHERE acquisition_ulid = ?2 AND expires_at > ?3) \
                            AS holder_alive",
                    params![resource, acq_ulid, now_ms],
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
    /// On success, the `released` flag short-circuits `Drop` — no retry
    /// spawn, no resource leak. The handle's heap allocations + the
    /// `Arc<Connection>` strong reference are reclaimed when the consumed
    /// `self` goes out of scope at end-of-function. This is the no-leak
    /// alternative to `mem::forget(self)`.
    ///
    /// On error, the flag stays `false` and `Drop` runs as a fallback,
    /// fire-and-forgetting another DELETE attempt against the connection.
    /// Callers see the error and can retry explicitly via `release_by_holder`
    /// or rely on Drop + TTL reclaim.
    ///
    /// # Errors
    /// `LockError::Db` on connection failure.
    pub async fn release(mut self) -> Result<(), LockError> {
        let acq_ulid = self.acquisition_ulid.clone();
        let conn = Arc::clone(&self.conn);
        match release_inner(&conn, &acq_ulid).await {
            Ok(()) => {
                self.released = true;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
}

impl Drop for LockHandle {
    fn drop(&mut self) {
        // Explicit release already DELETED the row — nothing to retry.
        if self.released {
            return;
        }
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let conn = Arc::clone(&self.conn);
        let acq_ulid = std::mem::take(&mut self.acquisition_ulid);
        tokio::spawn(async move {
            let _ = release_inner(&conn, &acq_ulid).await;
        });
    }
}

/// Release by per-acquisition ULID. Idempotent: a no-op if the row is
/// already gone. Caller obtains the ULID from `LockHandle::acquisition_ulid()`.
///
/// # Errors
/// `LockError::Db` on connection failure.
pub async fn release_by_holder(
    conn: &Arc<Connection>,
    acquisition_ulid: &str,
) -> Result<(), LockError> {
    release_inner(conn, acquisition_ulid).await
}

async fn release_inner(conn: &Arc<Connection>, acquisition_ulid: &str) -> Result<(), LockError> {
    let acq = acquisition_ulid.to_owned();
    conn.call(move |c| {
        c.execute(
            "DELETE FROM lock_holders WHERE acquisition_ulid = ?1",
            params![acq],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await?;
    Ok(())
}
