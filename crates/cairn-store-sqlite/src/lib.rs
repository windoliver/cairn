//! `SQLite` record store for Cairn.
//!
//! Ships the embedded migration set (records + FTS5, WAL ops, replay
//! ledger, locks, consent journal). Verb-level method bodies on the
//! `MemoryStore` impl land in follow-up issues; this crate currently
//! exposes [`open()`], [`open_in_memory()`], the plugin manifest, and a
//! P0 stub impl whose CRUD methods return
//! [`cairn_core::contract::memory_store::StoreError::Unimplemented`].

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod error;
pub mod migrations;
mod open;
mod verify;

pub use error::StoreError;
pub use open::{open, open_in_memory};

use cairn_core::contract::memory_store::{
    self as ms_contract, CONTRACT_VERSION, MemoryStore, MemoryStoreCapabilities,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::record::MemoryRecord;
use cairn_core::register_plugin;

/// Stable plugin name. Matches `name = ...` in `plugin.toml`.
pub const PLUGIN_NAME: &str = "cairn-store-sqlite";

/// Plugin capability manifest TOML (parsed at registration time).
pub const MANIFEST_TOML: &str = include_str!("../plugin.toml");

/// Contract-version range this crate accepts (`[0.2.0, 0.3.0)`). Shared by
/// the trait impl and the compile-time guard below so the manifest range
/// and the trait surface derive from one binding.
pub const ACCEPTED_RANGE: VersionRange =
    VersionRange::new(ContractVersion::new(0, 2, 0), ContractVersion::new(0, 3, 0));

/// SQLite-backed [`MemoryStore`] implementation.
///
/// **P0 stub:** `get`, `upsert`, and `list_active` return
/// [`ms_contract::StoreError::Unimplemented`]. Callers must treat those
/// errors as `CapabilityUnavailable` until the full schema lands in #46.
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

    async fn get(
        &self,
        _target_id: &str,
    ) -> Result<Option<ms_contract::StoredRecord>, ms_contract::StoreError> {
        Err(ms_contract::StoreError::Unimplemented)
    }

    async fn upsert(
        &self,
        _record: MemoryRecord,
    ) -> Result<ms_contract::StoredRecord, ms_contract::StoreError> {
        Err(ms_contract::StoreError::Unimplemented)
    }

    async fn list_active(&self) -> Result<Vec<ms_contract::StoredRecord>, ms_contract::StoreError> {
        Err(ms_contract::StoreError::Unimplemented)
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
