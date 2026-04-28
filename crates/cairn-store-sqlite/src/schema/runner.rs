//! Forward-only migration runner. Each migration runs in its own
//! rusqlite transaction; the ledger row is inserted in the same
//! transaction so failures leave the ledger in a consistent state.

use super::Migration;
use crate::error::SqliteStoreError;
use rusqlite::{Connection, OptionalExtension, params};

/// Bootstrap DDL: create the `schema_migrations` ledger table if it
/// doesn't exist. Run before iterating migrations so migration 0008
/// can be applied on the first pass (it creates the same table —
/// the `CREATE TABLE IF NOT EXISTS` guard is idempotent).
const META_BOOTSTRAP: &str = "
CREATE TABLE IF NOT EXISTS schema_migrations (
  id          INTEGER NOT NULL PRIMARY KEY,
  name        TEXT NOT NULL,
  checksum    TEXT NOT NULL,
  applied_at  TEXT NOT NULL
) STRICT;
";

/// Apply any migrations that have not yet been recorded in
/// `schema_migrations`. For already-applied migrations, verify the
/// stored checksum matches the embedded SQL; abort on mismatch.
///
/// Forward-only — no down migrations.
pub fn apply_pending(
    conn: &mut Connection,
    migrations: &[Migration],
) -> Result<(), SqliteStoreError> {
    conn.execute_batch(META_BOOTSTRAP)?;

    for m in migrations {
        let existing: Option<String> = conn
            .query_row(
                "SELECT checksum FROM schema_migrations WHERE id = ?1",
                params![m.id],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(prev) = existing {
            if prev != m.checksum {
                return Err(SqliteStoreError::ChecksumMismatch {
                    migration: m.name.to_string(),
                });
            }
            // Already applied and checksum matches — skip.
            continue;
        }

        // Not yet applied: execute in a single transaction with the
        // ledger insert so both succeed or both fail together.
        let tx = conn.transaction()?;
        tx.execute_batch(m.sql)
            .map_err(|e| SqliteStoreError::Migration {
                migration: m.name.to_string(),
                source: e,
            })?;
        tx.execute(
            "INSERT INTO schema_migrations (id, name, checksum, applied_at) \
             VALUES (?1, ?2, ?3, datetime('now'))",
            params![m.id, m.name, m.checksum],
        )?;
        tx.commit()?;
    }
    Ok(())
}
