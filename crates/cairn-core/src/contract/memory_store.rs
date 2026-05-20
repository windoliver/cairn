//! `MemoryStore` contract (brief §4 row 1).
//!
//! P0 scaffold: surface only — `name`, `capabilities`,
//! `supported_contract_versions`. CRUD/FTS/ANN/graph methods land in #46.

use thiserror::Error;

use crate::contract::version::{ContractVersion, VersionRange};
use crate::domain::{
    projection::{ProjectionCursor, ProjectionLedgerRow, ProjectionSummary},
    record::RecordId,
};

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

/// Search mode requested by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SearchMode {
    /// `SQLite` FTS / lexical search.
    Keyword,
    /// Vector search.
    Semantic,
    /// Lexical + vector search.
    Hybrid,
}

/// Caller preference for the Nexus BM25S ranking signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Bm25sPreference {
    /// Use BM25S when available and current.
    Auto,
    /// Require BM25S or fail closed.
    Required,
    /// Do not call BM25S.
    Disabled,
}

/// Search request passed through the memory store contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRequest {
    /// Free-text query.
    pub query: String,
    /// Requested search mode.
    pub mode: SearchMode,
    /// Maximum hits to return.
    pub limit: u32,
    /// BM25S ranking preference.
    pub bm25s: Bm25sPreference,
}

/// Ranking signal name included in search results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RankingSignalName {
    /// `SQLite` FTS5 lexical score.
    SqliteFts5,
    /// `SQLite` vector score.
    SqliteVec,
    /// Nexus BM25S lexical score.
    NexusBm25s,
}

/// Per-hit ranking signal detail.
#[derive(Debug, Clone, PartialEq)]
pub struct RankingSignal {
    /// Signal name.
    pub name: RankingSignalName,
    /// Whether this signal contributed to the final score.
    pub used: bool,
    /// Optional numeric score from the signal.
    pub score: Option<f64>,
    /// Reason when the signal was skipped.
    pub reason: Option<String>,
}

/// One search hit.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    /// Authoritative record id.
    pub record_id: RecordId,
    /// Hash of the authoritative record used to validate derived rankings.
    pub record_hash: String,
    /// Final normalized score.
    pub score: f64,
    /// Optional snippet.
    pub snippet: Option<String>,
    /// Ranking signals used or skipped.
    pub ranking_signals: Vec<RankingSignal>,
}

/// Search response from a store implementation.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchResponse {
    /// Search hits.
    pub hits: Vec<SearchHit>,
}

/// Projection item produced by a sidecar rebuild.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionApplyItem {
    /// Ledger row to persist.
    pub row: ProjectionLedgerRow,
}

/// Authoritative record material needed for a projection rebuild.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionRecord {
    /// Current projection cursor for this record/source.
    pub cursor: ProjectionCursor,
    /// Authoritative record body used for lexical projection.
    pub body: String,
    /// Optional vault-relative source path for parser projections.
    pub source_path: Option<String>,
    /// Optional source hash for parser projections.
    pub source_hash: Option<String>,
}

/// Errors from the memory store contract.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MemoryStoreError {
    /// Store does not support the requested capability.
    #[error("capability unavailable: {0}")]
    CapabilityUnavailable(String),
    /// Store I/O or query failed.
    #[error("store error: {0}")]
    Store(String),
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

    /// Search authoritative records.
    async fn search(&self, _request: SearchRequest) -> Result<SearchResponse, MemoryStoreError> {
        Err(MemoryStoreError::CapabilityUnavailable("search".to_owned()))
    }

    /// Return projection summaries derived from the authoritative ledger.
    async fn projection_summaries(&self) -> Result<Vec<ProjectionSummary>, MemoryStoreError> {
        Err(MemoryStoreError::CapabilityUnavailable(
            "projection_summaries".to_owned(),
        ))
    }

    /// Return authoritative cursors that should be sent to a projection target.
    async fn projection_cursors(&self) -> Result<Vec<ProjectionCursor>, MemoryStoreError> {
        Err(MemoryStoreError::CapabilityUnavailable(
            "projection_cursors".to_owned(),
        ))
    }

    /// Return authoritative records and source metadata for projection rebuilds.
    async fn projection_records(&self) -> Result<Vec<ProjectionRecord>, MemoryStoreError> {
        Err(MemoryStoreError::CapabilityUnavailable(
            "projection_records".to_owned(),
        ))
    }

    /// Return failed projection ledger rows for diagnostics.
    async fn projection_failures(&self) -> Result<Vec<ProjectionLedgerRow>, MemoryStoreError> {
        Err(MemoryStoreError::CapabilityUnavailable(
            "projection_failures".to_owned(),
        ))
    }

    /// Persist sidecar projection results into the authoritative projection ledger.
    async fn apply_projection_items(
        &self,
        _items: Vec<ProjectionApplyItem>,
    ) -> Result<(), MemoryStoreError> {
        Err(MemoryStoreError::CapabilityUnavailable(
            "apply_projection_items".to_owned(),
        ))
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

        async fn search(
            &self,
            _request: SearchRequest,
        ) -> Result<SearchResponse, MemoryStoreError> {
            Ok(SearchResponse {
                hits: vec![SearchHit {
                    record_id: RecordId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV")
                        .expect("valid test ULID"),
                    record_hash: "sha256:record-a".to_owned(),
                    score: 1.0,
                    snippet: Some("projection".to_owned()),
                    ranking_signals: vec![RankingSignal {
                        name: RankingSignalName::SqliteFts5,
                        used: true,
                        score: Some(1.0),
                        reason: None,
                    }],
                }],
            })
        }

        async fn projection_summaries(&self) -> Result<Vec<ProjectionSummary>, MemoryStoreError> {
            Ok(vec![])
        }

        async fn projection_cursors(&self) -> Result<Vec<ProjectionCursor>, MemoryStoreError> {
            Ok(vec![])
        }

        async fn projection_records(&self) -> Result<Vec<ProjectionRecord>, MemoryStoreError> {
            Ok(vec![])
        }

        async fn projection_failures(&self) -> Result<Vec<ProjectionLedgerRow>, MemoryStoreError> {
            Ok(vec![])
        }

        async fn apply_projection_items(
            &self,
            _items: Vec<ProjectionApplyItem>,
        ) -> Result<(), MemoryStoreError> {
            Ok(())
        }
    }

    #[test]
    fn dyn_compatible() {
        let s: Box<dyn MemoryStore> = Box::new(StubStore);
        assert_eq!(s.name(), "stub");
        assert!(s.capabilities().fts);
        assert!(s.supported_contract_versions().accepts(CONTRACT_VERSION));
    }

    #[tokio::test]
    async fn memory_store_search_contract_returns_ranking_signals() {
        let store = StubStore;
        let response = store
            .search(SearchRequest {
                query: "projection".to_owned(),
                mode: SearchMode::Keyword,
                limit: 10,
                bm25s: Bm25sPreference::Auto,
            })
            .await
            .expect("stub search");

        assert_eq!(response.hits.len(), 1);
        assert_eq!(
            response.hits[0].ranking_signals[0].name,
            RankingSignalName::SqliteFts5
        );
    }

    #[tokio::test]
    async fn memory_store_projection_summary_contract_is_available() {
        let store = StubStore;
        let summaries = store
            .projection_summaries()
            .await
            .expect("projection summaries");

        assert!(summaries.is_empty());
    }
}
