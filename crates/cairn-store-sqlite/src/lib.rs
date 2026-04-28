//! `SQLite` record store for Cairn.
//!
//! Async-fronted via `tokio_rusqlite`. Every `MemoryStore` trait method is
//! one `conn.call(|c| { … })` round-trip on a dedicated DB thread. Records
//! persist as a `record_json` blob plus denormalized hot columns; the WAL
//! state machine (#8) lives at the verb layer.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod error;
pub mod migrations;
pub mod open;
pub mod store;
mod verify;

pub use error::StoreError;
pub use open::{open, open_in_memory, open_in_memory_sync, open_sync};
pub use store::SqliteMemoryStore;

use cairn_core::contract::memory_store::CONTRACT_VERSION;
use cairn_core::contract::version::{ContractVersion, VersionRange};
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
