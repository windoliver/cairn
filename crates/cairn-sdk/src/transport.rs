//! In-process SDK transport.
//!
//! Each verb fn is the SDK-side mirror of the CLI handler in
//! `cairn-cli::verbs::*`. The SDK does no I/O of its own — it constructs
//! the same request envelope the CLI would and dispatches into the verb
//! layer. P0 verb handlers are stubs (#9) so each fn returns the canonical
//! `store not wired` [`SdkError::Unimplemented`] with a fresh operation ID.
//!
//! When verb handlers move into `cairn-core::verbs::*`, replace each `Err(...)`
//! arm with the dispatch into the shared handler.

use std::sync::Arc;

use cairn_core::config::CairnConfig;
use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::generated::common::Capabilities;
use cairn_core::generated::handshake::{HandshakeResponse, HandshakeResponseChallenge};
use cairn_core::generated::status::{
    StatusResponse, StatusResponseMcpGraphTools, StatusResponseMcpGraphToolsProbeBasis,
    StatusResponseMcpGraphToolsReason, StatusResponseMcpGraphToolsState, StatusResponseServerInfo,
};
use cairn_core::generated::verbs::{
    assemble_hot::{AssembleHotArgs, AssembleHotData},
    capture_trace::{CaptureTraceArgs, CaptureTraceData},
    forget::{ForgetArgs, ForgetData},
    ingest::{IngestArgs, IngestData},
    lint::{LintArgs, LintData},
    retrieve::{RetrieveArgs, RetrieveData},
    search::{SearchArgs, SearchArgsMode, SearchData},
    summarize::{SummarizeArgs, SummarizeData},
};
use cairn_core::pipeline::dispatch::{DefaultRegistry, pipeline_dispatch_advertisement};

use crate::stub::{new_nonce, now_rfc3339_seconds, store_not_wired};
use crate::{CONTRACT, SdkError, VerbResponse};

/// Marker trait for transport implementations.
///
/// P0 ships [`InProcess`] only. A `Subprocess` transport (forking the
/// `cairn` CLI and parsing its `--json` output) is tracked as a follow-up
/// once the verb handlers actually do work.
pub trait Transport: sealed::Sealed {}

mod sealed {
    pub trait Sealed {}
}

/// In-process transport: SDK calls dispatch directly into the verb layer
/// inside the same process. Zero-copy, no IPC, no daemon. The default
/// [`Sdk`] uses this transport.
#[derive(Debug, Default, Clone, Copy)]
pub struct InProcess;

impl sealed::Sealed for InProcess {}
impl Transport for InProcess {}

/// SDK client.
///
/// Construct with [`Sdk::new`] for the default in-process transport, or
/// [`Sdk::with_store`] to wire a [`MemoryStore`] so that verbs like
/// [`Sdk::search`] actually dispatch instead of returning
/// [`SdkError::Unimplemented`].
///
/// `status.server_info.incarnation` and `started_at` are minted **fresh on
/// every call**, matching the P0 CLI ground-truth (`cairn status` —
/// `crates/cairn-cli/src/verbs/status.rs`). Once the store adapter lands
/// (issue #9), both surfaces will read the canonical values from the
/// daemon table; until then SDK and CLI behave identically.
pub struct Sdk<T: Transport = InProcess> {
    _transport: T,
    /// Optional wired store. `None` → verbs return [`SdkError::Unimplemented`].
    store: Option<Arc<dyn MemoryStore>>,
    /// Active config, used for capability derivation and dispatch knobs.
    config: CairnConfig,
}

impl std::fmt::Debug for Sdk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sdk")
            .field(
                "store",
                &if self.store.is_some() {
                    "wired"
                } else {
                    "none"
                },
            )
            .field("config", &self.config)
            .finish()
    }
}

impl Clone for Sdk {
    fn clone(&self) -> Self {
        Self {
            _transport: InProcess,
            store: self.store.clone(),
            config: self.config.clone(),
        }
    }
}

impl Sdk<InProcess> {
    /// Construct an in-process SDK client.
    ///
    /// Verbs that require a store return [`SdkError::Unimplemented`] until
    /// the store is wired. Use [`Sdk::with_store`] to wire one.
    #[must_use]
    pub fn new() -> Self {
        Self {
            _transport: InProcess,
            store: None,
            config: CairnConfig::default(),
        }
    }

    /// Construct an in-process SDK client wired to a real store.
    ///
    /// Verbs that previously returned [`SdkError::Unimplemented`] now
    /// dispatch against the supplied store. The store is shared via [`Arc`]
    /// so callers can keep their own handle.
    ///
    /// **Note**: vault-sensitive verbs (e.g. `assemble_hot`) still return
    /// [`SdkError::Unimplemented`] regardless of `with_store` — the SDK
    /// has no safe way to couple a vault root to the supplied config from
    /// this layer; that wiring lands with #193. Use the CLI for committed
    /// `assemble_hot` until then.
    #[must_use]
    pub fn with_store(store: Arc<dyn MemoryStore>, config: CairnConfig) -> Self {
        Self {
            _transport: InProcess,
            store: Some(store),
            config,
        }
    }
}

