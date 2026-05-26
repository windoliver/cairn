//! [`AdminStateStore`] impl backed by `SQLite`.
//!
//! Tables:
//! - `admin_roles` — operator role grants/revocations (migration 0003).
//! - `connector_state` — connector enable/disable state (migration 0004).
//!
//! Follows the same pattern as [`crate::identity`]: owns its own
//! `parking_lot::Mutex<rusqlite::Connection>`, applies its migrations at open
//! time via `include_str!`, and exposes sync constructors (`open_in_memory`,
//! `open`).

use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension as _};

use cairn_core::contract::{AdminStateStore, ConnectorStateRow};
use cairn_core::contract::memory_store::StoreError;
use cairn_core::domain::Identity;
use cairn_core::domain::admin::AdminRole;

/// Compiled-in migration DDL for `admin_roles` (issue #161).
const MIGRATION_0003: &str = include_str!("../migrations/0003_admin_roles.sql");
/// Compiled-in migration DDL for `connector_state` (issue #161).
const MIGRATION_0004: &str = include_str!("../migrations/0004_connector_state.sql");

/// `SQLite`-backed implementation of [`AdminStateStore`].
///
/// Each instance wraps a single `rusqlite` [`Connection`] behind an
/// [`Arc`]`<`[`Mutex`]`>` so it can be shared safely. The mutex is
/// `parking_lot` — never held across an `.await` point.
///
/// Construct via [`SqliteAdminStateStore::open_in_memory`] (tests) or
/// [`SqliteAdminStateStore::open`] (production).
pub struct SqliteAdminStateStore {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteAdminStateStore {
    /// Open an in-memory `SQLite` database and apply the admin migrations.
    ///
    /// Intended for unit and integration tests; the database is discarded when
    /// the returned value is dropped.
    ///
    /// # Errors
    /// Returns a [`StoreError`] if the connection or migration fails.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory().map_err(|e| Box::new(e) as StoreError)?;
        Self::configure_connection(&conn)?;
        Self::run_migrations(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Open (or create) a `SQLite` database at `db_path` and apply the admin
    /// migrations.
    ///
    /// # Errors
    /// Returns a [`StoreError`] if the connection or migration fails.
    pub fn open(db_path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(db_path).map_err(|e| Box::new(e) as StoreError)?;
        Self::configure_connection(&conn)?;
        Self::run_migrations(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Apply per-connection pragmas.
    fn configure_connection(conn: &Connection) -> Result<(), StoreError> {
        conn.execute_batch("PRAGMA foreign_keys = ON")
            .map_err(|e| Box::new(e) as StoreError)?;
        Ok(())
    }

    /// Apply admin migrations (0003 + 0004) if the tables are absent.
    fn run_migrations(conn: &Connection) -> Result<(), StoreError> {
        let has_admin_roles: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema \
                 WHERE type = 'table' AND name = 'admin_roles'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map_err(|e| Box::new(e) as StoreError)?
            > 0;

        if !has_admin_roles {
            conn.execute_batch(MIGRATION_0003)
                .map_err(|e| Box::new(e) as StoreError)?;
        }

        let has_connector_state: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema \
                 WHERE type = 'table' AND name = 'connector_state'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map_err(|e| Box::new(e) as StoreError)?
            > 0;

        if !has_connector_state {
            conn.execute_batch(MIGRATION_0004)
                .map_err(|e| Box::new(e) as StoreError)?;
        }

        Ok(())
    }

    fn role_str(role: AdminRole) -> &'static str {
        match role {
            AdminRole::Operator => "operator",
            // `AdminRole` is `#[non_exhaustive]`; if new variants are added
            // without updating this match, the compiler will warn.
            _ => "unknown",
        }
    }
}

impl AdminStateStore for SqliteAdminStateStore {
    fn has_role(&self, actor: &Identity, role: AdminRole) -> Result<bool, StoreError> {
        let conn = self.conn.lock();
        let role_s = Self::role_str(role);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM admin_roles \
                 WHERE identity_id = ?1 AND role = ?2 AND revoked_at IS NULL",
                (actor.as_str(), role_s),
                |r| r.get(0),
            )
            .map_err(|e| Box::new(e) as StoreError)?;
        Ok(count > 0)
    }

    fn has_any_operator(&self) -> Result<bool, StoreError> {
        let conn = self.conn.lock();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM admin_roles \
                 WHERE role = 'operator' AND revoked_at IS NULL",
                [],
                |r| r.get(0),
            )
            .map_err(|e| Box::new(e) as StoreError)?;
        Ok(count > 0)
    }

