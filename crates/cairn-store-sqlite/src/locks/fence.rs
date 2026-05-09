//! Reader-fence API (brief §5.6).
//!
//! `register_pending` inserts a `reader_fence` row tied to a WAL `operation_id`.
//! `clear` flips state PENDING -> CLEARED (UPDATE; the existing 0004 trigger
//! `reader_fence_no_direct_delete` blocks DELETE until the linked op terminates).
//! `wait_for_drain` polls `reader_fence_pending_count` until 0 or `timeout`.
//!
//! No call sites use these helpers in this PR; follow-up issues (#57+ forget,
//! #50+ reindex) wire them into the WAL commit paths.

use std::sync::Arc;
use std::time::{Duration, Instant};

use rusqlite::params;
use tokio_rusqlite::Connection;

use super::error::{LockError, default_drain_retry};
use super::kinds::ResourceKey;

/// Register a PENDING reader fence row for `(resource, operation_id)`.
/// The `reader_fence_pending_idx` UNIQUE INDEX rejects a second PENDING
/// row for the same resource — callers must coordinate one fence at a time
/// per resource.
///
/// # Errors
/// `LockError::Db` on connection / uniqueness failure.
pub async fn register_pending(
    conn: &Arc<Connection>,
    resource: &ResourceKey,
    operation_id: &str,
) -> Result<(), LockError> {
    let resource_str = resource.as_resource_str();
    let op_id = operation_id.to_owned();
    conn.call(move |c| {
        c.execute(
            "INSERT INTO reader_fence(resource, operation_id, state, created_at) \
             VALUES (?1, ?2, 'PENDING', strftime('%s','now') * 1000)",
            params![resource_str, op_id],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await?;
    Ok(())
}

/// Flip state PENDING -> CLEARED for `(resource, operation_id)`.
/// Idempotent: a no-op if already CLEARED. The existing
/// `reader_fence_state_transition` trigger enforces the legal transition.
///
/// # Errors
/// `LockError::Db` on connection failure.
pub async fn clear(
    conn: &Arc<Connection>,
    resource: &ResourceKey,
    operation_id: &str,
) -> Result<(), LockError> {
    let resource_str = resource.as_resource_str();
    let op_id = operation_id.to_owned();
    conn.call(move |c| {
        c.execute(
            "UPDATE reader_fence \
                SET state = 'CLEARED', \
                    cleared_at = strftime('%s','now') * 1000 \
              WHERE resource = ?1 AND operation_id = ?2 AND state = 'PENDING'",
            params![resource_str, op_id],
        )?;
        Ok::<_, tokio_rusqlite::Error>(())
    })
    .await?;
    Ok(())
}

/// Poll `reader_fence_pending_count` until no PENDING rows remain for
/// `resource`, or `timeout` elapses. Polling interval: 25ms.
///
/// # Errors
/// - `LockError::DrainTimeout` if `timeout` elapses with PENDING rows still present.
/// - `LockError::Db` on connection failure.
pub async fn wait_for_drain(
    conn: &Arc<Connection>,
    resource: &ResourceKey,
    timeout: Duration,
) -> Result<(), LockError> {
    let resource_str = resource.as_resource_str();
    let started = Instant::now();
    loop {
        let pending: i64 = {
            let r = resource_str.clone();
            conn.call(move |c| {
                let n: i64 = c
                    .query_row(
                        "SELECT COALESCE(pending, 0) FROM reader_fence_pending_count \
                          WHERE resource = ?1",
                        params![r],
                        |row| row.get(0),
                    )
                    .or_else(|e| match e {
                        rusqlite::Error::QueryReturnedNoRows => Ok(0),
                        other => Err(other),
                    })?;
                Ok::<_, tokio_rusqlite::Error>(n)
            })
            .await?
        };
        if pending == 0 {
            return Ok(());
        }
        let waited = started.elapsed();
        if waited >= timeout {
            return Err(LockError::DrainTimeout {
                resource: resource_str.clone(),
                pending,
                waited_ms: waited.as_millis(),
                retry: default_drain_retry(resource_str),
            });
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::locks::incarnation::init_incarnation;
    use crate::open::open_in_memory;

    /// Insert a `wal_ops` row in COMMITTED state so `reader_fence_no_direct_delete`
    /// is satisfied if the test ever needs to delete. Returns the `operation_id`.
    ///
    /// Drives the row through ISSUED -> PREPARED -> COMMITTED so the §5.6
    /// FSM trigger (`wal_ops_state_transition`) accepts the path.
    async fn seed_committed_wal_op(conn: &Arc<Connection>) -> String {
        let op_id = ulid::Ulid::new().to_string();
        let op_id_clone = op_id.clone();
        conn.call(move |c| {
            // Pick a high `issued_seq` so concurrent tests on a shared in-memory
            // schema (none today, but cheap insurance) don't collide on the
            // strictly-advancing trigger.
            let issued_seq: i64 = c.query_row(
                "SELECT COALESCE(MAX(issued_seq), 0) + 1 FROM wal_ops",
                [],
                |row| row.get(0),
            )?;
            c.execute(
                "INSERT INTO wal_ops(\
                   operation_id, issued_seq, kind, state, envelope, issuer, \
                   principal, target_hash, scope_json, plan_ref, expires_at, \
                   signature, issued_at, updated_at) \
                 VALUES (?1, ?2, 'upsert', 'ISSUED', '{}', 'test', NULL, '', \
                         '{}', NULL, 9999999999999, '', 0, 0)",
                params![op_id_clone, issued_seq],
            )?;
            c.execute(
                "UPDATE wal_ops SET state = 'PREPARED', updated_at = 1 \
                  WHERE operation_id = ?1",
                params![op_id_clone],
            )?;
            c.execute(
                "UPDATE wal_ops SET state = 'COMMITTED', updated_at = 2 \
                  WHERE operation_id = ?1",
                params![op_id_clone],
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .unwrap();
        op_id
    }

    #[tokio::test]
    async fn register_then_clear_drains_immediately() {
        let store = open_in_memory().await.unwrap();
        let conn = Arc::clone(store.raw_conn_for_admin().unwrap());
        let _inc = init_incarnation(&conn).await.unwrap();
        let op_id = seed_committed_wal_op(&conn).await;
        let r = ResourceKey::vault("v1");

        register_pending(&conn, &r, &op_id).await.unwrap();
        clear(&conn, &r, &op_id).await.unwrap();
        wait_for_drain(&conn, &r, Duration::from_millis(100))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn wait_for_drain_times_out_when_pending_persists() {
        let store = open_in_memory().await.unwrap();
        let conn = Arc::clone(store.raw_conn_for_admin().unwrap());
        let _inc = init_incarnation(&conn).await.unwrap();
        let op_id = seed_committed_wal_op(&conn).await;
        let r = ResourceKey::vault("v_busy");

        register_pending(&conn, &r, &op_id).await.unwrap();
        let err = wait_for_drain(&conn, &r, Duration::from_millis(80))
            .await
            .unwrap_err();
        match err {
            LockError::DrainTimeout {
                resource,
                pending,
                waited_ms,
                ..
            } => {
                assert_eq!(resource, "vault:v_busy");
                assert_eq!(pending, 1);
                assert!(waited_ms >= 80);
            }
            other => panic!("expected DrainTimeout, got {other:?}"),
        }
    }
}