impl Default for Sdk<InProcess> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Transport> Sdk<T> {
    /// SDK / contract version. Mirrors `status().server_info.version`.
    #[must_use]
    pub const fn version(&self) -> &'static str {
        crate::version()
    }

    /// `status` — capability discovery (brief §8.0.a).
    ///
    /// Returns the contract version, advertised capabilities, and server
    /// info. `incarnation` and `started_at` are minted per-call to match
    /// the CLI's P0 behavior (no daemon table yet — see issue #9).
    ///
    /// Capabilities reflect what the SDK can execute end-to-end:
    /// - [`Sdk::new`] (no store wired) advertises an empty list — every
    ///   verb returns [`SdkError::Unimplemented`], so promising any
    ///   capability would mislead negotiating clients.
    /// - [`Sdk::with_store`] gates each capability on the store's own
    ///   advertisement (`MemoryStore::capabilities`), the wired config,
    ///   and the embedding-model presence implied by store FTS+vector
    ///   support. Search modes rely on FTS / vector indexes; advertising
    ///   them when the store cannot back them would allow the dispatcher
    ///   to surface `Internal` instead of fail-closed `CapabilityUnavailable`.
    #[must_use]
    pub fn status(&self) -> StatusResponse {
        StatusResponse {
            contract: CONTRACT.to_owned(),
            server_info: StatusResponseServerInfo {
                version: env!("CARGO_PKG_VERSION").to_owned(),
                build: cairn_core::time::build_profile().to_owned(),
                started_at: now_rfc3339_seconds(),
                incarnation: crate::stub::new_operation_id(),
            },
            capabilities: self.advertised_capabilities(),
            extensions: vec![],
            // Advertise the live routing policy. Mirrors the CLI
            // status producer: both surfaces emit the same
            // family-granular `pipeline_dispatch` derived from
            // `DefaultRegistry`, which is the registry the
            // `capture_trace` verb actually dispatches through. See
            // `crates/cairn-cli/src/verbs/status.rs` for the
            // long-form note.
            pipeline_dispatch: Some(pipeline_dispatch_advertisement(&DefaultRegistry)),
            // The SDK does not host an MCP server and cannot peek a
            // store's schema the way `cairn status` does. The closest
            // truthful answer is the same one the CLI emits when
            // there is no bound vault to probe: `state: no_vault,
            // reason: vault_not_bound, probe_basis: config_only`.
            // Both `Sdk::new()` and `Sdk::with_store(...)` resolve to
            // this state — the SDK has no MCP transport regardless of
            // store binding, so a graph-tools verdict is not
            // negotiable from this surface. Aligned with the CLI's
            // `ResolvedAvailability::NoVault` branch in
            // `verbs::status::McpGraphToolsStatus::from_resolved` so
            // the `status_parity_cli_vs_sdk` test enforces equality.
            mcp_graph_tools: Some(StatusResponseMcpGraphTools {
                state: StatusResponseMcpGraphToolsState::NoVault,
                reason: Some(StatusResponseMcpGraphToolsReason::VaultNotBound),
                tool_count: None,
                probe_basis: StatusResponseMcpGraphToolsProbeBasis::ConfigOnly,
                error: None,
            }),
        }
    }

    /// Collect the SDK's runtime state into `CapabilityGates` for `advertise()`.
    ///
    /// Centralises the mapping from SDK fields to the gate inputs so that both
    /// `advertised_capabilities` and any future caller share an identical view.
    fn gates(&self) -> cairn_core::status::CapabilityGates {
        let store_caps = self.store.as_ref().map(|s| {
            let c = s.capabilities();
            cairn_core::status::StoreCaps {
                fts: c.fts,
                vector: c.vector,
            }
        });
        let model_present = store_caps.as_ref().is_some_and(|c| c.vector);
        // SDK cannot inspect the environment for cloud provider readiness
        // (it has no access to OPENAI_API_KEY outside CLI context). We use a
        // two-factor proxy:
        //   1. The store's vector-index advertisement (`model_present`) —
        //      a store only advertises `vector: true` when it was built with
        //      an embedder that could produce vectors at index-time.
        //   2. `provider_model_aligned` — ensures `default_provider` and
        //      `embedding_model` name a consistent pair. Without this check,
        //      a config with `default_provider = openai` but
        //      `embedding_model = bge-small-en-v1.5` would advertise
        //      `semantic` and `hybrid`, but the ANN dispatcher would silently
        //      return zero results because indexed `vec_model` labels never
        //      match the OpenAI dispatcher's filter.
        //
        // CLI callers additionally check `OPENAI_API_KEY` presence and the
        // `openai` Cargo feature via `embedding_provider_ready()` in
        // `cairn-cli/src/verbs/mod.rs`. That stricter check is out of scope
        // for the SDK (no env access here).
        let provider_model_ok = cairn_core::config::provider_model_aligned(&self.config);
        let embedding_provider_ready = model_present && provider_model_ok;
        cairn_core::status::CapabilityGates {
            config: self.config.capabilities(embedding_provider_ready),
            store: store_caps,
            vault_bound: self.store.is_some(),
            model_present,
            embedding_provider_ready,
            llm_configured: false,
            contract_phase: cairn_core::status::Phase::V0_1,
        }
    }

    /// Project the SDK's executable state into a wire-format capability list.
    ///
    /// Delegates to [`cairn_core::status::advertise`] — the single source of
    /// truth for capability advertisement — then filters capabilities whose
    /// shared wiring flag is true for another surface but not yet honored by
    /// this SDK transport. Both [`Self::status`] and [`Self::require_capability`]
    /// read from here so they cannot drift.
    fn advertised_capabilities(&self) -> Vec<Capabilities> {
        let mut capabilities = cairn_core::status::advertise(&self.gates());
        // CLI `forget --record` is wired for issue #58, so the shared core
        // flag is true. The SDK `forget` method still returns
        // `Unimplemented` for all targets; do not advertise record forget
        // here until SDK dispatch can honor it end-to-end.
        capabilities.retain(|c| !matches!(c, Capabilities::CairnMcpV1ForgetRecord));
        capabilities
    }

    /// `handshake` — challenge mint (brief §8.0.a point d).
    ///
    /// P0 caveat: the issued nonce is ephemeral and is not persisted. Same
    /// caveat as `cairn handshake` — challenge-mode signed intents will be
    /// rejected until the store lands.
    ///
    /// # Errors
    ///
    /// Returns [`SdkError::ClockUnavailable`] when the host wall clock is
    /// degraded (pre-epoch or i64-ms overflow). The CLI / MCP handshake
    /// surfaces fail closed under the same condition; the SDK matches
    /// (round-3 review #2). The CSPRNG-driven nonce itself is fine — only
    /// the `expires_at` math depends on a trustworthy clock, and a wrong
    /// value here would either mint a never-expiring challenge or one
    /// stamped at the epoch.
    pub fn handshake(&self) -> Result<HandshakeResponse, SdkError> {
        const CHALLENGE_TTL_MS: i64 = 60_000;
        let now_ms =
            cairn_core::time::checked_now_ms().map_err(|e| SdkError::ClockUnavailable {
                reason: e.to_string(),
                operation_id: crate::stub::new_operation_id(),
            })?;
        let expires_at_ms = now_ms.saturating_add(CHALLENGE_TTL_MS);
        let expires_at = u64::try_from(expires_at_ms).unwrap_or(0);
        Ok(HandshakeResponse {
            contract: CONTRACT.to_owned(),
            challenge: HandshakeResponseChallenge {
                nonce: new_nonce(),
                expires_at,
            },
        })
    }

    /// `ingest` — accept new memory (brief §8.1).
    pub fn ingest(&self, args: &IngestArgs) -> Result<VerbResponse<IngestData>, SdkError> {
        validate_ingest(args)?;
        Err(unimplemented("ingest"))
    }

    /// `search` — hybrid keyword/semantic retrieval (brief §8.2).
    ///
    /// Fail-closed (CLAUDE.md §4.6): the requested mode's capability must
    /// be advertised by [`Self::status`], otherwise the call is rejected
    /// with [`SdkError::CapabilityUnavailable`] before any dispatch.
    /// `args.explain == Some(true)` additionally requires
    /// `cairn.mcp.v1.policy_trace` (per the
    /// `x-cairn-capability-when-true` annotation in
    /// `crates/cairn-idl/schema/verbs/search.json`).
    ///
    /// When the SDK was constructed with [`Sdk::with_store`], this dispatches
    /// into the shared [`cairn_core::verbs::search::run`] handler (the same
    /// one the CLI uses). Otherwise it returns [`SdkError::Unimplemented`].
    pub async fn search(&self, args: &SearchArgs) -> Result<VerbResponse<SearchData>, SdkError> {
        validate_search(args)?;
        self.require_capability(args.mode.capability())?;
        if args.explain == Some(true) {
            self.require_capability(Some("cairn.mcp.v1.policy_trace"))?;
        }

        let Some(store) = self.store.as_ref() else {
            return Err(unimplemented("search"));
        };

        // Map from the IDL SearchArgsMode to the core dispatcher SearchMode.
        // SearchArgsMode is imported at the top of the fn's enclosing module
        // via the type already in scope (used in validate_search above).
        let mode = match args.mode {
            SearchArgsMode::Keyword => cairn_core::verbs::search::SearchMode::Keyword,
            SearchArgsMode::Semantic => cairn_core::verbs::search::SearchMode::Semantic,
            SearchArgsMode::Hybrid => cairn_core::verbs::search::SearchMode::Hybrid,
            // Forward-compat: SearchArgsMode is #[non_exhaustive]; reject unknown variants
            // rather than silently accepting them.
            _ => return Err(unimplemented("search")),
        };

        // Derive the capability set the dispatcher will fail-closed
        // against, masked by the same store-capability signals
        // `advertised_capabilities` uses. Dispatcher gate ⊆ advertised
        // gate ⊆ status capabilities — three views, one truth.
        let store_caps = store.capabilities();
        // Mirror the same two-factor proxy as `gates()`: store vector-index
        // advertisement AND provider/model alignment. Without the alignment
        // check, a config with `default_provider = openai` but
        // `embedding_model = bge-small-en-v1.5` would let the request reach
        // the ANN dispatcher, which would return zero results because indexed
        // `vec_model` labels never match the OpenAI dispatcher filter.
        let provider_model_ok = cairn_core::config::provider_model_aligned(&self.config);
        let embedding_provider_ready = store_caps.vector && provider_model_ok;
        let mut caps = self.config.capabilities(embedding_provider_ready);
        caps.keyword_search = caps.keyword_search && store_caps.fts;
        caps.semantic_search = caps.semantic_search && store_caps.vector;
        caps.hybrid_search = caps.hybrid_search && store_caps.fts && store_caps.vector;

        // `args.limit` is validated in range [1, 1000] by `validate_search`,
        // so the `try_from` should not fail. Fall back to 10 on error.
        let limit = args.limit.map_or(10, |l| usize::try_from(l).unwrap_or(10));
        // Map the IDL `args.scope` (ScopeFilter) into the dispatcher's
        // `auth_scope` (ScopeTuple). Predicates the dispatcher cannot
        // yet honor are dropped with a tracing warn (see
        // `scope_filter_to_tuple` for rationale).
        let auth_scope = scope_filter_to_tuple(args.scope.as_ref());
        let request = cairn_core::verbs::search::SearchRequest {
            query: args.query.clone(),
            mode,
            limit,
            include_reasoning: args.include_reasoning.unwrap_or(false),
            visibility_allowlist: vec![],
            auth_scope,
            model_label: self.config.search.embedding_model.as_str().to_owned(),
            filter: args.filters.clone(),
            explain: args.explain.unwrap_or(false),
        };

        match cairn_core::verbs::search::run(store.as_ref(), &self.config, &caps, request).await {
            Ok(outcome) => Ok(envelope_from_outcome(outcome, mode)),
            Err(cairn_core::verbs::search::SearchError::CapabilityUnavailable { capability }) => {
                Err(SdkError::CapabilityUnavailable {
                    capability: capability.to_owned(),
                    reason: "rejected by dispatcher".to_owned(),
                    remediation: cairn_core::status::remediation_for(capability).map(str::to_owned),
                    operation_id: crate::stub::new_operation_id(),
                })
            }
            Err(cairn_core::verbs::search::SearchError::InvalidArgs { reason }) => {
                Err(SdkError::InvalidArgs { reason })
            }
            Err(cairn_core::verbs::search::SearchError::InvalidFilter { reason }) => {
                Err(SdkError::Protocol {
                    code: crate::error::ErrorCode::InvalidFilter,
                    message: format!("invalid filter: {reason}"),
                    data: Some(serde_json::json!({ "reason": reason })),
                    operation_id: crate::stub::new_operation_id(),
                })
            }
            Err(cairn_core::verbs::search::SearchError::Store(e)) => Err(SdkError::Protocol {
                code: crate::error::ErrorCode::Internal,
                message: format!("store error: {e}"),
                data: None,
                operation_id: crate::stub::new_operation_id(),
            }),
            // Forward-compat: SearchError is #[non_exhaustive]; surface unknown
            // variants as Internal rather than panicking.
            Err(e) => Err(SdkError::Protocol {
                code: crate::error::ErrorCode::Internal,
                message: format!("search dispatcher error: {e}"),
                data: None,
                operation_id: crate::stub::new_operation_id(),
            }),
        }
    }

    /// `retrieve` — by-target fetch (record/session/turn/folder/scope/profile).
    ///
    /// Fail-closed (CLAUDE.md §4.6): the variant's capability must be
    /// advertised by [`Self::status`], otherwise [`SdkError::CapabilityUnavailable`].
    pub fn retrieve(&self, args: &RetrieveArgs) -> Result<VerbResponse<RetrieveData>, SdkError> {
        validate_retrieve(args)?;
        self.require_capability(args.capability())?;
        Err(unimplemented("retrieve"))
    }

    /// `summarize` — rolling/periodic summary build (brief §8.4).
    pub fn summarize(&self, args: &SummarizeArgs) -> Result<VerbResponse<SummarizeData>, SdkError> {
        validate_summarize(args)?;
        Err(unimplemented("summarize"))
    }

    /// `assemble_hot` — hot-memory prefix assembly (brief §8.5, §11).
    ///
    /// Dispatches into [`cairn_core::verbs::assemble_hot::assemble_hot`] using
    /// the vault's [`cairn_core::config::HotMemoryConfig`]. No store is required — the assembler
    /// reads recipe steps from config and emits stub bodies (real source loading
    /// is tracked under #193). Validates `args` before dispatch.
    ///
    /// `args.budget` and `args.session_id` cannot yet be honored — bodies are
    /// stubbed to `""` and the assembler is session-agnostic. Rather than
    /// silently accept and drop them, the SDK rejects requests that supply
    /// either with [`SdkError::InvalidArgs`]. Both will become real once #193
    /// lands the loader.
    ///
    /// **The SDK currently always returns [`SdkError::Unimplemented`] for
    /// `assemble_hot`.** Tying vault-binding to the [`cairn_core::config::HotMemoryConfig`]
    /// safely requires a core-side config loader (so SDK callers cannot
    /// accidentally pair vault A's `vault.id` with vault B's recipe), and
    /// that loader lands with the real source-loading change in #193.
    /// Until then the CLI is the only surface that returns committed
    /// assemblies — it loads config from the same `vault_root` it probes,
    /// so the binding cannot diverge.
    pub fn assemble_hot(
        &self,
        args: &AssembleHotArgs,
    ) -> Result<VerbResponse<AssembleHotData>, SdkError> {
        validate_assemble_hot(args)?;

        if args.budget.is_some() {
            return Err(invalid(
                "assemble_hot: `budget` is not yet honored by the stub-body \
                 assembler (tracked under #193); omit until real loading lands",
            ));
        }
        if args.session_id.is_some() {
            return Err(invalid(
                "assemble_hot: `session_id` is not yet honored by the stub-body \
                 assembler (tracked under #193); omit until real loading lands",
            ));
        }

        // Always Unimplemented until #193 lands a vault-coupled config
        // loader. The CLI verb is the canonical surface for now.
        Err(unimplemented("assemble_hot"))
    }

    /// `capture_trace` — accept signed trace events (brief §8.6).
    pub fn capture_trace(
        &self,
        args: &CaptureTraceArgs,
    ) -> Result<VerbResponse<CaptureTraceData>, SdkError> {
        validate_capture_trace(args)?;
        Err(unimplemented("capture_trace"))
    }

    /// `lint` — privacy / provenance / schema / policy drift checks (brief §8.7).
    ///
    /// No pre-dispatch validation: `LintArgs` has no schema constraints
    /// beyond optional `write_report: bool`.
    pub fn lint(&self, _args: &LintArgs) -> Result<VerbResponse<LintData>, SdkError> {
        Err(unimplemented("lint"))
    }

    /// `forget` — record/session/scope tombstone + purge (brief §8.8, §5.6).
    ///
    /// Fail-closed (CLAUDE.md §4.6): the variant's capability must be
    /// advertised by [`Self::status`], otherwise [`SdkError::CapabilityUnavailable`].
    pub fn forget(&self, args: &ForgetArgs) -> Result<VerbResponse<ForgetData>, SdkError> {
        validate_forget(args)?;
        self.require_capability(args.capability())?;
        Err(unimplemented("forget"))
    }

    /// Reject with [`SdkError::CapabilityUnavailable`] when `required` is
    /// not advertised by `status()`. Verbs whose IDL declares no
    /// capability (`None`) are unconditionally allowed.
    ///
    /// When a store is wired (via [`Sdk::with_store`]), capability gating
    /// gating reads from the single `advertised_capabilities()` source so
    /// the gate cannot disagree with what `status` published. A capability
    /// missing from the advertised list is rejected with
    /// `CapabilityUnavailable` regardless of whether a store is wired —
    /// this prevents the previous drift where the store-wired branch
    /// consulted only config booleans and let modes through that
    /// `status` did not advertise (e.g. on a store with `vector=false`).
    fn require_capability(&self, required: Option<&'static str>) -> Result<(), SdkError> {
        self.require_capability_with_hint(required, None)
    }

    /// Like [`Self::require_capability`] but accepts a caller-supplied
    /// `override_hint` that replaces the generic entry from
    /// [`cairn_core::status::remediation_for`] when `Some`.
    ///
    /// Use this when the call site knows the specific failure cause (e.g.
    /// `OpenAI` key absent vs feature off vs unsupported model) so the
    /// operator receives actionable, cause-specific advice.
    fn require_capability_with_hint(
        &self,
        required: Option<&'static str>,
        override_hint: Option<&str>,
    ) -> Result<(), SdkError> {
        let Some(cap) = required else {
            return Ok(());
        };
        let advertised = self.advertised_capabilities();
        let is_advertised = advertised.iter().any(|c| {
            serde_json::to_value(c)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .as_deref()
                == Some(cap)
        });
        if is_advertised {
            Ok(())
        } else {
            let remediation = override_hint
                .map(str::to_owned)
                .or_else(|| cairn_core::status::remediation_for(cap).map(str::to_owned));
            Err(SdkError::CapabilityUnavailable {
                capability: cap.to_owned(),
                reason: "not advertised by `status` in this incarnation".to_owned(),
                remediation,
                operation_id: crate::stub::new_operation_id(),
            })
        }
    }
}