    fn grant_role(
        &self,
        actor: &Identity,
        role: AdminRole,
        granted_by: &Identity,
    ) -> Result<(), StoreError> {
        let conn = self.conn.lock();
        let role_s = Self::role_str(role);
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO admin_roles \
               (identity_id, role, granted_at, granted_by, revoked_at) \
             VALUES (?1, ?2, ?3, ?4, NULL) \
             ON CONFLICT(identity_id, role) DO UPDATE SET \
               revoked_at  = NULL, \
               granted_at  = excluded.granted_at, \
               granted_by  = excluded.granted_by",
            (actor.as_str(), role_s, &now, granted_by.as_str()),
        )
        .map_err(|e| Box::new(e) as StoreError)?;
        Ok(())
    }

    fn revoke_role(&self, actor: &Identity, role: AdminRole) -> Result<(), StoreError> {
        let conn = self.conn.lock();
        let role_s = Self::role_str(role);
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE admin_roles \
             SET revoked_at = ?3 \
             WHERE identity_id = ?1 AND role = ?2 AND revoked_at IS NULL",
            (actor.as_str(), role_s, &now),
        )
        .map_err(|e| Box::new(e) as StoreError)?;
        Ok(())
    }

    fn set_connector_enabled(
        &self,
        connector_name: &str,
        enabled: bool,
        changed_by: &Identity,
        reason: Option<&str>,
    ) -> Result<ConnectorStateRow, StoreError> {
        let conn = self.conn.lock();
        let now: DateTime<Utc> = Utc::now();
        let now_s = now.to_rfc3339();
        conn.execute(
            "INSERT INTO connector_state \
               (connector_name, enabled, last_changed_at, last_changed_by, reason) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(connector_name) DO UPDATE SET \
               enabled         = excluded.enabled, \
               last_changed_at = excluded.last_changed_at, \
               last_changed_by = excluded.last_changed_by, \
               reason          = excluded.reason",
            (
                connector_name,
                i64::from(enabled),
                &now_s,
                changed_by.as_str(),
                reason,
            ),
        )
        .map_err(|e| Box::new(e) as StoreError)?;
        Ok(ConnectorStateRow::new(
            connector_name.to_owned(),
            enabled,
            now,
            changed_by.clone(),
            reason.map(str::to_owned),
        ))
    }

    fn get_connector_state(
        &self,
        connector_name: &str,
    ) -> Result<Option<ConnectorStateRow>, StoreError> {
        let conn = self.conn.lock();
        let row = conn
            .query_row(
                "SELECT enabled, last_changed_at, last_changed_by, reason \
                 FROM connector_state WHERE connector_name = ?1",
                [connector_name],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| Box::new(e) as StoreError)?;

        match row {
            None => Ok(None),
            Some((en, ts, by, reason)) => {
                let last_changed_at = parse_ts(&ts)?;
                let last_changed_by = parse_identity(&by)?;
                Ok(Some(ConnectorStateRow::new(
                    connector_name.to_owned(),
                    en != 0,
                    last_changed_at,
                    last_changed_by,
                    reason,
                )))
            }
        }
    }

    fn list_connector_state(&self) -> Result<Vec<ConnectorStateRow>, StoreError> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT connector_name, enabled, last_changed_at, last_changed_by, reason \
                 FROM connector_state ORDER BY connector_name",
            )
            .map_err(|e| Box::new(e) as StoreError)?;

        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            })
            .map_err(|e| Box::new(e) as StoreError)?;

        let mut out = Vec::new();
        for r in rows {
            let (name, en, ts, by, reason) = r.map_err(|e| Box::new(e) as StoreError)?;
            out.push(ConnectorStateRow::new(
                name,
                en != 0,
                parse_ts(&ts)?,
                parse_identity(&by)?,
                reason,
            ));
        }
        Ok(out)
    }
}

fn parse_ts(s: &str) -> Result<DateTime<Utc>, StoreError> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| Box::new(e) as StoreError)
}

fn parse_identity(s: &str) -> Result<Identity, StoreError> {
    Identity::parse(s.to_owned()).map_err(|e| Box::new(e) as StoreError)
}
