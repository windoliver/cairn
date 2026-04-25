//! `MemoryStore` contract (brief §4 row 1).
//!
//! P0 scaffold: surface only — `name`, `capabilities`,
//! `supported_contract_versions`. CRUD/FTS/ANN/graph methods land in #46.

use crate::contract::version::{ContractVersion, VersionRange};

/// Contract version for `MemoryStore`. Bumps when the trait surface changes.
pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(0, 1, 0);

/// Static capability declaration for a `MemoryStore` impl.
///
/// Cairn queries this before dispatching ANN-, FTS-, or graph-using verbs;
/// missing capability → `CapabilityUnavailable` (brief §4.1).
// Four capability flags mirror the four distinct store dimensions; a state
// machine would add indirection with no gain here.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemoryStoreCapabilities {
    /// Whether full-text search (FTS5) is supported.
    pub fts: bool,
    /// Whether vector/ANN search is supported.
    pub vector: bool,
    /// Whether graph edge storage and traversal is supported.
    pub graph_edges: bool,
    /// Whether ACID transactions are supported.
    pub transactions: bool,
}

/// Storage contract — typed CRUD + ANN + FTS + graph over `MemoryRecord`.
///
/// Brief §4 row 1: P0 default is pure `SQLite` + FTS5; P1 default is the
/// Nexus sandbox profile. Method bodies arrive in #46 once `MemoryRecord`
/// (sub-issue #37) lands.
#[async_trait::async_trait]
pub trait MemoryStore: Send + Sync {
    /// Stable identifier of the registered plugin instance.
    fn name(&self) -> &str;

    /// Static capability advertisement (brief §4.1).
    fn capabilities(&self) -> &MemoryStoreCapabilities;

    /// Range of `MemoryStore::CONTRACT_VERSION` values this impl accepts.
    fn supported_contract_versions(&self) -> VersionRange;
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
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
        }
    }

    #[test]
    fn dyn_compatible() {
        let s: Box<dyn MemoryStore> = Box::new(StubStore);
        assert_eq!(s.name(), "stub");
        assert!(s.capabilities().fts);
        assert!(s.supported_contract_versions().accepts(CONTRACT_VERSION));
    }
}
