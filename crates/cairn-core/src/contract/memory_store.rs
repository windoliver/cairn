//! `MemoryStore` contract (brief §4 row 1).
//!
//! P0 scaffold: surface only - `name`, `capabilities`,
//! `supported_contract_versions`. CRUD/FTS/ANN/graph methods land in #46.

use crate::contract::version::{ContractVersion, VersionRange};
use crate::hot_memory::{HotMemoryInput, HotMemoryOutput, HotMemorySourceKind};

/// Contract version for `MemoryStore`. Bumps when the trait surface changes.
pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(0, 1, 2);

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

/// Request context for assembling hot memory.
#[derive(Debug, Clone, PartialEq)]
pub struct HotMemoryRequest {
    /// Session scope for the hot prefix.
    pub session_id: Option<String>,
    /// Agent scope when known.
    pub agent_id: Option<String>,
    /// Effective byte budget.
    pub budget_bytes: u32,
    /// Stable fingerprint of config values that affect hot memory.
    pub config_fingerprint: String,
    /// Centrality blend weight from config.
    pub god_node_weight: f32,
    /// Enabled source kinds for this request, in assembly order.
    pub source_kinds: Vec<HotMemorySourceKind>,
}

/// Scope of a cache invalidation request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotMemoryInvalidationScope {
    /// Delete every hot-memory cache row in the vault.
    Vault,
    /// Delete cache rows for a session.
    Session(String),
    /// Delete cache rows for an agent.
    Agent(String),
}

/// Errors from store-backed hot-memory reads and cache operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MemoryStoreError {
    /// The store cannot satisfy the request.
    #[error("memory store unavailable: {0}")]
    Unavailable(String),
    /// A backend query failed.
    #[error("memory store query failed: {message}")]
    Query {
        /// Human-readable backend query failure context.
        message: String,
        /// Optional backend source error.
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
    },
    /// A backend cache operation failed.
    #[error("memory store cache failed: {message}")]
    Cache {
        /// Human-readable backend cache failure context.
        message: String,
        /// Optional backend source error.
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
    },
}

impl MemoryStoreError {
    /// Build a query failure without a backend source error.
    pub fn query(message: impl Into<String>) -> Self {
        Self::Query {
            message: message.into(),
            source: None,
        }
    }

    /// Build a query failure preserving the backend source error.
    pub fn query_with_source(
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Query {
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }

    /// Build a cache failure without a backend source error.
    pub fn cache(message: impl Into<String>) -> Self {
        Self::Cache {
            message: message.into(),
            source: None,
        }
    }

    /// Build a cache failure preserving the backend source error.
    pub fn cache_with_source(
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Cache {
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }
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

    /// Fetch prepared hot-memory inputs for pure assembly.
    async fn hot_memory_input(
        &self,
        _request: &HotMemoryRequest,
    ) -> Result<HotMemoryInput, MemoryStoreError> {
        Err(MemoryStoreError::Unavailable(format!(
            "{} does not support hot memory",
            self.name()
        )))
    }

    /// Build the deterministic hot-memory cache key for this request and input.
    fn hot_memory_cache_key(
        &self,
        _request: &HotMemoryRequest,
        _input: &HotMemoryInput,
    ) -> Result<String, MemoryStoreError> {
        Err(MemoryStoreError::Unavailable(format!(
            "{} does not support hot memory",
            self.name()
        )))
    }

    /// Return a cached assembled prefix when available.
    async fn load_hot_memory_cache(
        &self,
        _key: &str,
    ) -> Result<Option<HotMemoryOutput>, MemoryStoreError> {
        Err(MemoryStoreError::Unavailable(format!(
            "{} does not support hot memory",
            self.name()
        )))
    }

    /// Store an assembled prefix in the hot cache.
    async fn store_hot_memory_cache(
        &self,
        _key: &str,
        _output: &HotMemoryOutput,
    ) -> Result<(), MemoryStoreError> {
        Err(MemoryStoreError::Unavailable(format!(
            "{} does not support hot memory",
            self.name()
        )))
    }

    /// Invalidate hot cache rows after relevant writes.
    async fn invalidate_hot_memory_cache(
        &self,
        _scope: HotMemoryInvalidationScope,
    ) -> Result<u64, MemoryStoreError> {
        Err(MemoryStoreError::Unavailable(format!(
            "{} does not support hot memory",
            self.name()
        )))
    }
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

        async fn hot_memory_input(
            &self,
            _request: &HotMemoryRequest,
        ) -> Result<HotMemoryInput, MemoryStoreError> {
            Ok(HotMemoryInput {
                sources: Vec::new(),
                source_revision: "stub-revision".to_owned(),
            })
        }

        fn hot_memory_cache_key(
            &self,
            request: &HotMemoryRequest,
            input: &HotMemoryInput,
        ) -> Result<String, MemoryStoreError> {
            Ok(format!(
                "{}:{}",
                request.config_fingerprint, input.source_revision
            ))
        }

        async fn load_hot_memory_cache(
            &self,
            _key: &str,
        ) -> Result<Option<HotMemoryOutput>, MemoryStoreError> {
            Ok(None)
        }

        async fn store_hot_memory_cache(
            &self,
            _key: &str,
            _output: &HotMemoryOutput,
        ) -> Result<(), MemoryStoreError> {
            Ok(())
        }

        async fn invalidate_hot_memory_cache(
            &self,
            _scope: HotMemoryInvalidationScope,
        ) -> Result<u64, MemoryStoreError> {
            Ok(0)
        }
    }

    #[test]
    fn dyn_compatible() {
        let s: Box<dyn MemoryStore> = Box::new(StubStore);
        assert_eq!(s.name(), "stub");
        assert!(s.capabilities().fts);
        assert!(s.supported_contract_versions().accepts(CONTRACT_VERSION));
    }

    #[test]
    fn contract_version_bumps_for_hot_memory_surface() {
        assert_eq!(CONTRACT_VERSION, ContractVersion::new(0, 1, 2));
        assert!(
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
                .accepts(CONTRACT_VERSION)
        );
    }

    #[tokio::test]
    async fn dyn_store_supports_hot_memory_methods() {
        let s: Box<dyn MemoryStore> = Box::new(StubStore);
        let request = HotMemoryRequest {
            session_id: Some("session-a".to_owned()),
            agent_id: Some("agent-a".to_owned()),
            budget_bytes: 1024,
            config_fingerprint: "config-a".to_owned(),
            god_node_weight: 0.3,
            source_kinds: crate::hot_memory::default_source_order(),
        };
        let input = s
            .hot_memory_input(&request)
            .await
            .expect("hot memory input");
        assert_eq!(input.source_revision, "stub-revision");
        let key = s.hot_memory_cache_key(&request, &input).expect("cache key");
        assert!(!key.is_empty());
        let cached = s.load_hot_memory_cache(&key).await.expect("load cache");
        assert!(cached.is_none());
        s.store_hot_memory_cache(
            &key,
            &HotMemoryOutput {
                prefix: String::new(),
                bytes: 0,
                sources: Vec::new(),
                truncation: Vec::new(),
                cache: crate::hot_memory::HotMemoryCacheInfo::miss(&key),
            },
        )
        .await
        .expect("store cache");
        let invalidated = s
            .invalidate_hot_memory_cache(HotMemoryInvalidationScope::Vault)
            .await
            .expect("invalidate cache");
        assert_eq!(invalidated, 0);
    }
}
