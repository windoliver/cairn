//! `SQLite` record store for Cairn.
//!
//! This crate is mid-migration for issue #46. The full `MemoryStore` read
//! impl (`get` / `list` / `version_history`) and the sealed `MemoryStoreApply`
//! write impl land in Task 3 of that issue. Until then the `MemoryStore`
//! trait impl returns placeholder errors so `register_plugin!` compiles and
//! the plugin manifest appears in the registry.
//!
//! See TODO markers tagged `#46 Task 3` for the sites to restore.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use cairn_core::contract::memory_store::{
    CONTRACT_VERSION, HistoryEntry, ListQuery, ListResult, MemoryStore, MemoryStoreCapabilities,
    StoreError, TargetId,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::{Principal, record::MemoryRecord};
use cairn_core::register_plugin;

/// Stable plugin name. Matches `name = ...` in `plugin.toml`.
pub const PLUGIN_NAME: &str = "cairn-store-sqlite";

/// Plugin capability manifest TOML (parsed at registration time).
pub const MANIFEST_TOML: &str = include_str!("../plugin.toml");

/// Contract-version range this crate accepts. Shared by the compile-time
/// guard so the manifest range and the trait surface derive from one binding.
pub const ACCEPTED_RANGE: VersionRange =
    VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));

/// Static capability advertisement. All flags `false` until Task 3 wires
/// up the real `rusqlite` backing store.
static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
    fts: false,
    vector: false,
    graph_edges: false,
    transactions: false,
};

// Compile-time assertion: our declared range must contain the host's
// CONTRACT_VERSION so the registry version check passes.
const _: () = assert!(
    ACCEPTED_RANGE.accepts(CONTRACT_VERSION),
    "host CONTRACT_VERSION outside this crate's declared range",
);

/// `SQLite`-backed `MemoryStore`. Fields and full impl arrive in #46 Task 3.
#[derive(Default)]
pub struct SqliteMemoryStore;

// TODO #46 Task 3: replace stub method bodies with real rusqlite impl.
#[async_trait::async_trait]
impl MemoryStore for SqliteMemoryStore {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn capabilities(&self) -> &MemoryStoreCapabilities {
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        ACCEPTED_RANGE
    }

    async fn get(
        &self,
        _principal: &Principal,
        _target_id: &TargetId,
    ) -> Result<Option<MemoryRecord>, StoreError> {
        Err(StoreError::Invariant("not implemented until Task 3 (#46)"))
    }

    async fn list(&self, _query: &ListQuery) -> Result<ListResult, StoreError> {
        Err(StoreError::Invariant("not implemented until Task 3 (#46)"))
    }

    async fn version_history(
        &self,
        _principal: &Principal,
        _target_id: &TargetId,
    ) -> Result<Vec<HistoryEntry>, StoreError> {
        Err(StoreError::Invariant("not implemented until Task 3 (#46)"))
    }
}

register_plugin!(
    MemoryStore,
    SqliteMemoryStore,
    "cairn-store-sqlite",
    MANIFEST_TOML
);