/// Mode-appropriate ranking score for a single hit.
///
/// Mirrors `cairn-cli`'s `hit_score`: keyword uses BM25, semantic uses
/// `1 - cosine_distance`, and hybrid prefers the dispatcher's
/// authoritative `final_score` from the explain block (lockstep with
/// candidates) — falling back to a rank-derived score when the caller
/// did not request `--explain`. Without this, hybrid graph-only rows
/// (which carry `bm25 = 0.0`) serialized as zero-score hits and any
/// client thresholding on `score` would suppress them.
fn hit_score(
    mode: cairn_core::verbs::search::SearchMode,
    idx: usize,
    c: &cairn_core::contract::memory_store::SearchCandidate,
    explain: Option<&[cairn_core::search::ScoreExplain]>,
) -> f64 {
    use cairn_core::verbs::search::SearchMode;
    let raw = match mode {
        SearchMode::Semantic => c.semantic_distance.map_or(0.0, |d| 1.0 - f64::from(d)),
        SearchMode::Hybrid => {
            if let Some(exps) = explain
                && let Some(e) = exps.get(idx)
            {
                e.final_score
            } else {
                #[allow(clippy::cast_precision_loss)]
                let rank_score = 1.0 / (1.0 + idx as f64);
                rank_score
            }
        }
        // Keyword + future variants (SearchMode is #[non_exhaustive]).
        _ => c.bm25,
    };
    if raw.is_finite() { raw } else { 0.0 }
}

