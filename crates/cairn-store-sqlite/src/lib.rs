//! `SQLite` record store for Cairn (P0).
//!
//! Provides `SqliteMemoryStore` — a `rusqlite`-backed implementation of
//! both `MemoryStore` (read, via `store.rs`) and `MemoryStoreApply` (write,
//! via `apply.rs`). Opens `.cairn/cairn.db`, applies forward-only migrations,
//! and exposes the store behind a `tokio::sync::Mutex<Connection>` so async
//! callers can dispatch blocking work to `tokio::task::spawn_blocking`.
//!
//! The `register_plugin!` macro wires this crate into the `PluginRegistry`
//! at startup. Registration constructs the store via `Default::default()`,
//! which opens an in-memory `SQLite` database — safe and side-effect-free for
//! capability probes. Real usage goes through `SqliteMemoryStore::open(path)`.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod apply;
pub mod conn;
pub mod error;
pub mod rebac;
pub mod rowmap;
pub mod schema;
pub mod store;

use cairn_core::contract::memory_store::CONTRACT_VERSION;
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::register_plugin;

/// Stable plugin name — must match the `name` field in `plugin.toml`.
pub const PLUGIN_NAME: &str = "cairn-store-sqlite";

/// Embedded plugin manifest TOML.
pub const MANIFEST_TOML: &str = include_str!("../plugin.toml");

/// Contract version range this crate accepts.
///
/// `0.2.0` covers the issue-46 surface (`Principal`-bearing reads + sealed
/// apply). Bump together with `cairn_core::contract::memory_store::CONTRACT_VERSION`.
pub const ACCEPTED_RANGE: VersionRange =
    VersionRange::new(ContractVersion::new(0, 2, 0), ContractVersion::new(0, 3, 0));

// Compile-time assertion: our declared range must contain the host's
// CONTRACT_VERSION so the registry version check passes on every build.
const _: () = assert!(
    ACCEPTED_RANGE.accepts(CONTRACT_VERSION),
    "host CONTRACT_VERSION outside this crate's declared range"
);

/// `SQLite`-backed `MemoryStore` and `MemoryStoreApply`.
///
/// Construction:
/// - **Normal path:** `SqliteMemoryStore::open(path).await` — opens or
///   creates the database, applies pending migrations.
/// - **Registry / capability-probe path:** `Default::default()` — opens an
///   in-memory (`:memory:`) database; no persistent state, no I/O side
///   effects.
pub struct SqliteMemoryStore {
    pub(crate) conn: conn::SharedConn,
}

impl SqliteMemoryStore {
    /// Async constructor. Opens (or creates) the database at `path`, applies
    /// WAL/FK pragmas, and runs all pending migrations.
    ///
    /// Dispatches the blocking open to `tokio::task::spawn_blocking`.
    ///
    /// # Errors
    ///
    /// Returns [`error::SqliteStoreError`] if the file cannot be opened or
    /// any migration fails.
    pub async fn open(path: &std::path::Path) -> Result<Self, error::SqliteStoreError> {
        let path = path.to_owned();
        let shared = tokio::task::spawn_blocking(move || conn::open_blocking(&path))
            .await
            .map_err(|e| error::SqliteStoreError::Io(std::io::Error::other(e.to_string())))??;
        Ok(Self { conn: shared })
    }
}

/// `Default` impl for use by `register_plugin!`. Opens an in-memory
/// database so capability probes work without touching the filesystem.
impl Default for SqliteMemoryStore {
    fn default() -> Self {
        // in-memory open must succeed; any failure here is a build-time bug.
        #[allow(clippy::expect_used)]
        let conn = conn::open_blocking(std::path::Path::new(":memory:"))
            .expect("invariant: in-memory SQLite open must not fail for registry probes");
        Self { conn }
    }
}

register_plugin!(
    MemoryStore,
    SqliteMemoryStore,
    "cairn-store-sqlite",
    MANIFEST_TOML
);
