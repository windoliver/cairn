//! `SQLite` schema migrations for the narrow lint slice.

use rusqlite::{Connection, params};

use crate::StoreError;

/// Create the `SQLite` tables and indexes required by edge linting.
pub fn migrate(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS entity_edges (
            id TEXT PRIMARY KEY,
            source_id TEXT NOT NULL,
            target_id TEXT NOT NULL,
            relation TEXT NOT NULL,
            valid_at INTEGER NOT NULL,
            invalid_at INTEGER,
            expired_at INTEGER,
            confidence TEXT NOT NULL CHECK (
                confidence IN ('EXTRACTED', 'INFERRED', 'AMBIGUOUS')
            ),
            confidence_score REAL NOT NULL CHECK (
                confidence_score >= 0.0 AND confidence_score <= 1.0
            ),
            created_at INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_entity_edges_live_triple
            ON entity_edges (source_id, target_id, relation)
            WHERE invalid_at IS NULL AND expired_at IS NULL;

        CREATE TABLE IF NOT EXISTS wal_ops (
            operation_id TEXT PRIMARY KEY,
            state TEXT NOT NULL,
            kind TEXT NOT NULL,
            reason TEXT NOT NULL,
            envelope TEXT NOT NULL,
            issued_at INTEGER NOT NULL,
            committed_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS replay_ledger (
            operation_id TEXT PRIMARY KEY,
            reason TEXT NOT NULL,
            committed_at INTEGER NOT NULL
        );
        ",
    )?;

    Ok(())
}

pub(crate) fn ensure_table(conn: &Connection, table: &'static str) -> Result<(), StoreError> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS (
            SELECT 1 FROM sqlite_master
            WHERE type = 'table' AND name = ?1
        )",
        params![table],
        |row| row.get(0),
    )?;

    if exists {
        Ok(())
    } else {
        Err(StoreError::SchemaMissing { object: table })
    }
}
