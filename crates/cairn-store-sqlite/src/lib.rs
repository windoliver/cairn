//! `SQLite` record store for Cairn (P0 scaffold).
//!
//! Schema, migrations, FTS5 and sqlite-vec integration arrive in
//! follow-up issues (#46 and later). For now this crate ships only the
//! plugin manifest, a stub `MemoryStore` impl with all capability flags
//! `false`, and a `register()` entry point so the host can include it
//! in `cairn plugins list/verify`.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use cairn_core::contract::memory_store::{CONTRACT_VERSION, MemoryStore, MemoryStoreCapabilities};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::register_plugin;

/// Stable plugin name. Matches `name = ...` in `plugin.toml`.
pub const PLUGIN_NAME: &str = "cairn-store-sqlite";

/// Plugin capability manifest TOML (parsed at registration time).
pub const MANIFEST_TOML: &str = include_str!("../plugin.toml");

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
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
    }
}

// Compile-time guard: this crate's accepted range must include the host
// CONTRACT_VERSION. If we ever bump CONTRACT_VERSION without bumping the
// range, the const evaluation here panics at build.
const _: () = {
    let lo = (0u16, 1u16, 0u16);
    let hi = (0u16, 2u16, 0u16);
    let v = (
        CONTRACT_VERSION.major,
        CONTRACT_VERSION.minor,
        CONTRACT_VERSION.patch,
    );
    assert!(
        (v.0 > lo.0 || (v.0 == lo.0 && (v.1 > lo.1 || (v.1 == lo.1 && v.2 >= lo.2))))
            && (v.0 < hi.0 || (v.0 == hi.0 && (v.1 < hi.1 || (v.1 == hi.1 && v.2 < hi.2)))),
        "host CONTRACT_VERSION outside this crate's declared range"
    );
};

register_plugin!(
    MemoryStore,
    SqliteMemoryStore,
    "cairn-store-sqlite",
    MANIFEST_TOML
);
