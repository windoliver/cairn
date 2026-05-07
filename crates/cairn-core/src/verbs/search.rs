//! `search` verb dispatcher.
//!
//! Single entry point used by every surface (CLI, SDK, MCP). Performs
//! capability gating, dispatches to the matching `MemoryStore` leg
//! (`search_keyword` / `search_semantic` / `search_hybrid`), applies
//! token-budget trimming, and packages the result + optional explain
//! block into the response envelope.
//!
//! No I/O beyond the `store.*` calls — keeps `cairn-core`'s adapter-free
//! invariant (CLAUDE.md §3).

use crate::config::{CairnConfig, CapabilitySet};
use crate::contract::memory_store::{
    HybridSearchArgs, KeywordSearchArgs, MemoryStore, SearchCandidate, SemanticSearchArgs,
};
use crate::domain::ScopeTuple;
use crate::domain::taxonomy::MemoryVisibility;
use crate::search::{ScoreExplain, token_budget_trim};

/// Mode requested by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SearchMode {
    /// FTS5 keyword leg only.
    Keyword,
    /// ANN vector leg only.
    Semantic,
    /// RRF fusion + cosine re-rank.
    Hybrid,
}

/// Inputs to [`run`].
#[derive(Debug, Clone)]
pub struct SearchRequest {
    /// Free-text query.
    pub query: String,
    /// Selected mode.
    pub mode: SearchMode,
    /// Page size.
    pub limit: usize,
    /// Visibility allowlist; empty = no narrowing.
    pub visibility_allowlist: Vec<MemoryVisibility>,
    /// Authorization scope tuple. Threaded into every search SQL path
    /// (keyword, semantic, graph, graph-only hydration). Issue #191.
    /// Use [`ScopeTuple::default`] when no narrowing is required.
    pub auth_scope: ScopeTuple,
    /// Active embedding model label (for semantic + hybrid).
    pub model_label: String,
    /// `true` → request explain block from the store and surface it.
    /// Caller must have already verified the `policy_trace` capability.
    pub explain: bool,
}

/// Result of [`run`]: the trimmed candidate page plus optional explain.
#[derive(Debug, Clone)]
pub struct SearchOutcome {
    /// Trimmed candidate page.
    pub candidates: Vec<SearchCandidate>,
    /// Per-candidate score-component explanations, in lockstep with
    /// `candidates`. Populated iff `request.explain` was true and the
    /// `policy_trace` capability was advertised.
    pub explain: Option<Vec<ScoreExplain>>,
    /// Hybrid-only: legs that ran in degraded mode (capability missing,
    /// SQL error, deadline, etc.). Empty for keyword/semantic and for
    /// fully successful hybrid runs. Surface code (CLI, MCP, SDK) may
    /// thread this through to operators or callers; v1 verb dispatch
    /// surfaces it here so partial-result signaling is not silently
    /// dropped between the store and the wire surface. Issue #191.
    pub degraded_legs: Vec<crate::search::DegradedLeg>,
}

/// Errors raised by the dispatcher.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SearchError {
    /// Required capability is not advertised by `status` in this incarnation.
    #[error("capability unavailable: {capability}")]
    CapabilityUnavailable {
        /// The capability identifier (e.g. `cairn.mcp.v1.search.hybrid`).
        capability: &'static str,
    },
    /// Args failed validation before dispatch.
    #[error("invalid args: {reason}")]
    InvalidArgs {
        /// Human-readable reason.
        reason: String,
    },
    /// Store impl raised an error.
    ///
    /// The wrapped `StoreError` is `Box<dyn Error + Send + Sync>` (see
    /// `crate::contract::memory_store::StoreError`); surface code should
    /// map this variant to a generic envelope `Internal`/sysexit 70 until
    /// per-adapter typed errors land in #62. Future change: either add a
    /// `kind: &'static str` discriminator or replace `StoreError` with a
    /// typed enum.
    #[error(transparent)]
    Store(#[from] crate::contract::memory_store::StoreError),
}

