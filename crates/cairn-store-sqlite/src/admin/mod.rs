//! Adapter impls of `cairn-core::contract::snapshot_artifact` traits, plus
//! the in-process types they need. Powers `cairn-core::verbs::admin::{snapshot,restore}::run`.

mod applier;
mod metadata;
mod producer;
mod reader;
mod registry;

pub use applier::SqliteSnapshotApplier;
pub use metadata::SqliteSnapshotMetadata;
pub use producer::SqliteSnapshotProducer;
pub use reader::SqliteSnapshotReader;
pub use registry::FileBackupRegistry;
