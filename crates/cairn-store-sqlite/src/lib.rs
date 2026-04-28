//! `SQLite` record store for Cairn.
//!
//! This crate is mid-migration for issue #46. The full `MemoryStore` read
//! impl (get / list / version_history) and the sealed `MemoryStoreApply`
//! write impl land in Task 3 of that issue. Until then the `MemoryStore`
//! trait impl and `register_plugin!` call are commented out so the
//! workspace compiles while cairn-core's trait surface is extended.
//!
//! See TODO markers tagged `#46 Task 3` for the sites to restore.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use cairn_core::contract::version::{ContractVersion, VersionRange};

/// Stable plugin name. Matches `name = ...` in `plugin.toml`.
pub const PLUGIN_NAME: &str = "cairn-store-sqlite";

/// Plugin capability manifest TOML (parsed at registration time).
pub const MANIFEST_TOML: &str = include_str!("../plugin.toml");

/// Contract-version range this crate accepts. Shared by the compile-time
/// guard so the manifest range and the trait surface derive from one binding.
pub const ACCEPTED_RANGE: VersionRange =
    VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));

/// SQLite-backed `MemoryStore`. Fields and full impl arrive in #46 Task 3.
#[derive(Default)]
pub struct SqliteMemoryStore;

/// Stub registration entry point. Registers nothing until Task 3 restores
/// the full `MemoryStore` impl.
///
/// TODO #46 Task 3: replace this stub with the real registration via
/// `register_plugin!` once the impl block below is restored.
pub fn register(
    _reg: &mut cairn_core::contract::registry::PluginRegistry,
) -> Result<(), cairn_core::contract::registry::PluginError> {
    // TODO #46 Task 3: uncomment register_plugin! call below and remove this stub.
    Ok(())
}

// TODO #46 Task 3: restore the MemoryStore impl and register_plugin! call.
// The three async read methods (get, list, version_history) required by the
// updated MemoryStore trait land in Task 3 along with the rusqlite backing
// store. Commented out here so the workspace compiles while cairn-core's
// trait surface is extended in Task 1.
//
// use cairn_core::contract::memory_store::{
//     CONTRACT_VERSION, HistoryEntry, ListQuery, ListResult, MemoryRecord,
//     MemoryStore, MemoryStoreCapabilities, StoreError, TargetId,
// };
// use cairn_core::domain::Principal;
// use cairn_core::register_plugin;
//
// #[async_trait::async_trait]
// impl MemoryStore for SqliteMemoryStore {
//     fn name(&self) -> &str { PLUGIN_NAME }
//     fn capabilities(&self) -> &MemoryStoreCapabilities { &CAPS }
//     fn supported_contract_versions(&self) -> VersionRange { ACCEPTED_RANGE }
//     async fn get(&self, _: &Principal, _: &TargetId)
//         -> Result<Option<MemoryRecord>, StoreError> { todo!() }
//     async fn list(&self, _: &ListQuery) -> Result<ListResult, StoreError> { todo!() }
//     async fn version_history(&self, _: &Principal, _: &TargetId)
//         -> Result<Vec<HistoryEntry>, StoreError> { todo!() }
// }
//
// const _: () = assert!(
//     ACCEPTED_RANGE.accepts(CONTRACT_VERSION),
//     "host CONTRACT_VERSION outside this crate's declared range"
// );
//
// register_plugin!(
//     MemoryStore,
//     SqliteMemoryStore,
//     "cairn-store-sqlite",
//     MANIFEST_TOML
// );