const POLICY_TRACE_CAP: &str = "cairn.mcp.v1.policy_trace";

/// Fail-closed capability gate for `request.mode`.
fn gate_mode(mode: SearchMode, caps: &CapabilitySet) -> Result<(), SearchError> {
    let (ok, name) = match mode {
        SearchMode::Keyword => (caps.keyword_search, "cairn.mcp.v1.search.keyword"),
        SearchMode::Semantic => (caps.semantic_search, "cairn.mcp.v1.search.semantic"),
        SearchMode::Hybrid => (caps.hybrid_search, "cairn.mcp.v1.search.hybrid"),
    };
    if ok {
        Ok(())
    } else {
        Err(SearchError::CapabilityUnavailable { capability: name })
    }
}

/// Run the dispatcher.
///
/// Order of operations:
/// 1. Mode capability gate (fail closed).
/// 2. `--explain` capability gate (`policy_trace`).
/// 3. Build mode-specific `*SearchArgs` with `with_explain` set.
/// 4. Dispatch to `store.search_*`.
/// 5. Trim candidates + explain in lockstep using
///    `config.search.max_snippet_chars_per_page`.
///
/// # Errors
///
/// - [`SearchError::CapabilityUnavailable`] for missing mode or
///   `policy_trace` capability.
/// - [`SearchError::InvalidArgs`] when the query is empty.
/// - [`SearchError::Store`] propagated from the store impl.
pub async fn run(
    store: &dyn MemoryStore,
    config: &CairnConfig,
    caps: &CapabilitySet,
    request: SearchRequest,
) -> Result<SearchOutcome, SearchError> {
    if request.query.trim().is_empty() {
        return Err(SearchError::InvalidArgs {
            reason: "query is empty".to_owned(),
        });
    }
    gate_mode(request.mode, caps)?;
    if request.explain && !caps.policy_trace {
        return Err(SearchError::CapabilityUnavailable {
            capability: POLICY_TRACE_CAP,
        });
    }

    let visibility = if request.visibility_allowlist.is_empty() {
        vec![
            MemoryVisibility::Private,
            MemoryVisibility::Session,
            MemoryVisibility::Project,
            MemoryVisibility::Team,
            MemoryVisibility::Org,
            MemoryVisibility::Public,
        ]
    } else {
        request.visibility_allowlist.clone()
    };

    let (candidates, explain, degraded_legs) = match request.mode {
        SearchMode::Keyword => {
            let args = KeywordSearchArgs {
                query: request.query.clone(),
                filter: None,
                auth_scope: request.auth_scope.clone(),
                visibility_allowlist: visibility,
                limit: request.limit,
                cursor: None,
                with_explain: request.explain,
            };
            let page = store.search_keyword(&args).await?;
            (page.candidates, page.explain, Vec::new())
        }
        SearchMode::Semantic => {
            let args = SemanticSearchArgs {
                query: request.query.clone(),
                filter: None,
                auth_scope: request.auth_scope.clone(),
                visibility_allowlist: visibility,
                limit: request.limit,
                model_label: request.model_label.clone(),
                with_explain: request.explain,
            };
            let page = store.search_semantic(&args).await?;
            (page.candidates, page.explain, Vec::new())
        }
        SearchMode::Hybrid => {
            let args = HybridSearchArgs {
                query: request.query.clone(),
                filter: None,
                auth_scope: request.auth_scope.clone(),
                visibility_allowlist: visibility,
                limit: request.limit,
                model_label: request.model_label.clone(),
                blend: config.search.rerank_blend,
                rrf_k: config.search.rrf_k,
                rerank_topk: config.search.rerank_topk,
                with_explain: request.explain,
                confidence_floor: 1e-3,
                graph_confidence_min: config.search.graph_confidence_min,
            };
            let page = store.search_hybrid(&args).await?;
            (page.candidates, page.explain, page.degraded_legs)
        }
    };

    let (candidates, explain) = token_budget_trim(
        candidates,
        explain,
        config.search.max_snippet_chars_per_page,
    );

    Ok(SearchOutcome {
        candidates,
        explain,
        degraded_legs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CairnConfig;
    use crate::contract::memory_store::{
        Edge, EdgeDir, EdgeKey, HybridSearchPage, KeywordSearchPage, ListArgs, ListPage,
        MemoryStoreCapabilities, RecordVersion, SemanticSearchPage, StoreError, TombstoneReason,
        UpsertOutcome,
    };
    use crate::contract::version::{ContractVersion, VersionRange};
    use crate::domain::record::MemoryRecord;
    use crate::domain::{RecordId, TargetId};
    use std::sync::Mutex;

    /// Stub store that records which leg was called.
    ///
    /// `last_hybrid` captures the
    /// `(blend, rrf_k, rerank_topk, graph_confidence_min)` args from the
    /// most recent `search_hybrid` call so tests can assert config knobs
    /// flow through correctly.
    struct CallRecorder {
        calls: Mutex<Vec<&'static str>>,
        capabilities: MemoryStoreCapabilities,
        last_hybrid: Mutex<Option<(f32, usize, usize, f32)>>,
    }

    #[async_trait::async_trait]
    impl MemoryStore for CallRecorder {
        fn name(&self) -> &'static str {
            "recorder"
        }
        fn capabilities(&self) -> &MemoryStoreCapabilities {
            &self.capabilities
        }
        fn supported_contract_versions(&self) -> VersionRange {
            use super::super::super::contract::memory_store::CONTRACT_VERSION;
            // Accept exactly the current contract version and up to the next minor.
            VersionRange::new(
                CONTRACT_VERSION,
                ContractVersion::new(CONTRACT_VERSION.major, CONTRACT_VERSION.minor + 1, 0),
            )
        }
        async fn upsert(&self, _r: &MemoryRecord) -> Result<UpsertOutcome, StoreError> {
            unimplemented!()
        }
        async fn get(&self, _id: &RecordId) -> Result<Option<MemoryRecord>, StoreError> {
            Ok(None)
        }
        async fn list(&self, _args: &ListArgs) -> Result<ListPage, StoreError> {
            Ok(ListPage {
                records: vec![],
                next_cursor: None,
            })
        }
        async fn tombstone(
            &self,
            _id: &RecordId,
            _reason: TombstoneReason,
        ) -> Result<(), StoreError> {
            Ok(())
        }
        async fn versions(&self, _target: &TargetId) -> Result<Vec<RecordVersion>, StoreError> {
            Ok(vec![])
        }
        async fn put_edge(&self, _edge: &Edge) -> Result<(), StoreError> {
            Ok(())
        }
        async fn remove_edge(&self, _key: &EdgeKey) -> Result<bool, StoreError> {
            Ok(false)
        }
        async fn neighbours(&self, _id: &RecordId, _dir: EdgeDir) -> Result<Vec<Edge>, StoreError> {
            Ok(vec![])
        }
        async fn search_keyword(
            &self,
            _args: &KeywordSearchArgs<'_>,
        ) -> Result<KeywordSearchPage, StoreError> {
            self.calls.lock().expect("mutex").push("keyword");
            Ok(KeywordSearchPage {
                candidates: vec![],
                next_cursor: None,
                explain: None,
            })
        }
        async fn search_semantic(
            &self,
            _args: &SemanticSearchArgs<'_>,
        ) -> Result<SemanticSearchPage, StoreError> {
            self.calls.lock().expect("mutex").push("semantic");
            Ok(SemanticSearchPage {
                candidates: vec![],
                explain: None,
            })
        }
        async fn search_hybrid(
            &self,
            args: &HybridSearchArgs<'_>,
        ) -> Result<HybridSearchPage, StoreError> {
            self.calls.lock().expect("mutex").push("hybrid");
            *self.last_hybrid.lock().expect("mutex") = Some((
                args.blend,
                args.rrf_k,
                args.rerank_topk,
                args.graph_confidence_min,
            ));
            Ok(HybridSearchPage {
                candidates: vec![],
                explain: None,
                degraded_legs: vec![],
            })
        }
    }

    fn caps(keyword: bool, semantic: bool, hybrid: bool) -> CapabilitySet {
        CapabilitySet {
            keyword_search: keyword,
            semantic_search: semantic,
            hybrid_search: hybrid,
            llm_extract: false,
            agent_extract: false,
            graph_edges: false,
            policy_trace: true,
            replay_sequence: true,
            replay_challenge: true,
        }
    }

    fn req(mode: SearchMode) -> SearchRequest {
        SearchRequest {
            query: "hello".to_owned(),
            mode,
            limit: 10,
            visibility_allowlist: vec![],
            auth_scope: ScopeTuple::default(),
            model_label: "MiniLM-L6-v2".to_owned(),
            explain: false,
        }
    }

    #[tokio::test]
    async fn keyword_routes_to_keyword_leg() {
        let store = CallRecorder {
            calls: Mutex::new(vec![]),
            capabilities: MemoryStoreCapabilities {
                fts: true,
                vector: false,
                graph_edges: false,
                transactions: true,
                per_record_consent_model: true,
                graph_search: false,
            },
            last_hybrid: Mutex::new(None),
        };
        let config = CairnConfig::default();
        run(
            &store,
            &config,
            &caps(true, false, false),
            req(SearchMode::Keyword),
        )
        .await
        .expect("ok");
        assert_eq!(store.calls.lock().expect("mutex").as_slice(), &["keyword"]);
    }

    #[tokio::test]
    async fn semantic_rejected_when_capability_absent() {
        let store = CallRecorder {
            calls: Mutex::new(vec![]),
            capabilities: MemoryStoreCapabilities::default(),
            last_hybrid: Mutex::new(None),
        };
        let config = CairnConfig::default();
        let err = run(
            &store,
            &config,
            &caps(true, false, false),
            req(SearchMode::Semantic),
        )
        .await
        .expect_err("expected error");
        match err {
            SearchError::CapabilityUnavailable { capability } => {
                assert_eq!(capability, "cairn.mcp.v1.search.semantic");
            }
            other => panic!("wrong error: {other:?}"),
        }
        assert!(store.calls.lock().expect("mutex").is_empty());
    }

    #[tokio::test]
    async fn hybrid_routes_when_capability_set() {
        let store = CallRecorder {
            calls: Mutex::new(vec![]),
            capabilities: MemoryStoreCapabilities {
                fts: true,
                vector: true,
                graph_edges: false,
                transactions: true,
                per_record_consent_model: true,
                graph_search: false,
            },
            last_hybrid: Mutex::new(None),
        };
        let config = CairnConfig::default();
        run(
            &store,
            &config,
            &caps(true, true, true),
            req(SearchMode::Hybrid),
        )
        .await
        .expect("ok");
        assert_eq!(store.calls.lock().expect("mutex").as_slice(), &["hybrid"]);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "stub MemoryStore impl is unavoidably wide"
    )]
    #[tokio::test]
    async fn hybrid_propagates_degraded_legs_through_search_outcome() {
        use crate::search::{DegradationReason, DegradedLeg, GraphSource};

        struct DegradedHybridStore;

        #[async_trait::async_trait]
        impl MemoryStore for DegradedHybridStore {
            fn name(&self) -> &'static str {
                "degraded"
            }
            fn capabilities(&self) -> &MemoryStoreCapabilities {
                static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
                    fts: true,
                    vector: true,
                    graph_edges: false,
                    transactions: true,
                    per_record_consent_model: true,
                    graph_search: false,
                };
                &CAPS
            }
            fn supported_contract_versions(&self) -> crate::contract::version::VersionRange {
                use crate::contract::memory_store::CONTRACT_VERSION;
                use crate::contract::version::ContractVersion;
                crate::contract::version::VersionRange::new(
                    CONTRACT_VERSION,
                    ContractVersion::new(CONTRACT_VERSION.major, CONTRACT_VERSION.minor + 1, 0),
                )
            }
            async fn upsert(
                &self,
                _r: &crate::domain::record::MemoryRecord,
            ) -> Result<crate::contract::memory_store::UpsertOutcome, StoreError> {
                unimplemented!()
            }
            async fn get(
                &self,
                _id: &crate::domain::RecordId,
            ) -> Result<Option<crate::domain::record::MemoryRecord>, StoreError> {
                Ok(None)
            }
            async fn list(
                &self,
                _args: &crate::contract::memory_store::ListArgs,
            ) -> Result<crate::contract::memory_store::ListPage, StoreError> {
                unimplemented!()
            }
            async fn tombstone(
                &self,
                _id: &crate::domain::RecordId,
                _reason: crate::contract::memory_store::TombstoneReason,
            ) -> Result<(), StoreError> {
                unimplemented!()
            }
            async fn versions(
                &self,
                _t: &crate::domain::TargetId,
            ) -> Result<Vec<crate::contract::memory_store::RecordVersion>, StoreError> {
                Ok(vec![])
            }
            async fn put_edge(
                &self,
                _e: &crate::contract::memory_store::Edge,
            ) -> Result<(), StoreError> {
                unimplemented!()
            }
            async fn remove_edge(
                &self,
                _k: &crate::contract::memory_store::EdgeKey,
            ) -> Result<bool, StoreError> {
                Ok(false)
            }
            async fn neighbours(
                &self,
                _id: &crate::domain::RecordId,
                _d: crate::contract::memory_store::EdgeDir,
            ) -> Result<Vec<crate::contract::memory_store::Edge>, StoreError> {
                Ok(vec![])
            }
            async fn search_keyword(
                &self,
                _args: &KeywordSearchArgs<'_>,
            ) -> Result<KeywordSearchPage, StoreError> {
                Ok(KeywordSearchPage {
                    candidates: vec![],
                    next_cursor: None,
                    explain: None,
                })
            }
            async fn search_semantic(
                &self,
                _args: &SemanticSearchArgs<'_>,
            ) -> Result<SemanticSearchPage, StoreError> {
                Ok(SemanticSearchPage {
                    candidates: vec![],
                    explain: None,
                })
            }
            async fn search_hybrid(
                &self,
                _args: &HybridSearchArgs<'_>,
            ) -> Result<HybridSearchPage, StoreError> {
                Ok(HybridSearchPage {
                    candidates: vec![],
                    explain: None,
                    degraded_legs: vec![DegradedLeg::Graph {
                        reason: DegradationReason::CapabilityUnavailable,
                        source: GraphSource::All,
                    }],
                })
            }
        }

        let config = CairnConfig::default();
        let outcome = run(
            &DegradedHybridStore,
            &config,
            &caps(true, true, true),
            req(SearchMode::Hybrid),
        )
        .await
        .expect("hybrid run");
        assert_eq!(
            outcome.degraded_legs.len(),
            1,
            "hybrid must propagate store's degraded_legs through SearchOutcome"
        );
    }

    #[tokio::test]
    async fn keyword_outcome_has_empty_degraded_legs() {
        let store = CallRecorder {
            calls: Mutex::new(vec![]),
            capabilities: MemoryStoreCapabilities::default(),
            last_hybrid: Mutex::new(None),
        };
        let config = CairnConfig::default();
        let outcome = run(
            &store,
            &config,
            &caps(true, false, false),
            req(SearchMode::Keyword),
        )
        .await
        .expect("ok");
        assert!(
            outcome.degraded_legs.is_empty(),
            "keyword leg never reports degraded_legs"
        );
    }

    #[allow(
        clippy::too_many_lines,
        reason = "stub MemoryStore impl is unavoidably wide"
    )]
    #[tokio::test]
    async fn auth_scope_threaded_into_keyword_args() {
        use std::sync::Mutex as StdMutex;

        struct ScopeRecorder {
            captured: StdMutex<Option<ScopeTuple>>,
        }
        #[async_trait::async_trait]
        impl MemoryStore for ScopeRecorder {
            fn name(&self) -> &'static str {
                "scope-recorder"
            }
            fn capabilities(&self) -> &MemoryStoreCapabilities {
                static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
                    fts: true,
                    vector: false,
                    graph_edges: false,
                    transactions: true,
                    per_record_consent_model: true,
                    graph_search: false,
                };
                &CAPS
            }
            fn supported_contract_versions(&self) -> crate::contract::version::VersionRange {
                use crate::contract::memory_store::CONTRACT_VERSION;
                use crate::contract::version::ContractVersion;
                crate::contract::version::VersionRange::new(
                    CONTRACT_VERSION,
                    ContractVersion::new(CONTRACT_VERSION.major, CONTRACT_VERSION.minor + 1, 0),
                )
            }
            async fn upsert(
                &self,
                _r: &crate::domain::record::MemoryRecord,
            ) -> Result<crate::contract::memory_store::UpsertOutcome, StoreError> {
                unimplemented!()
            }
            async fn get(
                &self,
                _id: &crate::domain::RecordId,
            ) -> Result<Option<crate::domain::record::MemoryRecord>, StoreError> {
                Ok(None)
            }
            async fn list(
                &self,
                _args: &crate::contract::memory_store::ListArgs,
            ) -> Result<crate::contract::memory_store::ListPage, StoreError> {
                unimplemented!()
            }
            async fn tombstone(
                &self,
                _id: &crate::domain::RecordId,
                _reason: crate::contract::memory_store::TombstoneReason,
            ) -> Result<(), StoreError> {
                unimplemented!()
            }
            async fn versions(
                &self,
                _t: &crate::domain::TargetId,
            ) -> Result<Vec<crate::contract::memory_store::RecordVersion>, StoreError> {
                Ok(vec![])
            }
            async fn put_edge(
                &self,
                _e: &crate::contract::memory_store::Edge,
            ) -> Result<(), StoreError> {
                unimplemented!()
            }
            async fn remove_edge(
                &self,
                _k: &crate::contract::memory_store::EdgeKey,
            ) -> Result<bool, StoreError> {
                Ok(false)
            }
            async fn neighbours(
                &self,
                _id: &crate::domain::RecordId,
                _d: crate::contract::memory_store::EdgeDir,
            ) -> Result<Vec<crate::contract::memory_store::Edge>, StoreError> {
                Ok(vec![])
            }
            async fn search_keyword(
                &self,
                args: &KeywordSearchArgs<'_>,
            ) -> Result<KeywordSearchPage, StoreError> {
                *self.captured.lock().expect("mutex") = Some(args.auth_scope.clone());
                Ok(KeywordSearchPage {
                    candidates: vec![],
                    next_cursor: None,
                    explain: None,
                })
            }
            async fn search_semantic(
                &self,
                _args: &SemanticSearchArgs<'_>,
            ) -> Result<SemanticSearchPage, StoreError> {
                unimplemented!()
            }
            async fn search_hybrid(
                &self,
                _args: &HybridSearchArgs<'_>,
            ) -> Result<HybridSearchPage, StoreError> {
                unimplemented!()
            }
        }

        let store = ScopeRecorder {
            captured: StdMutex::new(None),
        };
        let mut request = req(SearchMode::Keyword);
        request.auth_scope = ScopeTuple {
            tenant: Some("acme".into()),
            ..Default::default()
        };
        let config = CairnConfig::default();
        run(&store, &config, &caps(true, false, false), request)
            .await
            .expect("ok");
        let captured = store.captured.lock().expect("mutex").clone();
        assert_eq!(
            captured.expect("captured").tenant.as_deref(),
            Some("acme"),
            "verb dispatcher must thread request.auth_scope into the leg's args"
        );
    }

    #[tokio::test]
    async fn empty_query_rejected() {
        let store = CallRecorder {
            calls: Mutex::new(vec![]),
            capabilities: MemoryStoreCapabilities::default(),
            last_hybrid: Mutex::new(None),
        };
        let config = CairnConfig::default();
        let mut request = req(SearchMode::Keyword);
        request.query = "  ".to_owned();
        let err = run(&store, &config, &caps(true, false, false), request)
            .await
            .expect_err("expected error");
        assert!(matches!(err, SearchError::InvalidArgs { .. }));
    }

    // M4-A: explain rejected when policy_trace is absent
    #[tokio::test]
    async fn explain_rejected_when_policy_trace_absent() {
        let store = CallRecorder {
            calls: Mutex::new(vec![]),
            capabilities: MemoryStoreCapabilities {
                fts: true,
                vector: true,
                graph_edges: false,
                transactions: true,
                per_record_consent_model: true,
                graph_search: false,
            },
            last_hybrid: Mutex::new(None),
        };
        let config = CairnConfig::default();
        let mut request = req(SearchMode::Hybrid);
        request.explain = true;
        let mut c = caps(true, true, true); // mode caps allowed
        c.policy_trace = false; // but policy_trace not advertised
        let err = run(&store, &config, &c, request)
            .await
            .expect_err("should reject");
        match err {
            SearchError::CapabilityUnavailable { capability } => {
                assert_eq!(capability, "cairn.mcp.v1.policy_trace");
            }
            other => panic!("wrong error: {other:?}"),
        }
        // No store call should have happened.
        assert!(store.calls.lock().expect("mutex").is_empty());
    }

    // M4-B: hybrid config knobs (blend, rrf_k, rerank_topk) flow through from config
    #[tokio::test]
    async fn hybrid_args_carry_config_knobs() {
        let store = CallRecorder {
            calls: Mutex::new(vec![]),
            capabilities: MemoryStoreCapabilities {
                fts: true,
                vector: true,
                graph_edges: false,
                transactions: true,
                per_record_consent_model: true,
                graph_search: false,
            },
            last_hybrid: Mutex::new(None),
        };
        let mut config = CairnConfig::default();
        config.search.rerank_blend = 0.42;
        config.search.rrf_k = 99;
        config.search.rerank_topk = 33;
        config.search.graph_confidence_min = 0.7;
        run(
            &store,
            &config,
            &caps(true, true, true),
            req(SearchMode::Hybrid),
        )
        .await
        .expect("hybrid run");
        let captured = store.last_hybrid.lock().expect("mutex").expect("captured");
        assert!((captured.0 - 0.42).abs() < 1e-6, "blend mismatch");
        assert_eq!(captured.1, 99, "rrf_k mismatch");
        assert_eq!(captured.2, 33, "rerank_topk mismatch");
        assert!(
            (captured.3 - 0.7).abs() < 1e-6,
            "graph_confidence_min mismatch: got {}",
            captured.3,
        );
    }

    // M4-C: token_budget_trim is invoked — oversized candidates are trimmed.
    // Allow function_too_many_lines: the length comes from the mandatory
    // MemoryStore boilerplate for OversizedStore, not from logic complexity.
    #[allow(clippy::too_many_lines)]
    #[tokio::test]
    async fn dispatcher_applies_token_budget_trim() {
        use crate::domain::ScopeTuple;
        use crate::domain::taxonomy::{MemoryClass, MemoryKind, MemoryVisibility};

        /// Build a `SearchCandidate` with a fixed-length snippet.
        fn oversized_candidate(index: usize, snippet_len: usize) -> SearchCandidate {
            let id_str = format!("01HQZX9F5N{index:016X}");
            SearchCandidate {
                record_id: RecordId::parse(id_str).expect("valid record id"),
                target_id: TargetId::parse("01HQZX9F5N0000000000000000").expect("valid target id"),
                scope: ScopeTuple::default(),
                kind: MemoryKind::Fact,
                class: MemoryClass::Episodic,
                visibility: MemoryVisibility::Private,
                bm25: 0.0,
                recency_seconds: 0,
                confidence: 1.0,
                salience: 1.0,
                staleness_seconds: 0,
                snippet: "x".repeat(snippet_len),
                record_json: "{}".to_owned(),
                semantic_distance: None,
            }
        }

        struct OversizedStore;

        #[async_trait::async_trait]
        impl MemoryStore for OversizedStore {
            fn name(&self) -> &'static str {
                "oversized"
            }
            fn capabilities(&self) -> &MemoryStoreCapabilities {
                static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
                    fts: true,
                    vector: false,
                    graph_edges: false,
                    transactions: true,
                    per_record_consent_model: true,
                    graph_search: false,
                };
                &CAPS
            }
            fn supported_contract_versions(&self) -> VersionRange {
                use super::super::super::contract::memory_store::CONTRACT_VERSION;
                VersionRange::new(
                    CONTRACT_VERSION,
                    ContractVersion::new(CONTRACT_VERSION.major, CONTRACT_VERSION.minor + 1, 0),
                )
            }
            async fn upsert(&self, _r: &MemoryRecord) -> Result<UpsertOutcome, StoreError> {
                unimplemented!()
            }
            async fn get(&self, _id: &RecordId) -> Result<Option<MemoryRecord>, StoreError> {
                Ok(None)
            }
            async fn list(&self, _args: &ListArgs) -> Result<ListPage, StoreError> {
                unimplemented!()
            }
            async fn tombstone(
                &self,
                _id: &RecordId,
                _reason: TombstoneReason,
            ) -> Result<(), StoreError> {
                unimplemented!()
            }
            async fn versions(&self, _target: &TargetId) -> Result<Vec<RecordVersion>, StoreError> {
                unimplemented!()
            }
            async fn put_edge(&self, _edge: &Edge) -> Result<(), StoreError> {
                unimplemented!()
            }
            async fn remove_edge(&self, _key: &EdgeKey) -> Result<bool, StoreError> {
                unimplemented!()
            }
            async fn neighbours(
                &self,
                _id: &RecordId,
                _dir: EdgeDir,
            ) -> Result<Vec<Edge>, StoreError> {
                unimplemented!()
            }
            async fn search_keyword(
                &self,
                _args: &KeywordSearchArgs<'_>,
            ) -> Result<KeywordSearchPage, StoreError> {
                // Return 3 candidates each with a 1000-char snippet.
                let cands = (0..3).map(|i| oversized_candidate(i, 1000)).collect();
                Ok(KeywordSearchPage {
                    candidates: cands,
                    next_cursor: None,
                    explain: None,
                })
            }
            async fn search_semantic(
                &self,
                _args: &SemanticSearchArgs<'_>,
            ) -> Result<SemanticSearchPage, StoreError> {
                unimplemented!()
            }
            async fn search_hybrid(
                &self,
                _args: &HybridSearchArgs<'_>,
            ) -> Result<HybridSearchPage, StoreError> {
                unimplemented!()
            }
        }

        let mut config = CairnConfig::default();
        // 1500 chars budget is less than 3 * 1000 = 3000, so the trim should
        // keep only the first candidate (which alone is 1000 chars — within
        // budget) and drop the rest (1000 + 1000 = 2000 > 1500).
        config.search.max_snippet_chars_per_page = 1500;
        let outcome = run(
            &OversizedStore,
            &config,
            &caps(true, false, false),
            req(SearchMode::Keyword),
        )
        .await
        .expect("ok");
        assert!(
            outcome.candidates.len() < 3,
            "trim should have reduced count from 3, got {}",
            outcome.candidates.len()
        );
    }
}
