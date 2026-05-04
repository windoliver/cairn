//! In-transaction helpers for entity-graph WAL writes.
//!
//! Mirrors `identity/wal.rs` but writes to the generic `wal_ops` +
//! `wal_steps` tables (multi-step capable per migration 0002).
//! Production callers (Tasks 8–12) invoke these from inside a
//! `tokio_rusqlite::Connection::call(...)` closure that owns the
//! transaction; the helpers themselves are pure-sync `rusqlite`.

use rusqlite::Transaction;
use ulid::Ulid;

use crate::error::StoreError;

/// One day in milliseconds — default `expires_at` cushion when a caller
/// doesn't override it. WAL rows for graph ops are short-lived: the apply
/// path commits within the same transaction, so this is well above the
/// typical clock-skew tolerance for crash recovery (#55).
const ONE_DAY_MS: i64 = 24 * 60 * 60 * 1_000;

/// Insert a new `wal_ops` row in `ISSUED` state, then transition to `PREPARED`.
/// Returns the freshly-minted operation id (ULID, base32).
///
/// # Errors
/// Returns [`StoreError`] when the SQL fails (FK, CHECK, or trigger).
pub fn issue_op(tx: &Transaction<'_>, kind: &str, target_hash: &str) -> Result<String, StoreError> {
    let op_id = Ulid::new().to_string();
    let issued_seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(issued_seq), 0) + 1 FROM wal_ops",
        [],
        |r| r.get(0),
    )?;
    let now = chrono::Utc::now().timestamp_millis();
    tx.execute(
        "INSERT INTO wal_ops (operation_id, issued_seq, kind, state, envelope, \
         issuer, principal, target_hash, scope_json, expires_at, signature, \
         issued_at, updated_at) VALUES \
         (?1, ?2, ?3, 'ISSUED', '{}', 'cairn-store-sqlite', NULL, ?4, '{}', \
          ?5, '', ?6, ?6)",
        rusqlite::params![&op_id, issued_seq, kind, target_hash, now + ONE_DAY_MS, now],
    )?;
    tx.execute(
        "UPDATE wal_ops SET state = 'PREPARED', updated_at = ?2 WHERE operation_id = ?1",
        rusqlite::params![&op_id, now],
    )?;
    Ok(op_id)
}

/// Insert a `wal_steps` row in `PENDING`, then immediately transition
/// to `DONE`. Caller has already performed the underlying mutation in the
/// same transaction; this records the post-image marker.
///
/// `pre_image` is the serialized old-row blob for compensation (None on
/// fresh inserts).
///
/// # Errors
/// Returns [`StoreError`] when the SQL fails.
pub fn write_step(
    tx: &Transaction<'_>,
    op_id: &str,
    step_ord: i64,
    step_kind: &str,
    pre_image: Option<&[u8]>,
) -> Result<(), StoreError> {
    let now = chrono::Utc::now().timestamp_millis();
    tx.execute(
        "INSERT INTO wal_steps (operation_id, step_ord, step_kind, state, \
         attempts, pre_image, started_at) \
         VALUES (?1, ?2, ?3, 'PENDING', 1, ?4, ?5)",
        rusqlite::params![op_id, step_ord, step_kind, pre_image, now],
    )?;
    tx.execute(
        "UPDATE wal_steps SET state = 'DONE', finished_at = ?3 \
         WHERE operation_id = ?1 AND step_ord = ?2",
        rusqlite::params![op_id, step_ord, now],
    )?;
    Ok(())
}

/// Transition `wal_ops.state` from `PREPARED` to `COMMITTED`.
///
/// # Errors
/// Returns [`StoreError`] when the SQL fails (e.g. transition not allowed
/// by the FSM trigger).
pub fn commit_op(tx: &Transaction<'_>, op_id: &str) -> Result<(), StoreError> {
    let now = chrono::Utc::now().timestamp_millis();
    tx.execute(
        "UPDATE wal_ops SET state = 'COMMITTED', updated_at = ?2 \
         WHERE operation_id = ?1",
        rusqlite::params![op_id, now],
    )?;
    Ok(())
}
