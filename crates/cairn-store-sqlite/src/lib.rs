//! `SQLite` record store for Cairn (P0 scaffold).
//!
//! Schema, migrations, FTS5 and sqlite-vec integration arrive in #46.
//! This crate ships only the plugin manifest, stub `MemoryStore` impl with
//! all capability flags `false`, and a `register()` entry point.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use cairn_core::contract::memory_store::{
    CONTRACT_VERSION, MemoryStore, MemoryStoreCapabilities, StoreError, StoredRecord,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::record::MemoryRecord;
use cairn_core::register_plugin;

/// Canonical plugin name used in manifest and registry lookups.
pub const PLUGIN_NAME: &str = "cairn-store-sqlite";
/// Raw plugin manifest TOML, embedded at compile time from `plugin.toml`.
pub const MANIFEST_TOML: &str = include_str!("../plugin.toml");

/// Contract version range this crate acknowledges (`[0.2.0, 0.3.0)`).
///
/// **P0 stub:** `get`, `upsert`, and `list_active` all return
/// [`StoreError::Unimplemented`]. Callers must treat those errors as
/// `CapabilityUnavailable` until the full schema lands in #46.
pub const ACCEPTED_RANGE: VersionRange =
    VersionRange::new(ContractVersion::new(0, 2, 0), ContractVersion::new(0, 3, 0));

/// SQLite-backed [`MemoryStore`] implementation (P0 stub; full schema lands in #46).
#[derive(Default)]
pub struct SqliteMemoryStore;

#[async_trait::async_trait]
impl MemoryStore for SqliteMemoryStore {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn capabilities(&self) -> &MemoryStoreCapabilities {
        static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
            fts: false,
            vector: false,
            graph_edges: false,
            transactions: false,
        };
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        ACCEPTED_RANGE
    }

    async fn get(&self, _target_id: &str) -> Result<Option<StoredRecord>, StoreError> {
        Err(StoreError::Unimplemented)
    }

    async fn upsert(&self, _record: MemoryRecord) -> Result<StoredRecord, StoreError> {
        Err(StoreError::Unimplemented)
    }

    async fn list_active(&self) -> Result<Vec<StoredRecord>, StoreError> {
        Err(StoreError::Unimplemented)
    }
}

const _: () = assert!(
    ACCEPTED_RANGE.accepts(CONTRACT_VERSION),
    "host CONTRACT_VERSION outside this crate's declared range"
);

register_plugin!(
    MemoryStore,
    SqliteMemoryStore,
    "cairn-store-sqlite",
    MANIFEST_TOML
);
