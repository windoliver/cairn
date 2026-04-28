//! `SQLite` record store for Cairn.
//!
//! Ships the embedded migration set (records + FTS5, WAL ops, replay
//! ledger, locks, consent journal). Verb-level methods on the
//! `MemoryStore` impl arrive in follow-up issues; this crate currently
//! exposes [`open()`], [`open_in_memory()`], and the plugin manifest.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod error;
pub mod migrations;
mod open;
mod verify;

pub use error::StoreError;
pub use open::{open, open_in_memory};

use cairn_core::contract::memory_store::StoreError as TraitStoreError;
use cairn_core::contract::memory_store::{
    CONTRACT_VERSION, Edge, EdgeDir, EdgeKey, KeywordSearchArgs, KeywordSearchPage, ListArgs,
    ListPage, MemoryStore, MemoryStoreCapabilities, RecordVersion, TombstoneReason, UpsertOutcome,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::{MemoryRecord, RecordId, TargetId};
use cairn_core::register_plugin;

/// Stable plugin name. Matches `name = ...` in `plugin.toml`.
pub const PLUGIN_NAME: &str = "cairn-store-sqlite";

/// Plugin capability manifest TOML (parsed at registration time).
pub const MANIFEST_TOML: &str = include_str!("../plugin.toml");

/// Contract-version range this crate accepts. Shared by the trait impl and
/// the compile-time guard below so the manifest range and the trait surface
/// derive from one binding.
pub const ACCEPTED_RANGE: VersionRange =
    VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 3, 0));

/// P0 stub `MemoryStore`. All capability flags are `false`; verb methods
/// land with the storage implementation in #46.
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

    async fn upsert(&self, _r: &MemoryRecord) -> Result<UpsertOutcome, TraitStoreError> {
        Err("cairn-store-sqlite: upsert not yet implemented (#46)".into())
    }
    async fn get(&self, _id: &RecordId) -> Result<Option<MemoryRecord>, TraitStoreError> {
        Err("cairn-store-sqlite: get not yet implemented (#46)".into())
    }
    async fn list(&self, _args: &ListArgs) -> Result<ListPage, TraitStoreError> {
        Err("cairn-store-sqlite: list not yet implemented (#46)".into())
    }
    async fn tombstone(&self, _id: &RecordId, _r: TombstoneReason) -> Result<(), TraitStoreError> {
        Err("cairn-store-sqlite: tombstone not yet implemented (#46)".into())
    }
    async fn versions(&self, _t: &TargetId) -> Result<Vec<RecordVersion>, TraitStoreError> {
        Err("cairn-store-sqlite: versions not yet implemented (#46)".into())
    }
    async fn put_edge(&self, _e: &Edge) -> Result<(), TraitStoreError> {
        Err("cairn-store-sqlite: put_edge not yet implemented (#46)".into())
    }
    async fn remove_edge(&self, _k: &EdgeKey) -> Result<bool, TraitStoreError> {
        Err("cairn-store-sqlite: remove_edge not yet implemented (#46)".into())
    }
    async fn neighbours(&self, _id: &RecordId, _d: EdgeDir) -> Result<Vec<Edge>, TraitStoreError> {
        Err("cairn-store-sqlite: neighbours not yet implemented (#46)".into())
    }
    async fn search_keyword(
        &self,
        _args: &KeywordSearchArgs<'_>,
    ) -> Result<KeywordSearchPage, TraitStoreError> {
        Err("cairn-store-sqlite: search_keyword not yet implemented (#47)".into())
    }
}

// Compile-time guard: this crate's accepted range must include the host
// CONTRACT_VERSION. If we ever bump CONTRACT_VERSION without bumping the
// range, the const evaluation here panics at build.
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
