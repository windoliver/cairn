//! `SqliteMemoryStore` impl modules.

pub(crate) mod edges;
pub(crate) mod projection;
pub(crate) mod read;
pub(crate) mod tombstone;
pub(crate) mod trait_impl;
pub(crate) mod tx;
pub(crate) mod upsert;

use std::sync::Arc;

use tokio_rusqlite::Connection as AsyncConn;

/// Async-fronted `SQLite` memory store.
///
/// Two construction paths:
///
/// - [`SqliteMemoryStore::default`] — unconnected registry stub used by
///   the `register_plugin!` macro for identity/capability advertisement.
///   Trait verb methods return a "not initialized" error.
/// - [`crate::open()`] / [`crate::open_in_memory()`] — connected store with
///   pragmas + migrations applied. Trait verb methods route through the
///   wrapped `tokio_rusqlite::Connection` on a dedicated DB thread.
///
/// Construction is side-effect free per brief §4.1; the `open` path is
/// the only side-effecting one.
#[derive(Default, Clone)]
pub struct SqliteMemoryStore {
    pub(crate) conn: Option<Arc<AsyncConn>>,
}

impl std::fmt::Debug for SqliteMemoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteMemoryStore")
            .field("connected", &self.conn.is_some())
            .finish_non_exhaustive()
    }
}
