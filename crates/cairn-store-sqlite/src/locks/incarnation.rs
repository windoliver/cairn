//! Daemon incarnation lifecycle (brief §5.6).
//!
//! `init_incarnation` runs once per `Store::open`, after migrations:
//!   1. Mints a fresh ULID for this daemon incarnation.
//!   2. INSERT OR REPLACE into the `daemon_incarnation` singleton.
//!   3. `DELETE FROM lock_holders WHERE owner_incarnation != :new`
//!      — drops both prior-incarnation rows and the `__pre_v2__` sentinel
//!      rows backfilled by migration 0050.
//!   4. Bumps `locks.epoch` on every affected resource so any in-flight CAS
//!      from a prior incarnation fails closed.
//!
//! Returned `Arc<str>` is cached by the connected `SqliteMemoryStore` and
//! threaded into every `acquire(...)` call.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::params;
use tokio_rusqlite::Connection;
use tracing::warn;

use super::error::LockError;

/// Mint a new incarnation, GC stale `lock_holders` rows, and bump epoch on
/// every affected resource. Idempotent: safe to call multiple times in tests.
///
/// # P0 single-writer-process assumption
///
/// Per brief §5.6 and the design spec
/// (`docs/superpowers/specs/2026-05-06-issue-56-locks-fencing-design.md`),
/// P0 ships with **one writer process per vault**. This routine
/// unconditionally reclaims any `lock_holders` row whose `owner_incarnation`
/// does not match the freshly-minted ULID — there is no `boot_id` /
/// BOOTTIME-ns liveness proof to distinguish "prior process died" from
/// "another process is alive right now". Both collapse to "reclaim and
/// proceed". A second concurrent open would therefore steal the first
/// writer's locks; that is the documented P0 limitation. `acquire`'s
/// pre-flight liveness check defends against the inverse direction (a
/// stale process can no longer GC the live owner's holders) by reading
/// the singleton inside its own transaction and failing closed.
///
/// **Deferred to P1+** (per design spec):
/// - BOOTTIME-ns lease clock + `boot_id` column for true liveness proof.
/// - Cross-process takeover handshake (the supervisor pattern).
///
/// Until those land, `cairn` is operated as a single writer per vault.
///
/// # Errors
/// `LockError::Db` on connection failure.
/// `LockError::Clock` if the system clock is before UNIX epoch.
pub async fn init_incarnation(conn: &Arc<Connection>) -> Result<Arc<str>, LockError> {
    let incarnation: Arc<str> = Arc::from(ulid::Ulid::new().to_string());
    let started_at = system_time_ms()?;
    let pid = i64::from(std::process::id());

    let inc_for_call: Arc<str> = Arc::clone(&incarnation);
    let inc_for_log: Arc<str> = Arc::clone(&incarnation);
    let observed = conn
        .call(move |c| {
            let tx = c.transaction()?;

            // 0. Snapshot the prior singleton, if any, for diagnostic logging.
            //    A non-empty prior incarnation means another writer process
            //    was here before — we are about to reclaim its lock_holders
            //    rows. P0 single-writer assumption (see fn-doc) intends this
            //    to be a once-per-cold-start event; a warning here surfaces
            //    multi-process misuse where the design spec defers the real
            //    fix (BOOTTIME-ns + boot_id liveness) to P1+.
            let prior: Option<(String, i64, i64)> = match tx.query_row(
                "SELECT incarnation_id, started_at, pid \
                   FROM daemon_incarnation WHERE id = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            ) {
                Ok(t) => Some(t),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => return Err(e.into()),
            };

            // 1. Upsert the singleton row. CHECK (id = 1) is enforced by 0004.
            tx.execute(
                "INSERT INTO daemon_incarnation (id, incarnation_id, started_at, pid) \
                 VALUES (1, ?1, ?2, ?3) \
                 ON CONFLICT(id) DO UPDATE SET \
                   incarnation_id = excluded.incarnation_id, \
                   started_at     = excluded.started_at, \
                   pid            = excluded.pid",
                params![&*inc_for_call, started_at, pid],
            )?;

            // 2. Find resources that will lose holders, so we can bump their epoch.
            //    Collect first; the DELETE that follows fires the count-after-delete
            //    trigger on locks but does NOT bump epoch (epoch is brief-defined as
            //    a reclaim signal, separate from holder_count maintenance).
            let mut stmt = tx.prepare(
                "SELECT DISTINCT resource FROM lock_holders \
                  WHERE owner_incarnation != ?1",
            )?;
            let affected: Vec<String> = stmt
                .query_map(params![&*inc_for_call], |row| row.get::<_, String>(0))?
                .collect::<Result<_, _>>()?;
            drop(stmt);

            // 3. GC stale holders.
            let gc_count = tx.execute(
                "DELETE FROM lock_holders WHERE owner_incarnation != ?1",
                params![&*inc_for_call],
            )?;

            // 4. Bump epoch on every affected resource.
            for resource in &affected {
                tx.execute(
                    "UPDATE locks SET epoch = epoch + 1, \
                                      updated_at = strftime('%s','now') * 1000 \
                      WHERE resource = ?1",
                    params![resource],
                )?;
            }

            tx.commit()?;
            Ok::<_, tokio_rusqlite::Error>((prior, gc_count, affected.len()))
        })
        .await?;

    // Visibility for the P0 multi-process anti-pattern. If we just GC'd
    // any holders, an operator running two `cairn` processes against one
    // vault will see a clear warning instead of silent data races.
    let (prior, gc_count, affected_count) = observed;
    if gc_count > 0 {
        warn!(
            new_incarnation = %inc_for_log,
            prior_incarnation = %prior.as_ref().map_or("<none>", |t| t.0.as_str()),
            prior_pid = prior.as_ref().map_or(0, |t| t.2),
            gc_count = gc_count,
            affected_resources = affected_count,
            "lock substrate reclaimed lock_holders rows from a different incarnation; \
             P0 single-writer assumption violated if the prior process is still alive \
             (design spec: BOOTTIME-ns + boot_id deferred to P1+)"
        );
    }

    Ok(incarnation)
}