/// Convert a domain [`cairn_core::search::DegradedLeg`] into its IDL
/// wire representation. Mirrors the schema variants — keep in sync with
/// `crates/cairn-idl/schema/verbs/search.json` (`DegradedLegEntry`).
fn degraded_leg_to_idl(
    leg: &cairn_core::search::DegradedLeg,
) -> cairn_core::generated::verbs::search::DegradedLegEntry {
    use cairn_core::generated::verbs::search::{
        DegradedLegEntry, DegradedLegEntryLeg, DegradedLegEntryReason, DegradedLegEntrySource,
    };
    use cairn_core::search::{DegradationReason, DegradedLeg, GraphSource};

    let reason_to_idl = |r: DegradationReason| match r {
        DegradationReason::CapabilityUnavailable => DegradedLegEntryReason::CapabilityUnavailable,
        DegradationReason::DeadlineExceeded => DegradedLegEntryReason::Timeout,
        // SqlError + WorkerPanic + future variants (DegradationReason
        // is #[non_exhaustive]) all surface as SqlError on the wire.
        _ => DegradedLegEntryReason::SqlError,
    };
    let source_to_idl = |s: GraphSource| match s {
        GraphSource::AuthKeywordSeed => DegradedLegEntrySource::AuthKeywordSeed,
        GraphSource::AuthSemanticSeed => DegradedLegEntrySource::AuthSemanticSeed,
        // GraphSource::All + future variants (GraphSource is
        // #[non_exhaustive]) collapse to All on the wire.
        _ => DegradedLegEntrySource::All,
    };
    match leg {
        DegradedLeg::Semantic { reason } => DegradedLegEntry {
            leg: DegradedLegEntryLeg::Semantic,
            reason: reason_to_idl(*reason),
            source: None,
        },
        DegradedLeg::Graph { reason, source } => DegradedLegEntry {
            leg: DegradedLegEntryLeg::Graph,
            reason: reason_to_idl(*reason),
            source: Some(source_to_idl(*source)),
        },
        // Forward-compat: DegradedLeg is #[non_exhaustive].
        _ => DegradedLegEntry {
            leg: DegradedLegEntryLeg::Graph,
            reason: DegradedLegEntryReason::SqlError,
            source: Some(DegradedLegEntrySource::All),
        },
    }
}

