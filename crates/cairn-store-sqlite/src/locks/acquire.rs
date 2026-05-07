//! `acquire` — typed lock acquisition with epoch fencing (brief §5.6).
//!
//! Single `SQLite` transaction:
//!   1. GC stale holders (expired TTL OR mismatched `owner_incarnation`).
//!   2. Recompute live holder count.
//!   3. Decide:
//!      - empty / all-GC'd → seed/bump `locks.epoch` + `INSERT lock_holders`.
//!      - live > 0 AND `mode == :wanted == Shared` → `INSERT lock_holders`; epoch unchanged.
//!      - any incompatibility → `LockError::Held` with rich context.
//!   4. Cache (`holder_id`, `acquired_epoch`, `owner_incarnation`) in returned handle.
//!
//! `locks.(mode, holder_count)` are derived columns: the AFTER INSERT/DELETE
//! triggers from migration 0004 maintain them. We only ever set `epoch`
//! ourselves; the trigger handles `mode`/`holder_count`.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::params;
use tokio_rusqlite::Connection;

use super::error::{LockError, default_fenced_retry, default_held_retry};
use super::handle::LockHandle;
use super::kinds::{LockMode, ResourceKey};

/// Acquire a lock at `resource` with the requested `mode`.
///
/// `holder_id` should uniquely identify the caller (process+task scope, e.g.
/// a ULID or `pid={}-{}`). `ttl` is the maximum hold time before another
/// caller may reclaim — the recovery path for crashed holders.
///
/// `owner_incarnation` is the `Arc<str>` from `init_incarnation` — wired
/// through `Store::lock_context()` (Task 9).
///
/// `operation` is a free-form caller-supplied label (verb name, etc.) that
/// shows up in `LockError::Held` for diagnostics.
///
/// # Errors
/// - `LockError::Held` if a non-expired holder exists and modes are incompatible.
/// - `LockError::Db` on connection / `SQLite` failure.
/// - `LockError::Clock` if the system clock is before UNIX epoch.
#[allow(
    clippy::too_many_arguments,
    reason = "verb-shape: callers pass scope, mode, holder_id, ttl, incarnation, operation each at the call site"
)]
#[allow(
    clippy::too_many_lines,
    reason = "single transaction body: GC + read + decision branches kept colocated"
)]
pub async fn acquire(
    conn: &Arc<Connection>,
    resource: &ResourceKey,
    mode: LockMode,
    holder_id: &str,
    ttl: Duration,
    owner_incarnation: &Arc<str>,
    operation: &str,
) -> Result<LockHandle, LockError> {
    let resource_str = resource.as_resource_str();
    let ttl_ms = i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX);

    let inc_owned = owner_incarnation.to_string();
    let holder_owned = holder_id.to_owned();
    let resource_owned = resource_str.clone();
    let mode_str = mode.as_db_str();
    let operation_owned = operation.to_owned();

    let outcome = conn
        .call(move |c| {
            // tokio_rusqlite serializes work onto a dedicated DB thread, so a
            // timestamp captured before `conn.call` is queued can be stale by
            // the time it lands in the INSERT. Compute now_ms / expires_at
            // INSIDE the transaction closure so the lease window starts from
            // the moment we actually own the SQLite write lock — preventing
            // the next acquirer from reclaiming a fresh holder due to
            // queueing latency. A clock failure here propagates as a typed
            // `AcquisitionOutcome::Clock` so the outer match preserves the
            // documented `LockError::Clock` (vs. stringifying through Db).
            let Ok(now_ms) = system_time_ms() else {
                return Ok::<AcquisitionOutcome, tokio_rusqlite::Error>(AcquisitionOutcome::Clock);
            };
            let expires_at = now_ms.saturating_add(ttl_ms);

            let tx = c.transaction()?;

            // 0. Liveness check: confirm the caller's cached incarnation still
            //    matches the on-disk `daemon_incarnation` singleton. If a
            //    second process has opened the DB and minted a fresh
            //    incarnation, the caller is stale — fail closed instead of
            //    deleting the live owner's holder rows in step 1.
            let on_disk_incarnation: Option<String> = match tx.query_row(
                "SELECT incarnation_id FROM daemon_incarnation WHERE id = 1",
                [],
                |row| row.get::<_, String>(0),
            ) {
                Ok(s) => Some(s),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                // Any other SQL error (table missing, type mismatch, etc.) is
                // a real store failure — propagate as Db so callers see the
                // root cause instead of a synthesized stale-incarnation fence.
                Err(e) => return Err(e.into()),
            };
            if on_disk_incarnation.as_deref() != Some(inc_owned.as_str()) {
                drop(tx);
                return Ok::<AcquisitionOutcome, tokio_rusqlite::Error>(
                    AcquisitionOutcome::Stale {
                        resource: resource_owned,
                        caller_incarnation: inc_owned,
                        on_disk_incarnation: on_disk_incarnation.unwrap_or_default(),
                    },
                );
            }

            // 1. GC: expired OR not-current-incarnation. The AFTER DELETE
            //    trigger will decrement `locks.holder_count` and reset
            //    `locks.mode` to 'NONE' if the count hits 0. Step 0 ensures
            //    `inc_owned` IS the on-disk current incarnation, so this
            //    DELETE only removes truly stale rows.
            tx.execute(
                "DELETE FROM lock_holders \
                  WHERE resource = ?1 \
                    AND (expires_at <= ?2 OR owner_incarnation != ?3)",
                params![resource_owned, now_ms, inc_owned],
            )?;

            // 2. Read current state.
            let (current_mode, current_epoch, live): (Option<String>, i64, i64) = tx
                .query_row(
                    "SELECT l.mode, l.epoch, COUNT(h.holder_id) AS live \
                       FROM locks l \
                       LEFT JOIN lock_holders h \
                         ON h.resource = l.resource \
                      WHERE l.resource = ?1 \
                      GROUP BY l.resource",
                    params![resource_owned],
                    |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok((None, 0, 0)),
                    other => Err(other),
                })?;

            // 3. Decide.
            // a) No locks row yet OR live=0 (all GC'd) — reclaim/seed path.
            //    We seed/bump `locks.epoch` only; the AFTER INSERT trigger
            //    on `lock_holders` upgrades `(mode, holder_count)` from
            //    ('NONE', 0) to (NEW.mode_requested, 1) atomically.
            if current_mode.is_none() || live == 0 {
                let new_epoch = if current_mode.is_some() {
                    current_epoch + 1 // reclaim => bump
                } else {
                    1 // first ever
                };
                if current_mode.is_none() {
                    // Seed a fresh `locks` row in the canonical "no holders"
                    // shape; the trigger upgrades it on the holder INSERT below.
                    tx.execute(
                        "INSERT INTO locks(resource, mode, holder_count, updated_at, epoch) \
                         VALUES (?1, 'NONE', 0, ?2, ?3)",
                        params![resource_owned, now_ms, new_epoch],
                    )?;
                } else {
                    // Existing row whose holders just GC'd — bump epoch only.
                    // The DELETE trigger has already reset mode to 'NONE' /
                    // holder_count to 0.
                    tx.execute(
                        "UPDATE locks SET epoch = ?2, updated_at = ?3 \
                          WHERE resource = ?1",
                        params![resource_owned, new_epoch, now_ms],
                    )?;
                }
                tx.execute(
                    "INSERT INTO lock_holders(resource, holder_id, mode_requested, \
                       acquired_at, expires_at, acquired_epoch, owner_incarnation) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        resource_owned,
                        holder_owned,
                        mode_str,
                        now_ms,
                        expires_at,
                        new_epoch,
                        inc_owned
                    ],
                )?;
                tx.commit()?;
                return Ok::<AcquisitionOutcome, tokio_rusqlite::Error>(AcquisitionOutcome::Ok {
                    acquired_epoch: new_epoch,
                    acquired_at: now_ms,
                });
            }

            // b) Compatible Shared+Shared. The AFTER INSERT trigger
            //    increments `locks.holder_count`; mode stays 'SHARED'.
            if current_mode.as_deref() == Some("SHARED") && mode_str == "SHARED" {
                tx.execute(
                    "INSERT INTO lock_holders(resource, holder_id, mode_requested, \
                       acquired_at, expires_at, acquired_epoch, owner_incarnation) \
                     VALUES (?1, ?2, 'SHARED', ?3, ?4, ?5, ?6)",
                    params![
                        resource_owned,
                        holder_owned,
                        now_ms,
                        expires_at,
                        current_epoch,
                        inc_owned
                    ],
                )?;
                tx.commit()?;
                return Ok(AcquisitionOutcome::Ok {
                    acquired_epoch: current_epoch,
                    acquired_at: now_ms,
                });
            }

            // c) Incompatible — fetch incumbent details for the rich error.
            let (incumbent_holder, incumbent_acquired_at, incumbent_expires_at): (
                String,
                i64,
                i64,
            ) = tx.query_row(
                "SELECT holder_id, acquired_at, expires_at FROM lock_holders \
                  WHERE resource = ?1 \
                  ORDER BY acquired_at ASC LIMIT 1",
                params![resource_owned],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            let incumbent_mode = match current_mode.as_deref() {
                Some("SHARED") => LockMode::Shared,
                _ => LockMode::Exclusive,
            };
            // Rollback by dropping tx without commit.
            drop(tx);
            Ok(AcquisitionOutcome::Held {
                resource: resource_owned,
                mode: incumbent_mode,
                operation: operation_owned,
                current_holder: incumbent_holder,
                ttl_remaining_ms: incumbent_expires_at.saturating_sub(now_ms),
                since_ms: now_ms.saturating_sub(incumbent_acquired_at),
            })
        })
        .await?;

    match outcome {
        AcquisitionOutcome::Ok {
            acquired_epoch,
            acquired_at,
        } => Ok(LockHandle::new(
            resource_str,
            holder_id.to_owned(),
            acquired_at,
            acquired_epoch,
            Arc::clone(owner_incarnation),
            Arc::clone(conn),
        )),
        AcquisitionOutcome::Clock => Err(LockError::Clock),
        AcquisitionOutcome::Held {
            resource,
            mode,
            operation,
            current_holder,
            ttl_remaining_ms,
            since_ms,
        } => Err(LockError::Held {
            resource,
            mode,
            operation,
            current_holder,
            ttl_remaining_ms,
            since_ms,
            retry: default_held_retry(),
        }),
        AcquisitionOutcome::Stale {
            resource,
            caller_incarnation,
            on_disk_incarnation,
        } => {
            tracing::warn!(
                resource = %resource,
                caller_incarnation = %caller_incarnation,
                on_disk_incarnation = %on_disk_incarnation,
                "lock acquire rejected: caller's cached incarnation does not match \
                 on-disk daemon_incarnation singleton — caller is stale"
            );
            // Caller's cached epoch is meaningless once their incarnation is
            // stale; report (-1, -1) sentinels so callers can distinguish the
            // stale-incarnation case from a normal epoch-bump fence.
            Err(LockError::Fenced {
                resource,
                expected_epoch: -1,
                observed_epoch: -1,
                retry: default_fenced_retry(),
            })
        }
    }
}

