//! `MemoryStore` contract (brief §4 row 1).
//!
//! P0 scaffold extended in #46: adds Principal-bearing read methods
//! (`get`, `list`, `version_history`) and a sealed apply (write) surface
//! gated by an `ApplyToken` constructible only from `cairn_core::wal`.

pub mod apply;
pub mod error;
pub mod types;

pub use apply::*;
pub use error::*;
pub use types::*;

use crate::contract::version::{ContractVersion, VersionRange};

/// Contract version for `MemoryStore`. Bumps when the trait surface changes.
///
/// `0.2.0` covers the Principal-bearing read surface (`get`, `list`,
/// `version_history`) plus the sealed `MemoryStoreApply` write surface
/// added by issue #46. Implementations advertising `0.1.0` are not
/// surface-compatible and must republish.
pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(0, 2, 0);

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
/// Brief §4 row 1: P0 default is pure `SQLite` + FTS5. Read methods are
/// gated by a `Principal` for per-row rebac evaluation (brief lines
/// 2557/3287/4136). Write methods live on the sealed [`MemoryStoreApply`]
/// surface and require an [`ApplyToken`] issued by `cairn_core::wal`.
#[async_trait::async_trait]
pub trait MemoryStore: Send + Sync {
    /// Stable identifier of the registered plugin instance.
    fn name(&self) -> &str;

    /// Static capability advertisement (brief §4.1).
    fn capabilities(&self) -> &MemoryStoreCapabilities;

    /// Range of `MemoryStore::CONTRACT_VERSION` values this impl accepts.
    fn supported_contract_versions(&self) -> VersionRange;

    /// Read the active version of a logical record by stable `target_id`,
    /// gated by rebac against `principal`.
    ///
    /// Returns `None` when:
    /// - No active, non-tombstoned, non-expired version exists, OR
    /// - The principal cannot read the active version.
    ///
    /// The three cases are indistinguishable to the caller (brief line 4136:
    /// "hidden rows never surface").
    async fn get(
        &self,
        principal: &crate::domain::principal::Principal,
        target_id: &TargetId,
    ) -> Result<Option<crate::domain::record::MemoryRecord>, StoreError>;

    /// Range/list query gated by rebac against `query.principal`.
    ///
    /// The query carries pre-resolved scope filters; the store evaluates
    /// each candidate row's scope + `actor_chain` against the principal and
    /// drops non-readable rows before returning (brief line 3287).
    /// `ListResult::hidden` reports the count of dropped rows (brief line
    /// 4136).
    async fn list(&self, query: &ListQuery) -> Result<ListResult, StoreError>;

    /// Full lifecycle history for a logical `target_id`, gated by rebac.
    ///
    /// Returns all `Version` entries the principal can read (ascending by
    /// `version`), then any `Purge` markers from `record_purges` (ascending
    /// by `purged_at`). The WAL executor passes a system principal
    /// (constructed via [`Principal::system`](crate::domain::principal::Principal::system)
    /// with an `ApplyToken`) that bypasses scope filtering.
    async fn version_history(
        &self,
        principal: &crate::domain::principal::Principal,
        target_id: &TargetId,
    ) -> Result<Vec<HistoryEntry>, StoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::memory_store::apply::{
        ApplyToken, MemoryStoreApply, MemoryStoreApplyTx, private::Sealed,
    };
    use crate::contract::memory_store::error::StoreError;
    use crate::contract::memory_store::types::{HistoryEntry, ListQuery, ListResult, TargetId};
    use crate::contract::version::{ContractVersion, VersionRange};
    use crate::domain::{principal::Principal, record::MemoryRecord};

    struct StubStore;

    impl Sealed for StubStore {}

    #[async_trait::async_trait]
    impl MemoryStore for StubStore {
        fn name(&self) -> &'static str {
            "stub"
        }

        fn capabilities(&self) -> &MemoryStoreCapabilities {
            static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
                fts: true,
                vector: false,
                graph_edges: true,
                transactions: true,
            };
            &CAPS
        }

        fn supported_contract_versions(&self) -> VersionRange {
            VersionRange::new(ContractVersion::new(0, 2, 0), ContractVersion::new(0, 3, 0))
        }

        async fn get(
            &self,
            _principal: &Principal,
            _target_id: &TargetId,
        ) -> Result<Option<MemoryRecord>, StoreError> {
            Ok(None)
        }

        async fn list(&self, _query: &ListQuery) -> Result<ListResult, StoreError> {
            Ok(ListResult {
                rows: vec![],
                hidden: 0,
            })
        }

        async fn version_history(
            &self,
            _principal: &Principal,
            _target_id: &TargetId,
        ) -> Result<Vec<HistoryEntry>, StoreError> {
            Ok(vec![])
        }
    }

    #[async_trait::async_trait]
    impl MemoryStoreApply for StubStore {
        async fn with_apply_tx<F, T>(&self, _token: ApplyToken, _f: F) -> Result<T, StoreError>
        where
            F: FnOnce(&mut dyn MemoryStoreApplyTx) -> Result<T, StoreError> + Send + 'static,
            T: Send + 'static,
        {
            Err(StoreError::Invariant("stub: not implemented"))
        }
    }

    #[test]
    fn dyn_compatible_read() {
        let s: Box<dyn MemoryStore> = Box::new(StubStore);
        assert_eq!(s.name(), "stub");
        assert!(s.capabilities().fts);
        assert!(s.supported_contract_versions().accepts(CONTRACT_VERSION));
    }

    #[tokio::test]
    async fn apply_returns_stub_error() {
        // MemoryStoreApply has a generic method, so it cannot be used as
        // `dyn MemoryStoreApply`. Verify the concrete impl via its direct type.
        let s = StubStore;
        let token = crate::wal::test_apply_token();
        let result = s.with_apply_tx(token, |_tx| Ok::<(), StoreError>(())).await;
        assert!(matches!(result, Err(StoreError::Invariant(_))));
    }
}