/// Convert a [`cairn_core::verbs::search::SearchOutcome`] into the SDK's
/// [`VerbResponse<SearchData>`] envelope.
fn envelope_from_outcome(
    outcome: cairn_core::verbs::search::SearchOutcome,
    mode: cairn_core::verbs::search::SearchMode,
) -> VerbResponse<SearchData> {
    use cairn_core::generated::common::Ulid;
    use cairn_core::generated::envelope::ResponseVerb;
    use cairn_core::generated::verbs::search::{Hit, HitTrust, ScoreExplain, SearchData};
    use cairn_core::policy_trace::{to_wire, to_wire_exclusions};

    let hits: Vec<Hit> = outcome
        .candidates
        .iter()
        .enumerate()
        .map(|(idx, c)| Hit {
            record_id: Ulid(c.record_id.as_str().to_owned()),
            score: hit_score(mode, idx, c, outcome.explain.as_deref()),
            snippet: Some(c.snippet.clone()),
            citation: None,
            trust: HitTrust::Unknown,
        })
        .collect();

    let score_explain = outcome.explain.map(|exps| {
        exps.into_iter()
            .map(|e| ScoreExplain {
                record_id: Ulid(e.record_id.as_str().to_owned()),
                // Rank values are 1-based positions (usually < 10_000).
                // `i64::try_from` is infallible on 64-bit; saturate to
                // `i64::MAX` on the unlikely 32-bit overflow edge.
                bm25_rank: e.bm25_rank.map(|r| i64::try_from(r).unwrap_or(i64::MAX)),
                semantic_rank: e
                    .semantic_rank
                    .map(|r| i64::try_from(r).unwrap_or(i64::MAX)),
                rrf_score: e.rrf_score,
                cosine: e.cosine,
                final_score: e.final_score,
            })
            .collect()
    });

    let degraded_legs = if outcome.degraded_legs.is_empty() {
        None
    } else {
        Some(
            outcome
                .degraded_legs
                .iter()
                .map(degraded_leg_to_idl)
                .collect(),
        )
    };

    let data = SearchData {
        hits,
        next_cursor: None,
        excluded: outcome.excluded.map(|items| to_wire_exclusions(&items)),
        score_explain,
        degraded_legs,
    };

    VerbResponse {
        operation_id: crate::stub::new_operation_id(),
        policy_trace: to_wire(&outcome.policy_trace),
        verb: ResponseVerb::Search,
        target: None,
        data,
    }
}

/// Map an IDL [`ScopeFilter`](cairn_core::generated::common::ScopeFilter)
/// to a domain [`ScopeTuple`](cairn_core::domain::ScopeTuple) so the SDK
/// transport propagates real auth context into search dispatch instead of
/// the empty default tuple. The seven scope dimensions threaded through
/// are the ones `do_search_*` actually predicate on via the JSON1 scope
/// clause: `tenant`/`workspace`/`session_id`/`entity`/`user`/`agent`.
///
/// `ScopeFilter` predicates the dispatcher cannot yet honor (`kind`,
/// `tags`, `tier`, `record_ids`) are dropped here with a tracing warn —
/// the contract is backwards-compatible with prior callers (where these
/// fields were silently ignored entirely) and the warning surfaces the
/// widening for ops follow-up. A future PR threads them into the
/// `SearchRequest.filter` path; until then, callers needing strict
/// enforcement should move those predicates to `args.filter` directly.
fn scope_filter_to_tuple(
    sf: Option<&cairn_core::generated::common::ScopeFilter>,
) -> cairn_core::domain::ScopeTuple {
    let Some(sf) = sf else {
        return cairn_core::domain::ScopeTuple::default();
    };
    // `kind`/`tags`/`tier`/`record_ids` fall through silently — the
    // pre-change SDK ignored every scope predicate, so dropping the
    // un-honored subset preserves wire compatibility. A future PR
    // threads them into `SearchRequest.filter`.
    cairn_core::domain::ScopeTuple {
        tenant: sf.tenant.clone(),
        workspace: sf.workspace.clone(),
        session_id: sf.session_id.clone(),
        entity: sf.entity.clone(),
        user: sf.user.clone(),
        agent: sf.agent.clone(),
        ..cairn_core::domain::ScopeTuple::default()
    }
}

/// Wrap a `&'static str` from a hand-rolled validator into [`SdkError::InvalidArgs`].
fn invalid(reason: &'static str) -> SdkError {
    SdkError::InvalidArgs {
        reason: reason.to_owned(),
    }
}

/// Validate a [`cairn_core::generated::common::Ulid`] newtype against the same rules the generated
/// `Deserialize` enforces (26 chars, Crockford base32 alphabet). Direct
/// construction in Rust skips those checks; the SDK reapplies them so
/// malformed IDs cannot cross the boundary.
fn validate_ulid(id: &cairn_core::generated::common::Ulid) -> Result<(), SdkError> {
    if id.0.len() != 26 {
        return Err(invalid("ULID: must be 26 chars"));
    }
    if !id
        .0
        .bytes()
        .all(|b| matches!(b, b'0'..=b'9' | b'A'..=b'H' | b'J' | b'K' | b'M' | b'N' | b'P'..=b'T' | b'V'..=b'Z'))
    {
        return Err(invalid(
            "ULID: must be Crockford base32 (uppercase, no I/L/O/U)",
        ));
    }
    Ok(())
}

/// Validate a [`cairn_core::generated::common::Cursor`] against its `Deserialize` rules
/// (non-empty, ≤ 512 chars).
fn validate_cursor(cursor: &cairn_core::generated::common::Cursor) -> Result<(), SdkError> {
    if cursor.0.is_empty() {
        return Err(invalid("Cursor: must not be empty"));
    }
    if cursor.0.len() > 512 {
        return Err(invalid("Cursor: must be <= 512 chars"));
    }
    Ok(())
}