/// Read the current incarnation from the singleton.
///
/// # Errors
/// `LockError::Db` on connection failure.
/// `LockError::NoIncarnation` if the singleton row is missing.
pub async fn current_incarnation(conn: &Arc<Connection>) -> Result<Arc<str>, LockError> {
    let row: Option<String> = conn
        .call(|c| {
            // Distinguish "row absent" (treated as NoIncarnation) from any
            // other SQLite failure. Using `.ok()` here would mask schema
            // drift / table-rename / type-coercion errors as a missing
            // incarnation, sending callers down the retry path instead of
            // surfacing the real fault.
            match c.query_row(
                "SELECT incarnation_id FROM daemon_incarnation WHERE id = 1",
                [],
                |row| row.get::<_, String>(0),
            ) {
                Ok(s) => Ok::<_, tokio_rusqlite::Error>(Some(s)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e.into()),
            }
        })
        .await?;
    row.map(Arc::from).ok_or(LockError::NoIncarnation)
}

// LockError is an `enum` whose `Held` / `Fenced` variants are deliberately
// rich for caller diagnostics; `result_large_err` flags this size at private
// helper boundaries even though the surrounding public API already returns it.
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
    use crate::open::open_in_memory;

    #[tokio::test]
    async fn init_incarnation_creates_singleton() {
        let store = open_in_memory().await.unwrap();
        let conn = store.raw_conn_for_admin().unwrap();
        let inc = init_incarnation(conn).await.unwrap();
        assert!(!inc.is_empty());
        let read_back = current_incarnation(conn).await.unwrap();
        assert_eq!(&*inc, &*read_back);
    }

    #[tokio::test]
    async fn second_init_drops_prior_holders_and_bumps_epoch() {
        let store = open_in_memory().await.unwrap();
        let conn = store.raw_conn_for_admin().unwrap();
        let inc1 = init_incarnation(conn).await.unwrap();

        // Plant a holder under inc1.
        let inc1_str: String = inc1.to_string();
        conn.call(move |c| {
            let tx = c.transaction()?;
            // Seed a `locks` row with epoch=5. We use mode=NONE/holder_count=0 so the
            // count-after-insert trigger upgrades it to EXCLUSIVE/1 cleanly; the trigger
            // only writes (mode, holder_count, updated_at) so epoch=5 is preserved.
            tx.execute(
                "INSERT INTO locks(resource, mode, holder_count, updated_at, epoch) \
                 VALUES ('vault:test', 'NONE', 0, 0, 5)",
                [],
            )?;
            tx.execute(
                "INSERT INTO lock_holders(resource, holder_id, mode_requested, \
                   acquired_at, expires_at, acquired_epoch, owner_incarnation) \
                 VALUES ('vault:test', 'holder_a', 'EXCLUSIVE', 0, 9999999999999, 5, ?1)",
                params![inc1_str],
            )?;
            tx.commit()?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .unwrap();

        // New incarnation: should GC the inc1 holder and bump epoch from 5 to 6.
        let inc2 = init_incarnation(conn).await.unwrap();
        assert_ne!(&*inc1, &*inc2);

        let (epoch, holder_count): (i64, i64) = conn
            .call(|c| {
                Ok::<_, tokio_rusqlite::Error>(c.query_row(
                    "SELECT epoch, (SELECT COUNT(*) FROM lock_holders WHERE resource = 'vault:test') \
                       FROM locks WHERE resource = 'vault:test'",
                    [],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(epoch, 6, "epoch must bump on incarnation change");
        assert_eq!(holder_count, 0, "prior-incarnation holder must be GC'd");
    }
}
