//! `wal_ops` issue, prepare, and finalize helpers for record operations.

use cairn_core::wal::{OpState, OperationId, WalKind};
use rusqlite::{Connection, params};

use crate::error::StoreError;
use crate::store::current_unix_ms;

pub(crate) fn new_operation_id(kind: WalKind) -> Result<OperationId, StoreError> {
    let raw = format!("{}-{}", kind.as_str(), ulid::Ulid::new());
    OperationId::parse(raw).map_err(|e| StoreError::Invariant {
        what: format!("generated invalid operation id: {e}"),
    })
}

pub(crate) fn issue_prepared(
    conn: &Connection,
    op_id: &OperationId,
    kind: WalKind,
    target_hash: &str,
    scope_json: &str,
) -> Result<(), StoreError> {
    let now = current_unix_ms();
    conn.execute(
        "INSERT INTO wal_ops \
           (operation_id, issued_seq, kind, state, envelope, issuer, principal, \
            target_hash, scope_json, plan_ref, expires_at, signature, issued_at, updated_at) \
         VALUES (?1, COALESCE((SELECT MAX(issued_seq) FROM wal_ops), 0) + 1, ?2, \
            'ISSUED', '{}', 'cairn-store-sqlite', NULL, ?3, ?4, NULL, 0, 'local', ?5, ?5)",
        params![op_id.as_str(), kind.as_str(), target_hash, scope_json, now],
    )?;
    conn.execute(
        "UPDATE wal_ops SET state = 'PREPARED', updated_at = ?1 WHERE operation_id = ?2",
        params![now, op_id.as_str()],
    )?;
    Ok(())
}

pub(crate) fn finalize(
    conn: &Connection,
    op_id: &OperationId,
    state: OpState,
    reason: &str,
) -> Result<(), StoreError> {
    let now = current_unix_ms();
    conn.execute(
        "UPDATE wal_ops SET state = ?1, reason = COALESCE(reason, ?2), updated_at = ?3 \
         WHERE operation_id = ?4",
        params![state.as_str(), reason, now, op_id.as_str()],
    )?;
    Ok(())
}
