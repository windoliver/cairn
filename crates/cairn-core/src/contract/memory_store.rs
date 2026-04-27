//! `MemoryStore` contract (brief §4 row 1).

use crate::contract::version::{ContractVersion, VersionRange};
use crate::domain::record::MemoryRecord;

/// Contract version for `MemoryStore`. Bumps when the trait surface changes.
pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(0, 2, 0);

/// Static capability declaration for a `MemoryStore` impl.
// Four capability flags mirror the four distinct store dimensions; a state
// machine would add indirection with no gain here.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemoryStoreCapabilities {
    /// Whether the store supports full-text search (FTS5 or equivalent).
    pub fts: bool,
    /// Whether the store supports vector similarity search.
    pub vector: bool,
    /// Whether the store supports graph-edge traversal queries.
    pub graph_edges: bool,
    /// Whether the store supports multi-statement transactions.
    pub transactions: bool,
}

/// A `MemoryRecord` at a specific store version.
///
/// `version` is the monotonic per-`target_id` counter from the DB COW model
/// (brief §3.0). Projection and resync use it for optimistic concurrency
/// checks without touching the DB row directly.
#[derive(Debug, Clone)]
pub struct StoredRecord {
    /// The stored memory record.
    pub record: MemoryRecord,
    /// Monotonic version counter. `1` for a record's first write.
    pub version: u32,
}

/// Errors returned by `MemoryStore` methods.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// The store has not yet implemented the requested operation.
    #[error("store not yet implemented")]
    Unimplemented,
    /// An internal store error with a descriptive message.
    #[error("store internal error: {0}")]
    Internal(String),
}

/// Storage contract — typed CRUD over `MemoryRecord`.
///
/// Brief §4 row 1. Method bodies arrive in #46 (`SQLite` impl);
/// `FixtureStore` in `cairn-test-fixtures` serves tests.
#[async_trait::async_trait]
pub trait MemoryStore: Send + Sync {
    /// Returns the store's human-readable name (e.g., `"sqlite"`, `"fixture"`).
    fn name(&self) -> &str;
    /// Returns the static capability advertisement for this store instance.
    fn capabilities(&self) -> &MemoryStoreCapabilities;
    /// Returns the range of contract versions this store implementation accepts.
    fn supported_contract_versions(&self) -> VersionRange;

    /// Return the active `StoredRecord` for `target_id`, or `None` if absent.
    async fn get(&self, target_id: &str) -> Result<Option<StoredRecord>, StoreError>;

    /// Write a record. If a record with the same `id` already exists, bumps
    /// its version. Returns the stored version.
    async fn upsert(&self, record: MemoryRecord) -> Result<StoredRecord, StoreError>;

    /// Return all active (non-tombstoned) records.
    async fn list_active(&self) -> Result<Vec<StoredRecord>, StoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubStore;

    #[async_trait::async_trait]
    impl MemoryStore for StubStore {
        fn name(&self) -> &'static str {
            "stub"
        }
        fn capabilities(&self) -> &MemoryStoreCapabilities {
            static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
                fts: true,
                vector: false,
                graph_edges: false,
                transactions: true,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            VersionRange::new(ContractVersion::new(0, 2, 0), ContractVersion::new(0, 3, 0))
        }
        async fn get(&self, _: &str) -> Result<Option<StoredRecord>, StoreError> {
            Err(StoreError::Unimplemented)
        }
        async fn upsert(&self, _: MemoryRecord) -> Result<StoredRecord, StoreError> {
            Err(StoreError::Unimplemented)
        }
        async fn list_active(&self) -> Result<Vec<StoredRecord>, StoreError> {
            Err(StoreError::Unimplemented)
        }
    }

    #[tokio::test]
    async fn dyn_compatible() {
        let s: Box<dyn MemoryStore> = Box::new(StubStore);
        assert_eq!(s.name(), "stub");
        assert!(s.capabilities().fts);
        assert!(s.supported_contract_versions().accepts(CONTRACT_VERSION));
        assert!(s.get("x").await.is_err());
        assert!(s.list_active().await.is_err());
    }
}
