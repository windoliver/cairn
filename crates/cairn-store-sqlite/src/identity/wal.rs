//! `identity_wal` insert helper for [`super::SqliteIdentityRegistry`] (issue #50).
//!
//! Every `IdentityRegistry` mutation must call [`wal_insert`] inside its
//! `SQLite` transaction so that a crash before commit rolls both the WAL row
//! and the record mutation back atomically (spec §3.5 / §5.6).

use chrono::Utc;
use rusqlite::Transaction;
use ulid::Ulid;

use cairn_core::contract::identity_registry::RegistryError;

/// Insert one row into `identity_wal` as part of a live transaction.
///
/// The `op_id` is a freshly-minted ULID (stored as 16 raw bytes). The
/// `applied_at` timestamp is the current UTC instant in RFC 3339 form.
///
/// # Errors
/// Returns [`RegistryError::Backend`] if the `INSERT` fails.
pub(super) fn wal_insert(
    tx: &Transaction<'_>,
    op_kind: &str,
    target: &str,
    request_payload: &[u8],
) -> Result<(), RegistryError> {
    let op_id = Ulid::new().to_bytes();
    tx.execute(
        "INSERT INTO identity_wal \
         (op_id, op_kind, target_identity, request_payload, applied_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            &op_id[..],
            op_kind,
            target,
            request_payload,
            Utc::now().to_rfc3339()
        ],
    )
    .map_err(|e| RegistryError::Backend(Box::new(e)))?;
    Ok(())
}