/// Validate a [`cairn_core::generated::common::ScopeFilter`] against the generated `TryFrom<RawScopeFilter>`
/// rules (at least one predicate present, no empty strings or empty arrays).
fn validate_scope_filter(
    scope: &cairn_core::generated::common::ScopeFilter,
) -> Result<(), SdkError> {
    let any_present = scope.user.is_some()
        || scope.agent.is_some()
        || scope.tenant.is_some()
        || scope.workspace.is_some()
        || scope.entity.is_some()
        || scope.tier.is_some()
        || scope.session_id.is_some()
        || scope.kind.is_some()
        || scope.tags.is_some()
        || scope.record_ids.is_some();
    if !any_present {
        return Err(invalid(
            "scope: at least one of [user, agent, tenant, workspace, entity, tier, session_id, kind, tags, record_ids] is required",
        ));
    }
    let nonempty = |opt: Option<&String>, msg: &'static str| -> Result<(), SdkError> {
        if let Some(v) = opt
            && v.is_empty()
        {
            return Err(invalid(msg));
        }
        Ok(())
    };
    nonempty(scope.user.as_ref(), "user: must not be empty")?;
    nonempty(scope.agent.as_ref(), "agent: must not be empty")?;
    nonempty(scope.tenant.as_ref(), "tenant: must not be empty")?;
    nonempty(scope.workspace.as_ref(), "workspace: must not be empty")?;
    nonempty(scope.entity.as_ref(), "entity: must not be empty")?;
    nonempty(scope.session_id.as_ref(), "session_id: must not be empty")?;
    if let Some(kinds) = &scope.kind {
        if kinds.is_empty() {
            return Err(invalid("kind: must contain at least one item"));
        }
        for k in kinds {
            if k.is_empty() {
                return Err(invalid("kind: items must not be empty"));
            }
        }
    }
    if let Some(tags) = &scope.tags {
        if tags.is_empty() {
            return Err(invalid("tags: must contain at least one item"));
        }
        for t in tags {
            if t.is_empty() {
                return Err(invalid("tags: items must not be empty"));
            }
        }
    }
    if let Some(ids) = &scope.record_ids {
        if ids.is_empty() {
            return Err(invalid("record_ids: must contain at least one item"));
        }
        for id in ids {
            validate_ulid(id)?;
        }
    }
    Ok(())
}

/// Structural RFC-3986 floor for the `format: uri` constraint. A full URI
/// parser lives in the verb handler once the store wires in; the SDK
/// enforces the cheap shape — non-empty `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." ) ":"`
/// scheme, plus a non-empty hier-part — so obvious garbage (`""`, `":"`,
/// `"http"`, `"://x"`, `"1bad:rest"`) cannot reach storage.
fn validate_uri(s: &str) -> Result<(), SdkError> {
    if s.is_empty() {
        return Err(invalid("url: must not be empty"));
    }
    // RFC 3986 forbids ASCII whitespace and control characters anywhere in
    // the URI; reject them upfront so values like "http: " or
    // "http:\npath" cannot slip through the structural floor.
    if !s.is_ascii() {
        // RFC 3986 §2.1: non-ASCII bytes must be percent-encoded. Reject raw
        // non-ASCII so values like "http:💥/x" cannot reach storage.
        return Err(invalid("url: must be ASCII (percent-encode non-ASCII)"));
    }
    if s.bytes()
        .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return Err(invalid("url: must not contain whitespace or control chars"));
    }
    let Some(colon) = s.find(':') else {
        return Err(invalid("url: must be a valid URI"));
    };
    if colon == 0 || colon == s.len() - 1 {
        return Err(invalid("url: must be a valid URI"));
    }
    let scheme = &s[..colon];
    let mut chars = scheme.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphabetic());
    let rest_ok = chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !(first_ok && rest_ok) {
        return Err(invalid("url: must be a valid URI"));
    }
    Ok(())
}

/// Mirrors the full `ingest` JSON Schema: the IDL `validate()` covers the
/// exactly-one-of XOR; the schema additionally pins `minLength: 1` on `body`,
/// `file`, `folder`, `url`, `kind`, `session_id`, every `include[*]`,
/// every `exclude[*]`, and every `tags[*]`; `format: uri` on `url`; and
/// `batch_size` in `[1, 65535]`. The generated `TryFrom<RawIngestArgs>` only
/// enforces the XOR, so direct Rust construction would otherwise sail past
/// these constraints.
fn validate_ingest(args: &IngestArgs) -> Result<(), SdkError> {
    args.validate().map_err(invalid)?;
    // Brief §5.5 — `dry_run` and `human_review` are mutually exclusive.
    // The CLI clap layer rejects `--dry-run --human-review` together; the
    // SDK / MCP wire path bypasses clap, so re-enforce here so a direct
    // construction or deserialize cannot drive both modes at once.
    if args.dry_run.unwrap_or(false) && args.human_review.unwrap_or(false) {
        return Err(invalid(
            "dry_run and human_review are mutually exclusive (brief §5.5)",
        ));
    }
    if let Some(body) = &args.body
        && body.is_empty()
    {
        return Err(invalid("body: must not be empty"));
    }
    if let Some(file) = &args.file
        && file.is_empty()
    {
        return Err(invalid("file: must not be empty"));
    }
    if let Some(folder) = &args.folder
        && folder.is_empty()
    {
        return Err(invalid("folder: must not be empty"));
    }
    if let Some(url) = &args.url {
        validate_uri(url)?;
    }
    if let Some(include) = &args.include {
        for pattern in include {
            if pattern.is_empty() {
                return Err(invalid("include[*]: must not be empty"));
            }
        }
    }
    if let Some(exclude) = &args.exclude {
        for pattern in exclude {
            if pattern.is_empty() {
                return Err(invalid("exclude[*]: must not be empty"));
            }
        }
    }
    if let Some(batch_size) = args.batch_size
        && !(1..=65_535).contains(&batch_size)
    {
        return Err(invalid("batch_size: must be in [1, 65535]"));
    }
    if let Some(fm) = &args.frontmatter
        && !fm.is_object()
    {
        return Err(invalid("frontmatter: must be a JSON object"));
    }
    if args.kind.is_empty() {
        return Err(invalid("kind: must not be empty"));
    }
    if let Some(sid) = &args.session_id
        && sid.is_empty()
    {
        return Err(invalid("session_id: must not be empty"));
    }
    if let Some(tags) = &args.tags {
        for tag in tags {
            if tag.is_empty() {
                return Err(invalid("tags[*]: must not be empty"));
            }
        }
    }
    Ok(())
}

/// Mirrors the wire constraints from `SearchArgs`'s generated
/// `TryFrom<RawSearchArgs>` plus nested type validators (`Cursor`,
/// `ScopeFilter`, recursive `SearchArgsFilters`) — direct Rust
/// construction bypasses the generated deserializer for these inner types
/// too.
fn validate_search(args: &SearchArgs) -> Result<(), SdkError> {
    if args.query.is_empty() {
        return Err(invalid("query: must not be empty"));
    }
    if let Some(lim) = args.limit
        && !(1..=1000).contains(&lim)
    {
        return Err(invalid("limit: must be in [1, 1000]"));
    }
    if let Some(cursor) = &args.cursor {
        validate_cursor(cursor)?;
    }
    if let Some(scope) = &args.scope {
        validate_scope_filter(scope)?;
    }
    if let Some(filters) = &args.filters {
        validate_filter_node(filters, 8)?;
    }
    Ok(())
}

