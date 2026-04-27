//! `SQLite` record store for Cairn (P0 scaffold).
//!
//! Schema, migrations, FTS5 and sqlite-vec integration arrive in #46.
//! This crate ships only the plugin manifest, stub `MemoryStore` impl with
//! all capability flags `false`, and a `register()` entry point.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use cairn_core::contract::memory_store::{
    CONTRACT_VERSION, MemoryStore, MemoryStoreCapabilities, StoredRecord, StoreError,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::record::MemoryRecord;
use cairn_core::register_plugin;

pub const PLUGIN_NAME: &str = "cairn-store-sqlite";
pub const MANIFEST_TOML: &str = include_str!("../plugin.toml");

pub const ACCEPTED_RANGE: VersionRange =
    VersionRange::new(ContractVersion::new(0, 2, 0), ContractVersion::new(0, 3, 0));

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
