//! [`IdentityRegistry`] `SQLite` adapter (issue #50).
//!
//! This module owns [`SqliteIdentityRegistry`], which backs
//! [`cairn_core::contract::identity_registry::IdentityRegistry`] against a
//! real `SQLite` database via `rusqlite`.  Schema is applied by embedding
//! `migrations/0002_identity.sql` at compile time; no external migration
//! runner is required.
//!
//! Submodule layout:
//! - [`queries`] — read-path SQL helpers (lands in C4+).
//! - [`wal`]     — `identity_wal` insert helper (lands in C4+).

mod queries;
mod wal;

use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;
use rusqlite::Connection;

use cairn_core::contract::identity_registry::RegistryError;

/// Compiled-in `0002_identity.sql` migration DDL.
const MIGRATION_0002: &str = include_str!("../../migrations/0002_identity.sql");

/// `SQLite`-backed implementation of [`cairn_core::contract::identity_registry::IdentityRegistry`].
///
/// Each instance wraps a single `rusqlite` [`Connection`] behind an
/// [`Arc`]`<`[`Mutex`]`>` so it can be shared across `async` tasks via
/// `spawn_blocking` (the mutex is `parking_lot` — never held across an
/// `.await` point).
///
/// Construct with [`SqliteIdentityRegistry::open_in_memory`] (tests) or
/// [`SqliteIdentityRegistry::open`] (production).
pub struct SqliteIdentityRegistry {
    // `IdentityRegistry` trait methods (C4+) consume this field; suppress the
    // dead_code lint until the impl lands.
    #[allow(dead_code)]
    conn: Arc<Mutex<Connection>>,
}

impl SqliteIdentityRegistry {
    /// Open an in-memory `SQLite` database and apply the identity migration.
    ///
    /// Intended for unit and integration tests; the database is discarded when
    /// the returned value is dropped.
    ///
    /// # Errors
    /// Returns [`RegistryError::Backend`] if the connection or migration fails.
    pub fn open_in_memory() -> Result<Self, RegistryError> {
        let conn = Connection::open_in_memory().map_err(|e| RegistryError::Backend(Box::new(e)))?;
        Self::run_migrations(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Open (or create) a `SQLite` database at `db_path` and apply the
    /// identity migration.
    ///
    /// # Errors
    /// Returns [`RegistryError::Backend`] if the connection or migration fails.
    pub fn open(db_path: &Path) -> Result<Self, RegistryError> {
        let conn = Connection::open(db_path).map_err(|e| RegistryError::Backend(Box::new(e)))?;
        Self::run_migrations(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Apply [`MIGRATION_0002`] to `conn` in a single `execute_batch` call.
    fn run_migrations(conn: &Connection) -> Result<(), RegistryError> {
        conn.execute_batch(MIGRATION_0002)
            .map_err(|e| RegistryError::Backend(Box::new(e)))
    }
}