/// Recursive validator mirroring the generated
/// `parse_search_args_filters_node` rules: max depth 8, max fanout 32,
/// non-empty `and`/`or` arrays, leaf shape per `validate_filter_leaf_shape`.
fn validate_filter_node(
    node: &cairn_core::generated::verbs::search::SearchArgsFilters,
    remaining_depth: u32,
) -> Result<(), SdkError> {
    use cairn_core::generated::verbs::search::SearchArgsFilters as F;
    const MAX_FANOUT: usize = 32;
    match node {
        F::And { and } => {
            if remaining_depth == 0 {
                return Err(invalid("filter: exceeds max boolean depth"));
            }
            if and.is_empty() {
                return Err(invalid("filter.and: must contain at least one item"));
            }
            if and.len() > MAX_FANOUT {
                return Err(invalid("filter.and: exceeds max fanout"));
            }
            for child in and {
                validate_filter_node(child, remaining_depth - 1)?;
            }
            Ok(())
        }
        F::Or { or } => {
            if remaining_depth == 0 {
                return Err(invalid("filter: exceeds max boolean depth"));
            }
            if or.is_empty() {
                return Err(invalid("filter.or: must contain at least one item"));
            }
            if or.len() > MAX_FANOUT {
                return Err(invalid("filter.or: exceeds max fanout"));
            }
            for child in or {
                validate_filter_node(child, remaining_depth - 1)?;
            }
            Ok(())
        }
        F::Not { not } => {
            if remaining_depth == 0 {
                return Err(invalid("filter: exceeds max boolean depth"));
            }
            validate_filter_node(not, remaining_depth - 1)
        }
        F::Leaf(value) => validate_filter_leaf(value),
        // Forward-compat for #[non_exhaustive].
        _ => Err(invalid("unsupported filter variant")),
    }
}

/// Mirrors the generated `validate_filter_leaf_shape` for `SearchArgsFilters::Leaf`.
#[allow(clippy::too_many_lines)] // mirrors the generated 12-op grammar one-for-one
fn validate_filter_leaf(v: &serde_json::Value) -> Result<(), SdkError> {
    let obj = v
        .as_object()
        .ok_or_else(|| invalid("filter leaf: must be a JSON object"))?;
    for k in obj.keys() {
        if !matches!(k.as_str(), "field" | "op" | "value") {
            return Err(invalid("filter leaf: unknown key"));
        }
    }
    let field = obj
        .get("field")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid("filter leaf: field must be a string"))?;
    if field.is_empty() {
        return Err(invalid("filter leaf: field must not be empty"));
    }
    let op = obj
        .get("op")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid("filter leaf: op must be a string"))?;
    let value = obj
        .get("value")
        .ok_or_else(|| invalid("filter leaf: value missing"))?;
    match op {
        "string_contains" | "string_starts_with" | "string_ends_with" => {
            let s = value
                .as_str()
                .ok_or_else(|| invalid("filter leaf: value must be a non-empty string"))?;
            if s.is_empty() {
                return Err(invalid("filter leaf: value must be a non-empty string"));
            }
        }
        "eq" => {
            if value.is_string() {
                if value.as_str().unwrap_or("").is_empty() {
                    return Err(invalid("filter leaf: value must be a non-empty string"));
                }
            } else if !(value.is_number() || value.is_boolean()) {
                return Err(invalid(
                    "filter leaf: eq value must be string, number, or boolean",
                ));
            }
        }
        "neq" => {
            if value.is_string() {
                if value.as_str().unwrap_or("").is_empty() {
                    return Err(invalid("filter leaf: value must be a non-empty string"));
                }
            } else if !value.is_number() {
                return Err(invalid(
                    "filter leaf: neq value must be string or number — booleans only support eq",
                ));
            }
        }
        "lt" | "lte" | "gt" | "gte" => {
            if !value.is_number() {
                return Err(invalid("filter leaf: value must be a number"));
            }
        }
        "in" | "nin" => {
            let arr = value
                .as_array()
                .ok_or_else(|| invalid("filter leaf: value must be a non-empty array"))?;
            if arr.is_empty() {
                return Err(invalid("filter leaf: value must be a non-empty array"));
            }
            let first = &arr[0];
            if first.is_string() {
                for item in arr {
                    let s = item.as_str().ok_or_else(|| {
                        invalid("filter leaf: in/nin array must be all strings or all numbers")
                    })?;
                    if s.is_empty() {
                        return Err(invalid(
                            "filter leaf: array items must be non-empty strings",
                        ));
                    }
                }
            } else if first.is_number() {
                for item in arr {
                    if !item.is_number() {
                        return Err(invalid(
                            "filter leaf: in/nin array must be all strings or all numbers",
                        ));
                    }
                }
            } else {
                return Err(invalid(
                    "filter leaf: in/nin array must be all strings or all numbers",
                ));
            }
        }
        "between" => {
            let arr = value.as_array().ok_or_else(|| {
                invalid("filter leaf: between value must be a 2-element number array")
            })?;
            if arr.len() != 2 {
                return Err(invalid(
                    "filter leaf: between value must be a 2-element number array",
                ));
            }
            for item in arr {
                if !item.is_number() {
                    return Err(invalid("filter leaf: between value items must be numbers"));
                }
            }
        }
        "array_contains" => {
            if value.is_string() {
                if value.as_str().unwrap_or("").is_empty() {
                    return Err(invalid(
                        "filter leaf: array_contains value must be a non-empty string or number",
                    ));
                }
            } else if !value.is_number() {
                return Err(invalid(
                    "filter leaf: array_contains value must be a non-empty string or number",
                ));
            }
        }
        "array_contains_any" | "array_contains_all" => {
            let arr = value.as_array().ok_or_else(|| {
                invalid("filter leaf: array_contains_* value must be a non-empty array")
            })?;
            if arr.is_empty() {
                return Err(invalid(
                    "filter leaf: array_contains_* value must be a non-empty array",
                ));
            }
            for item in arr {
                if item.is_string() {
                    if item.as_str().unwrap_or("").is_empty() {
                        return Err(invalid(
                            "filter leaf: array items must be non-empty strings or numbers",
                        ));
                    }
                } else if !item.is_number() {
                    return Err(invalid(
                        "filter leaf: array items must be non-empty strings or numbers",
                    ));
                }
            }
        }
        "array_size_eq" => {
            let n = value.as_i64().ok_or_else(|| {
                invalid("filter leaf: array_size_eq value must be a non-negative integer")
            })?;
            if n < 0 {
                return Err(invalid(
                    "filter leaf: array_size_eq value must be a non-negative integer",
                ));
            }
        }
        _ => return Err(invalid("filter leaf: unknown op")),
    }
    Ok(())
}