enum AcquisitionOutcome {
    Ok {
        acquired_epoch: i64,
        /// Wall-clock ms captured INSIDE the txn closure — the source of truth
        /// for the holder's lease window. Threaded back to `LockHandle::new`
        /// so external callers see the actual acquisition timestamp, not the
        /// pre-queue value.
        acquired_at: i64,
    },
    /// In-tx clock read failed (`SystemTime` pre-epoch). Surfaced as the
    /// typed `LockError::Clock` by the outer `match` — preserves the
    /// documented error contract across the closure boundary.
    Clock,
    Held {
        resource: String,
        mode: LockMode,
        operation: String,
        current_holder: String,
        ttl_remaining_ms: i64,
        since_ms: i64,
    },
    /// Caller's cached `owner_incarnation` does not match the on-disk
    /// `daemon_incarnation` singleton — caller is a stale process. Surfaced
    /// as `LockError::Fenced` so existing retry logic kicks in.
    Stale {
        resource: String,
        caller_incarnation: String,
        on_disk_incarnation: String,
    },
}

#[allow(
    clippy::result_large_err,
    reason = "inherits LockError for ?-propagation"
)]
fn system_time_ms() -> Result<i64, LockError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| LockError::Clock)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::locks::incarnation::init_incarnation;
    use crate::open::open_in_memory;
    use std::sync::Arc;
    use std::time::Duration;

    async fn setup() -> (
        crate::SqliteMemoryStore,
        Arc<tokio_rusqlite::Connection>,
        Arc<str>,
    ) {
        let store = open_in_memory().await.unwrap();
        let conn = Arc::clone(store.raw_conn_for_admin().unwrap());
        let inc = init_incarnation(&conn).await.unwrap();
        (store, conn, inc)
    }

    #[tokio::test]
    async fn acquire_exclusive_succeeds_when_free() {
        let (_store, conn, inc) = setup().await;
        let r = ResourceKey::vault("v1");
        let h = acquire(
            &conn,
            &r,
            LockMode::Exclusive,
            "h_a",
            Duration::from_secs(2),
            &inc,
            "test",
        )
        .await
        .unwrap();
        assert_eq!(h.acquired_epoch(), 1);
    }

    #[tokio::test]
    async fn acquire_exclusive_blocked_by_existing() {
        let (_store, conn, inc) = setup().await;
        let r = ResourceKey::vault("v1");
        let _h1 = acquire(
            &conn,
            &r,
            LockMode::Exclusive,
            "h_a",
            Duration::from_secs(2),
            &inc,
            "op_a",
        )
        .await
        .unwrap();
        let err = acquire(
            &conn,
            &r,
            LockMode::Exclusive,
            "h_b",
            Duration::from_secs(2),
            &inc,
            "op_b",
        )
        .await
        .unwrap_err();
        match err {
            LockError::Held {
                current_holder,
                operation,
                ttl_remaining_ms,
                ..
            } => {
                assert_eq!(current_holder, "h_a");
                assert_eq!(operation, "op_b");
                assert!(ttl_remaining_ms > 0);
            }
            other => panic!("expected Held, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn acquire_shared_compatible_with_existing_shared() {
        let (_store, conn, inc) = setup().await;
        let r = ResourceKey::session("t1", "default", "sess");
        let _h1 = acquire(
            &conn,
            &r,
            LockMode::Shared,
            "h_a",
            Duration::from_secs(2),
            &inc,
            "op_a",
        )
        .await
        .unwrap();
        let h2 = acquire(
            &conn,
            &r,
            LockMode::Shared,
            "h_b",
            Duration::from_secs(2),
            &inc,
            "op_b",
        )
        .await
        .unwrap();
        // Both share the same epoch — Shared+Shared does NOT bump epoch.
        assert_eq!(h2.acquired_epoch(), 1);
    }

    #[tokio::test]
    async fn acquire_after_expiry_bumps_epoch() {
        let (_store, conn, inc) = setup().await;
        let r = ResourceKey::vault("v1");
        let h1 = acquire(
            &conn,
            &r,
            LockMode::Exclusive,
            "h_a",
            Duration::from_millis(50),
            &inc,
            "a",
        )
        .await
        .unwrap();
        assert_eq!(h1.acquired_epoch(), 1);
        // Don't release — let it expire.
        std::mem::forget(h1);
        tokio::time::sleep(Duration::from_millis(120)).await;
        let h2 = acquire(
            &conn,
            &r,
            LockMode::Exclusive,
            "h_b",
            Duration::from_secs(2),
            &inc,
            "b",
        )
        .await
        .unwrap();
        assert_eq!(h2.acquired_epoch(), 2, "reclaim must bump epoch");
    }

    #[tokio::test]
    async fn with_fencing_passes_when_owned() {
        let (_store, conn, inc) = setup().await;
        let r = ResourceKey::entity("t1", "default", "rec_x");
        let h = acquire(
            &conn,
            &r,
            LockMode::Exclusive,
            "h_a",
            Duration::from_secs(2),
            &inc,
            "op",
        )
        .await
        .unwrap();
        let result: i64 = h
            .with_fencing(|tx| tx.query_row("SELECT 42", [], |row| row.get::<_, i64>(0)))
            .await
            .unwrap();
        assert_eq!(result, 42);
    }

    #[tokio::test]
    async fn with_fencing_fails_after_reclaim() {
        let (_store, conn, inc) = setup().await;
        let r = ResourceKey::vault("v1");
        let h1 = acquire(
            &conn,
            &r,
            LockMode::Exclusive,
            "h_a",
            Duration::from_millis(50),
            &inc,
            "a",
        )
        .await
        .unwrap();
        // Hold the handle, but let the lock expire.
        tokio::time::sleep(Duration::from_millis(120)).await;
        let _h2 = acquire(
            &conn,
            &r,
            LockMode::Exclusive,
            "h_b",
            Duration::from_secs(2),
            &inc,
            "b",
        )
        .await
        .unwrap();
        // h1's CAS must now fail.
        let err = h1
            .with_fencing(|tx| tx.query_row("SELECT 1", [], |row| row.get::<_, i64>(0)))
            .await
            .unwrap_err();
        match err {
            LockError::Fenced {
                expected_epoch,
                observed_epoch,
                ..
            } => {
                assert_eq!(expected_epoch, 1);
                assert_eq!(observed_epoch, 2);
            }
            other => panic!("expected Fenced, got {other:?}"),
        }
    }

    /// A caller whose cached `owner_incarnation` does not match the on-disk
    /// `daemon_incarnation` singleton is a stale process — `acquire` must
    /// fail closed with `LockError::Fenced` (sentinel `expected=-1, observed=-1`)
    /// BEFORE the GC step. This protects the live owner's holders from being
    /// deleted by a stale acquirer.
    #[tokio::test]
    async fn stale_caller_incarnation_is_rejected_before_gc() {
        let (_store, conn, current_inc) = setup().await;
        let r = ResourceKey::entity("t1", "default", "rec_protected");

        // Live owner under the current (real) incarnation.
        let _live = acquire(
            &conn,
            &r,
            LockMode::Exclusive,
            "live_owner",
            Duration::from_mins(1),
            &current_inc,
            "live",
        )
        .await
        .unwrap();

        // Construct a synthetic stale incarnation — a ULID that was never
        // the singleton. This simulates a process that captured an
        // incarnation `Arc<str>`, then a different process replaced the
        // singleton via `init_incarnation` while the first process kept
        // its cached value.
        let stale_inc: Arc<str> = Arc::from(ulid::Ulid::new().to_string());
        assert_ne!(&*stale_inc, &*current_inc, "synthetic must differ");

        let err = acquire(
            &conn,
            &r,
            LockMode::Exclusive,
            "stale_acquirer",
            Duration::from_secs(2),
            &stale_inc,
            "stale_op",
        )
        .await
        .unwrap_err();
        match err {
            LockError::Fenced {
                expected_epoch,
                observed_epoch,
                ..
            } => {
                assert_eq!(expected_epoch, -1, "sentinel for stale-incarnation");
                assert_eq!(observed_epoch, -1);
            }
            other => panic!("expected Fenced, got {other:?}"),
        }

        // The pre-flight check rejected before GC, so live_owner's row
        // must still exist — proving the stale acquirer was prevented
        // from corrupting the live owner's lease.
        let live_count: i64 = conn
            .call(|c| {
                Ok::<i64, tokio_rusqlite::Error>(c.query_row(
                    "SELECT COUNT(*) FROM lock_holders WHERE holder_id = 'live_owner'",
                    [],
                    |row| row.get(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(
            live_count, 1,
            "stale acquirer must NOT delete live owner's holder row"
        );
    }

    /// Regression for round-5 finding 1: a stale handle from a prior
    /// shared acquisition must NOT pass `with_fencing` after the row's
    /// TTL elapsed and a later acquirer recreated the same
    /// `(holder_id, acquired_epoch, owner_incarnation)` tuple. The
    /// per-acquisition `acquired_at` discriminator is what disambiguates.
    #[tokio::test]
    async fn stale_handle_blocked_after_holder_id_reuse() {
        let (_store, conn, inc) = setup().await;
        let r = ResourceKey::session("t1", "default", "shared_session");

        // First shared holder: short TTL.
        let h1 = acquire(
            &conn,
            &r,
            LockMode::Shared,
            "shared_caller",
            Duration::from_millis(60),
            &inc,
            "first",
        )
        .await
        .unwrap();
        let first_epoch = h1.acquired_epoch();
        let first_at = h1.acquired_at();
        std::mem::forget(h1); // simulate process pause; row stays until TTL

        // Wait past TTL so the row is reclaimable, then re-acquire with
        // the SAME holder_id. Tokio's mock-clock-free sleep is enough.
        tokio::time::sleep(Duration::from_millis(120)).await;

        // Bring shared count back via a fresh shared acquire (different
        // holder_id) so locks.epoch is forced to bump first, then the
        // reused holder_id can rejoin.
        let _h_other = acquire(
            &conn,
            &r,
            LockMode::Shared,
            "another_caller",
            Duration::from_mins(1),
            &inc,
            "other",
        )
        .await
        .unwrap();

        // Sleep a millisecond so the new acquire's acquired_at differs
        // even on fast clocks.
        tokio::time::sleep(Duration::from_millis(2)).await;
        let h_reused = acquire(
            &conn,
            &r,
            LockMode::Shared,
            "shared_caller", // SAME holder_id as h1
            Duration::from_mins(1),
            &inc,
            "reused",
        )
        .await
        .unwrap();
        // Either same epoch (Shared+Shared compat after first reclaim) or
        // higher; what matters is acquired_at differs from h1.
        assert_ne!(
            h_reused.acquired_at(),
            first_at,
            "fresh acquisition must have a distinct acquired_at"
        );
        let _ = first_epoch; // silence unused warning under -D warnings

        // The original h1 was forgotten; reconstruct the stale-handle
        // ownership predicate by hand and verify the CAS query rejects
        // it (no row matches the FULL identity tuple including h1's
        // acquired_at).
        let count: i64 = {
            let inc_str: String = inc.to_string();
            let r_str = r.as_resource_str();
            conn.call(move |c| {
                Ok::<i64, tokio_rusqlite::Error>(c.query_row(
                    "SELECT COUNT(*) FROM lock_holders \
                      WHERE resource = ?1 AND holder_id = ?2 \
                        AND acquired_at = ?3 AND owner_incarnation = ?4",
                    params![r_str, "shared_caller", first_at, inc_str],
                    |row| row.get(0),
                )?)
            })
            .await
            .unwrap()
        };
        assert_eq!(
            count, 0,
            "no row matches h1's stale identity tuple — CAS will reject it"
        );
    }

    /// Regression for round-5 finding 2: a handle whose lease elapsed
    /// must be rejected by `with_fencing`'s CAS even before any later
    /// `acquire()` GC sweep deletes the row.
    #[tokio::test]
    async fn with_fencing_rejects_expired_handle_before_gc() {
        let (_store, conn, inc) = setup().await;
        let r = ResourceKey::vault("v_expire");
        let h = acquire(
            &conn,
            &r,
            LockMode::Exclusive,
            "h_expiry",
            Duration::from_millis(40),
            &inc,
            "expiry_test",
        )
        .await
        .unwrap();
        // Wait past TTL but DO NOT call acquire() again — so no GC sweep.
        tokio::time::sleep(Duration::from_millis(80)).await;
        // Verify the row is still in the DB (not yet GC'd).
        let count: i64 = conn
            .call(|c| {
                Ok::<i64, tokio_rusqlite::Error>(c.query_row(
                    "SELECT COUNT(*) FROM lock_holders WHERE holder_id = 'h_expiry'",
                    [],
                    |row| row.get(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(count, 1, "row still present (no GC sweep ran)");
        // The CAS must still reject because expires_at has elapsed.
        let err = h
            .with_fencing(|tx| tx.query_row("SELECT 1", [], |row| row.get::<_, i64>(0)))
            .await
            .unwrap_err();
        match err {
            LockError::Fenced { .. } => {}
            other => panic!("expected Fenced for expired-but-not-yet-GC'd handle, got {other:?}"),
        }
    }
}
