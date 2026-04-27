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

/// Static identity descriptor for a [`MemoryStore`] plugin (§4.1).
///
/// This companion trait carries the two associated consts that the
/// `register_plugin_with!` macro checks **before construction** — the
/// stable plugin name and the supported contract-version range.
///
/// Separating these consts from [`MemoryStore`] is required by stable Rust:
/// associated consts in a trait break `dyn` compatibility unless gated by
/// `where Self: Sized` (an unstable feature as of 1.95). Placing them in a
/// `Sized`-bounded companion trait keeps `dyn MemoryStore` valid while still
/// allowing the macro to enforce `<Impl as MemoryStorePlugin>::NAME ==
/// registered_name` at compile time.
///
/// Every concrete [`MemoryStore`] implementation should also implement
/// `MemoryStorePlugin`. The blanket-compatible methods `fn name` and
/// `fn supported_contract_versions` on [`MemoryStore`] should delegate to
/// these consts (e.g. `fn name(&self) -> &str { Self::NAME }`).
pub trait MemoryStorePlugin: MemoryStore + Sized {
    /// Stable plugin name, checked statically before construction (§4.1).
    ///
    /// Must match the `name` literal passed to `register_plugin!` /
    /// `register_plugin_with!`.
    const NAME: &'static str;

    /// Version range checked statically before construction (§4.1).
    const SUPPORTED_VERSIONS: VersionRange;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubStore;

    #[async_trait::async_trait]
    impl MemoryStore for StubStore {
        fn name(&self) -> &'static str {
            Self::NAME
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
            Self::SUPPORTED_VERSIONS
        }
    }

    impl MemoryStorePlugin for StubStore {
        const NAME: &'static str = "stub";
        const SUPPORTED_VERSIONS: VersionRange =
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
    }

    #[test]
    fn dyn_compatible() {
        let s: Box<dyn MemoryStore> = Box::new(StubStore);
        assert_eq!(s.name(), "stub");
        assert!(s.capabilities().fts);
        assert!(s.supported_contract_versions().accepts(CONTRACT_VERSION));
    }

    #[test]
    fn static_consts_accessible() {
        assert_eq!(StubStore::NAME, "stub");
        assert!(StubStore::SUPPORTED_VERSIONS.accepts(CONTRACT_VERSION));
    }
}