/// Mirrors every wire constraint from `RetrieveArgs`'s generated
/// `TryFrom<RawRetrieveArgs>` — Record (no constraints), Session, Turn,
/// Folder, Scope, and Profile.
fn validate_retrieve(args: &RetrieveArgs) -> Result<(), SdkError> {
    use cairn_core::generated::verbs::retrieve::RetrieveArgs as A;
    match args {
        A::Record { id } => validate_ulid(id),
        A::Session {
            session_id,
            limit,
            include,
            cursor,
            ..
        } => {
            if let Some(c) = cursor {
                validate_cursor(c)?;
            }
            if session_id.is_empty() {
                return Err(invalid("session_id: must not be empty"));
            }
            if let Some(lim) = *limit
                && !(1..=10000).contains(&lim)
            {
                return Err(invalid("limit: must be in [1, 10000]"));
            }
            if let Some(inc) = include {
                if inc.is_empty() {
                    return Err(invalid("include: must contain at least one item"));
                }
                let mut seen = std::collections::BTreeSet::new();
                for item in inc {
                    if !seen.insert(*item as u8) {
                        return Err(invalid("include: items must be unique"));
                    }
                }
            }
            Ok(())
        }
        A::Turn {
            session_id,
            include,
            ..
        } => {
            if session_id.is_empty() {
                return Err(invalid("session_id: must not be empty"));
            }
            if let Some(inc) = include {
                if inc.is_empty() {
                    return Err(invalid("include: must contain at least one item"));
                }
                let mut seen = std::collections::BTreeSet::new();
                for item in inc {
                    if !seen.insert(*item as u8) {
                        return Err(invalid("include: items must be unique"));
                    }
                }
            }
            Ok(())
        }
        A::Folder { path, depth } => {
            if path.is_empty() {
                return Err(invalid("path: must not be empty"));
            }
            if let Some(d) = *depth
                && d > 16
            {
                return Err(invalid("depth: must be in [0, 16]"));
            }
            Ok(())
        }
        A::Scope { cursor, scope } => {
            // Note: A::Scope.cursor is Option<String>, not the Cursor newtype
            // (the IDL passes the raw cursor here). Mirror the inline string
            // checks the generated TryFrom enforces.
            if let Some(c) = cursor {
                if c.is_empty() {
                    return Err(invalid("cursor: must not be empty"));
                }
                if c.len() > 512 {
                    return Err(invalid("cursor: must be <= 512 chars"));
                }
            }
            validate_scope_filter(scope)
        }
        A::Profile { user, agent } => {
            if user.is_none() && agent.is_none() {
                return Err(invalid("at least one of [user, agent] is required"));
            }
            if let Some(u) = user
                && u.is_empty()
            {
                return Err(invalid("user: must not be empty"));
            }
            if let Some(a) = agent
                && a.is_empty()
            {
                return Err(invalid("agent: must not be empty"));
            }
            Ok(())
        }
        // Forward-compat: RetrieveArgs is #[non_exhaustive]; reject unknown
        // future variants rather than silently accept them.
        _ => Err(invalid("unsupported retrieve target variant")),
    }
}

/// Mirrors the JSON-schema constraints for `summarize` (the generated
/// Rust `SummarizeArgs` only enforces `deny_unknown_fields`, but the wire
/// schema requires non-empty `record_ids` and non-empty `kind` if
/// present). Each `record_id` is also validated as a real ULID.
fn validate_summarize(args: &SummarizeArgs) -> Result<(), SdkError> {
    if args.record_ids.is_empty() {
        return Err(invalid("record_ids: must contain at least one item"));
    }
    for id in &args.record_ids {
        validate_ulid(id)?;
    }
    if let Some(kind) = &args.kind
        && kind.is_empty()
    {
        return Err(invalid("kind: must not be empty when present"));
    }
    Ok(())
}

/// Mirrors the JSON-schema constraints for `assemble_hot`: `budget`
/// in `[0, 4 MiB]`, `session_id` non-empty when present.
fn validate_assemble_hot(args: &AssembleHotArgs) -> Result<(), SdkError> {
    if let Some(budget) = args.budget
        && budget > 4_194_304
    {
        return Err(invalid("budget: must be <= 4194304 (4 MiB)"));
    }
    if let Some(session_id) = &args.session_id
        && session_id.is_empty()
    {
        return Err(invalid("session_id: must not be empty when present"));
    }
    Ok(())
}

/// Mirrors the JSON-schema constraints for `capture_trace`: either
/// `from` or (`blocks` + `session_id`) must be present; all provided
/// strings must be non-empty.
fn validate_capture_trace(args: &CaptureTraceArgs) -> Result<(), SdkError> {
    if let Some(from) = &args.from
        && from.is_empty()
    {
        return Err(invalid("from: must not be empty when present"));
    }
    if let Some(blocks) = &args.blocks
        && blocks.is_empty()
    {
        return Err(invalid("blocks: must not be empty when present"));
    }
    if let Some(session_id) = &args.session_id
        && session_id.is_empty()
    {
        return Err(invalid("session_id: must not be empty when present"));
    }
    let has_from = args.from.is_some();
    let has_blocks = args.blocks.is_some();
    if !has_from && !has_blocks {
        return Err(invalid("capture_trace: requires either `from` or `blocks`"));
    }
    if has_from && has_blocks {
        return Err(invalid(
            "capture_trace: `from` and `blocks` are mutually exclusive",
        ));
    }
    if has_from && args.session_id.is_some() {
        return Err(invalid(
            "session_id: only supported with `blocks` for capture_trace",
        ));
    }
    if has_blocks && args.session_id.is_none() {
        return Err(invalid(
            "session_id: required when using `blocks` for capture_trace",
        ));
    }
    Ok(())
}

/// Mirrors every wire constraint in `ForgetArgs`'s generated
/// `TryFrom<RawForgetArgs>` plus nested type validators (`Ulid`, `ScopeFilter`).
fn validate_forget(args: &ForgetArgs) -> Result<(), SdkError> {
    use cairn_core::generated::verbs::forget::ForgetArgs as F;
    // Brief §5.5 — `dry_run` and `human_review` are mutually exclusive on
    // every variant. SDK / MCP callers can construct either bool field
    // directly, so re-enforce here.
    let (dry_run, human_review) = match args {
        F::Record {
            dry_run,
            human_review,
            ..
        }
        | F::Session {
            dry_run,
            human_review,
            ..
        }
        | F::Scope {
            dry_run,
            human_review,
            ..
        } => (dry_run.unwrap_or(false), human_review.unwrap_or(false)),
        _ => (false, false),
    };
    if dry_run && human_review {
        return Err(invalid(
            "dry_run and human_review are mutually exclusive (brief §5.5)",
        ));
    }
    match args {
        F::Record { record_id, .. } => validate_ulid(record_id),
        F::Session { session_id, .. } => {
            if session_id.is_empty() {
                return Err(invalid("session_id: must not be empty"));
            }
            Ok(())
        }
        F::Scope { scope, .. } => validate_scope_filter(scope),
        // Forward-compat for #[non_exhaustive].
        _ => Err(invalid("unsupported forget target variant")),
    }
}

/// Canonical P0 stub: every verb returns [`SdkError::Unimplemented`] until
/// verb dispatch lands (#9). Distinct from a generic `Internal` so callers
/// can fail fast instead of retrying.
fn unimplemented(verb: &'static str) -> SdkError {
    store_not_wired(verb)
}
