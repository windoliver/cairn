//! Shared helpers for `cairn admin {grant, replay-wal, connector …}` subcommands.
//!
//! The async main `cairn_store_sqlite::open` runs every migration in the
//! `crates/cairn-store-sqlite/src/migrations/sql/` chain, including
//! 0070 / 0071 which provision `admin_roles` + `connector_state`. Sync
//! admin subcommands need those tables to exist before opening
//! `SqliteAdminStateStore` so the main store's drift check
//! (`verify.rs::EXPECTED_OBJECTS` + `schema_migrations` integrity) never
//! sees a partial state.
//!
//! [`ensure_main_store_schema`] is a single entry point that any sync
//! admin path can call before performing its work.

use std::path::Path;

use anyhow::{Context as _, Result};

/// Open the main async store on its own short-lived tokio runtime so
/// every migration in the chain is applied. A returned `Ok(())`
/// guarantees that on the next sync [`cairn_store_sqlite::SqliteAdminStateStore::open`]
/// the admin tables exist AND `schema_migrations` already has rows 70 / 71.
///
/// # Errors
/// Surfaces any error from the underlying async store open (migration
/// failure, drift check, IO).
pub fn ensure_main_store_schema(db_path: &Path) -> Result<()> {
    // A dedicated single-thread runtime keeps this helper callable from
    // any sync caller without imposing tokio context requirements on the
    // surrounding code. `tokio_rusqlite` runs the async worker inside.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime for admin schema bootstrap")?;
    rt.block_on(async {
        let store = cairn_store_sqlite::open(db_path)
            .await
            .with_context(|| format!("open main store at {}", db_path.display()))?;
        drop(store);
        anyhow::Ok(())
    })
}
