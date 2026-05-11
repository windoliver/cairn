//! `CairnMcpHandler` — MCP `ServerHandler` implementation.
//!
//! Wires the IDL-generated [`TOOLS`] constant into the `tools/list` response
//! and routes `tools/call` either through the real verb dispatcher (when a
//! store is wired via [`CairnMcpHandler::with_store`]) or through
//! [`dispatch_stub`] for verbs whose dispatch has not yet landed.

use std::sync::Arc;

use std::collections::BTreeMap;

use rmcp::{
    RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, Content, Implementation, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::RequestContext,
};

use cairn_core::config::CairnConfig;
use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::domain::ScopeTuple;
use cairn_core::mcp_auth::{McpAuthContext, McpGraphAvailability, McpTransport};

use cairn_store_sqlite::SqliteMemoryStore;
use cairn_store_sqlite::entity_graph::queries::GraphQueries;

use crate::generated::TOOLS;

/// Materialized graph-request bundle. Resolved once; carried into dispatch.
/// Holds the **concrete** sqlite store handle — `GraphQueries` is sqlite-
/// specific and there is no graph-capable trait on `dyn MemoryStore` yet.
struct GraphRequest {
    store: Arc<SqliteMemoryStore>,
    allowed: Vec<ScopeTuple>,
    now_ms: i64,
}

/// Reason why a graph request could not be materialized.
enum GraphUnavailable {
    /// Transport/config/capability gate from Plan A returned non-Available.
    /// The inner value carries the specific availability variant for
    /// diagnostics; currently not read by callers that match only on `Err(_)`.
    #[allow(dead_code)]
    Gate(McpGraphAvailability),
    /// Resolver returned Err or empty Vec at request time.
    Resolver,
}

/// Build a `CallToolResult` indicating a capability is unavailable.
fn capability_unavailable_result(name: &str) -> CallToolResult {
    CallToolResult::error(vec![Content::text(format!(
        "capability unavailable: {name}"
    ))])
}

/// MCP server handler for the Cairn verb layer.
///
/// Implements [`rmcp::ServerHandler`]. When constructed with
/// [`CairnMcpHandler::with_store`] the `search` tool dispatches through
/// [`cairn_core::verbs::search::run`]; all other tools fall back to
/// [`dispatch_stub`] until their real dispatch lands in a follow-up PR.
pub struct CairnMcpHandler {
    store: Option<Arc<dyn MemoryStore>>,
    sqlite_store: Option<Arc<SqliteMemoryStore>>,
    scope: Option<Arc<dyn cairn_core::mcp_auth::McpSessionScope>>,
    config: CairnConfig,
    principal: ScopeTuple,
    transport: McpTransport,
}

impl Default for CairnMcpHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for CairnMcpHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CairnMcpHandler")
            .field("store_wired", &self.store.is_some())
            .field("sqlite_store_wired", &self.sqlite_store.is_some())
            .field("scope_wired", &self.scope.is_some())
            // config omitted: may contain sensitive keys (embedding model paths,
            // provider credentials). Use finish_non_exhaustive to signal the
            // omission to derive-based tooling.
            .finish_non_exhaustive()
    }
}

impl CairnMcpHandler {
    /// Create a handler without a wired store (dispatch falls back to stub).
    #[must_use]
    pub fn new() -> Self {
        Self {
            store: None,
            sqlite_store: None,
            scope: None,
            config: CairnConfig::default(),
            principal: ScopeTuple::default(),
            transport: McpTransport::Stdio,
        }
    }

    /// Create a handler wired to a real store.
    ///
    /// Tools that have a dispatch path use the store; everything else falls
    /// back to the existing stub.
    #[must_use]
    pub fn with_store(store: Arc<dyn MemoryStore>, config: CairnConfig) -> Self {
        Self {
            store: Some(store),
            sqlite_store: None,
            scope: None,
            config,
            principal: ScopeTuple::default(),
            transport: McpTransport::Stdio,
        }
    }

    /// Create a handler wired to a real store, a scope resolver, and a
    /// principal. Plan A entry point — `tools/list` still returns the
    /// 8-verb manifest; graph tools land in Plan C.
    #[must_use]
    pub fn with_store_and_scope(
        store: Arc<dyn MemoryStore>,
        scope: Arc<dyn cairn_core::mcp_auth::McpSessionScope>,
        config: CairnConfig,
        principal: ScopeTuple,
    ) -> Self {
        Self {
            store: Some(store),
            sqlite_store: None,
            scope: Some(scope),
            config,
            principal,
            transport: McpTransport::Stdio,
        }
    }

    /// Create a handler wired to both the trait-object store (verb path) and
    /// the concrete sqlite store (graph path), plus a scope resolver and
    /// principal. Plan C entry point — enables graph tool advertisement and
    /// dispatch.
    #[must_use]
    pub fn with_store_scope_and_sqlite(
        store: Arc<dyn MemoryStore>,
        sqlite_store: Arc<SqliteMemoryStore>,
        scope: Arc<dyn cairn_core::mcp_auth::McpSessionScope>,
        config: CairnConfig,
        principal: ScopeTuple,
    ) -> Self {
        Self {
            store: Some(store),
            sqlite_store: Some(sqlite_store),
            scope: Some(scope),
            config,
            principal,
            transport: McpTransport::Stdio,
        }
    }

    /// Returns `true` if a store is wired into this handler.
    #[must_use]
    pub fn has_store(&self) -> bool {
        self.store.is_some()
    }

    /// Returns `true` if a scope resolver is wired into this handler.
    #[must_use]
    pub fn has_scope(&self) -> bool {
        self.scope.is_some()
    }

    /// Returns the principal this handler was constructed with.
    #[must_use]
    pub fn principal(&self) -> &ScopeTuple {
        &self.principal
    }

    /// Returns the names of all tools in the current manifest.
    ///
    /// Includes the eight IDL verbs unconditionally and the `handshake`
    /// prelude tool when (a) a sqlite store is wired AND (b)
    /// [`cairn_core::status::wiring::REPLAY_CHALLENGE_WIRED`] is `true`.
    /// While the wiring flag is `false`, no signed-mutation path consumes
    /// the persisted nonce, so listing `handshake` would let clients mint
    /// rows they cannot redeem (brief §15 fail-closed; round-1 review #1).
    /// Graph tools are appended dynamically by `list_tools` based on a
    /// per-request scope probe — they are not listed here.
    #[must_use]
    pub fn listed_tool_names(&self) -> Vec<String> {
        let mut names: Vec<String> = TOOLS.iter().map(|t| t.name.to_string()).collect();
        if crate::prelude_tools::is_enabled(
            self.sqlite_store.is_some(),
            cairn_core::status::wiring::REPLAY_CHALLENGE_WIRED,
        ) {
            names.extend(
                crate::prelude_tools::PRELUDE_TOOLS
                    .iter()
                    .map(|t| t.name.to_string()),
            );
        }
        names
    }

    /// Build an auth context for the current request.
    ///
    /// On stdio the principal is fixed at construction time (single-tenant
    /// invariant from spec §2.1.1: `ConfigBackedScope` keys exclusively on
    /// the configured principal). The `request_id` is taken from the rmcp
    /// [`rmcp::service::RequestContext::id`] passed by the caller so a
    /// future context-sensitive resolver sees a real per-request token
    /// rather than a constant sentinel.
    fn auth_context_for<'r>(&'r self, request_id: &'r str) -> McpAuthContext<'r> {
        McpAuthContext::new(&self.principal, request_id)
    }

    /// Single source of truth for "is the graph surface usable for this
    /// request, and if so with what scope set?" Called by both `list_tools`
    /// and `call_tool`. Never called twice per request.
    fn materialize_graph_request(
        &self,
        ctx: &McpAuthContext<'_>,
    ) -> Result<GraphRequest, GraphUnavailable> {
        let (Some(store), Some(sqlite_store), Some(scope)) = (
            self.store.as_ref(),
            self.sqlite_store.as_ref(),
            self.scope.as_ref(),
        ) else {
            return Err(GraphUnavailable::Gate(
                McpGraphAvailability::UnavailableNoStoreCapability,
            ));
        };
        let avail = self.config.mcp_graph_tools_available(
            Some(scope.as_ref()),
            self.transport,
            store.capabilities(),
        );
        if !matches!(avail, McpGraphAvailability::Available { .. }) {
            return Err(GraphUnavailable::Gate(avail));
        }
        // Single resolver call — Err or empty -> Resolver-unavailable, never panic.
        let allowed = match scope.allowed_scopes(ctx) {
            Ok(v) if !v.is_empty() => v,
            _ => return Err(GraphUnavailable::Resolver),
        };
        // Validate every tuple the resolver returned. The graph SQL only
        // binds the six dimensions exposed by `dimension_iter`; a resolver
        // that returns a tuple with `project` set or otherwise malformed
        // would have that restriction silently dropped, broadening the
        // caller's authorization. Fail closed on any non-validating tuple
        // — config-time validation only catches `ConfigBackedScope`; the
        // `McpSessionScope` trait is public and supports alternate
        // resolvers that may produce dynamic tuples.
        for tup in &allowed {
            if tup.validate().is_err() {
                return Err(GraphUnavailable::Resolver);
            }
        }
        Ok(GraphRequest {
            store: sqlite_store.clone(),
            allowed,
            now_ms: chrono::Utc::now().timestamp_millis(),
        })
    }

    /// Snapshot of the status response this handler advertises through MCP
    /// `initialize`. Used by parity tests (issue #53) so the test does not
    /// need to reach into rmcp's extension slot. Return shape is identical
    /// to what `Sdk::status()` and `cairn status --json` produce for the
    /// same inputs — volatile fields (`incarnation`, `started_at`) differ
    /// per call.
    #[must_use]
    pub fn status_response(&self) -> cairn_core::generated::status::StatusResponse {
        self.build_status_response()
    }

    fn build_status_response(&self) -> cairn_core::generated::status::StatusResponse {
        use cairn_core::generated::status::{StatusResponse, StatusResponseServerInfo};
        use cairn_core::pipeline::dispatch::{DefaultRegistry, pipeline_dispatch_advertisement};

        let store_caps = self.store.as_ref().map(|s| {
            let c = s.capabilities();
            cairn_core::status::StoreCaps {
                fts: c.fts,
                vector: c.vector,
            }
        });
        let model_present = store_caps.as_ref().is_some_and(|c| c.vector);
        // MCP handler mirrors SDK: use a two-factor proxy for
        // `embedding_provider_ready`:
        //   1. Store vector-index advertisement (`model_present`).
        //   2. Provider/model alignment (`provider_model_aligned`): if
        //      `default_provider = openai` but `embedding_model` names a
        //      local candle model (or vice-versa), advertising semantic/hybrid
        //      would cause the dispatcher to return silent empty results
        //      instead of a clean CapabilityUnavailable. Gate-closed instead.
        // See cairn-sdk/src/transport.rs::gates() for the full rationale.
        let provider_model_ok = cairn_core::config::provider_model_aligned(&self.config);
        let embedding_provider_ready = model_present && provider_model_ok;
        let gates = cairn_core::status::CapabilityGates {
            config: self.config.capabilities(embedding_provider_ready),
            store: store_caps,
            vault_bound: self.store.is_some(),
            model_present,
            embedding_provider_ready,
            llm_configured: false,
            contract_phase: cairn_core::status::Phase::V0_1,
        };

        // Post-filter capabilities whose shared core wiring flags are true
        // for another surface but not yet honored by this MCP transport.
        // CLI `forget --record` is wired for issue #58, but MCP non-search
        // verbs still fall through `dispatch_stub`; do not advertise record
        // forget here until MCP dispatch can honor it end-to-end.
        let mut capabilities = cairn_core::status::advertise(&gates);
        capabilities.retain(|c| {
            !matches!(
                c,
                cairn_core::generated::common::Capabilities::CairnMcpV1ForgetRecord
            )
        });

        // Post-filter replay capabilities to keep status advertisement and
        // `handshake` tool exposure in lockstep at the MCP boundary
        // (round-2 review #3). `cairn-core::status::advertise` does not
        // know about MCP's concrete sqlite handle; without this filter a
        // `with_store(...)` handler would advertise
        // `cairn.mcp.v1.replay.{sequence,challenge}` once the wiring
        // flag flips, even though `tools/list` would still omit
        // `handshake` (the prelude needs the sqlite handle to mint
        // nonces). Brief §15 fail-closed: capability advertisement
        // tracks the runtime that can actually honor it.
        if self.sqlite_store.is_none() {
            capabilities.retain(|c| {
                !matches!(
                    c,
                    cairn_core::generated::common::Capabilities::CairnMcpV1ReplayChallenge
                        | cairn_core::generated::common::Capabilities::CairnMcpV1ReplaySequence
                )
            });
        }

        StatusResponse {
            contract: "cairn.mcp.v1".to_owned(),
            server_info: StatusResponseServerInfo {
                version: env!("CARGO_PKG_VERSION").to_owned(),
                build: cairn_core::time::build_profile().to_owned(),
                started_at: cairn_core::time::now_rfc3339_seconds(),
                incarnation: cairn_core::time::new_operation_id(),
            },
            capabilities,
            extensions: vec![],
            pipeline_dispatch: Some(pipeline_dispatch_advertisement(&DefaultRegistry)),
            // Mirror the SDK's `Sdk::status` and CLI's no-vault path so
            // CLI/SDK/MCP three-way parity holds. Until MCP exposes a
            // `with_scope_and_sqlite_store` constructor that this helper
            // can probe, every MCP status emits the same `NoVault`
            // wire response (state: no_vault, reason: vault_not_bound,
            // probe_basis: config_only) — the closest truthful answer
            // when there is no bound vault to probe.
            mcp_graph_tools: Some(cairn_core::generated::status::StatusResponseMcpGraphTools {
                state: cairn_core::generated::status::StatusResponseMcpGraphToolsState::NoVault,
                reason: Some(
                    cairn_core::generated::status::StatusResponseMcpGraphToolsReason::VaultNotBound,
                ),
                tool_count: None,
                probe_basis:
                    cairn_core::generated::status::StatusResponseMcpGraphToolsProbeBasis::ConfigOnly,
                error: None,
            }),
        }
    }
}

impl ServerHandler for CairnMcpHandler {
    /// Return server identity and advertise tool capability.
    ///
    /// The Cairn status block (`capabilities[]`, `pipeline_dispatch`, etc.) is
    /// embedded in `serverCapabilities.experimental["cairn.status"]`.
    ///
    /// rmcp 0.14's `ServerCapabilities` does NOT have a dedicated top-level
    /// extension field for Cairn's status JSON — the only arbitrary-JSON slot
    /// in `InitializeResult` is `capabilities.experimental` (a
    /// `BTreeMap<String, serde_json::Map>`) and the `instructions` string. We
    /// use `experimental["cairn.status"]` because:
    ///
    /// 1. It is typed for arbitrary JSON objects — no encoding gymnastics.
    /// 2. The MCP spec explicitly reserves `experimental` for vendor extensions.
    /// 3. `instructions` is a human-readable string; embedding JSON there would
    ///    be non-standard and harder to parse for MCP clients.
    ///
    /// Wire shape of `experimental["cairn.status"]`:
    /// ```json
    /// {
    ///   "contract": "cairn.mcp.v1",
    ///   "server_info": { "version": "...", "build": "...", ... },
    ///   "capabilities": ["cairn.mcp.v1.search.keyword", ...],
    ///   "extensions": [],
    ///   "pipeline_dispatch": { ... }
    /// }
    /// ```
    ///
    /// This makes the advertised `capabilities[]` and `pipeline_dispatch`
    /// machine-readable to any MCP client on the actual `initialize` wire path.
    /// The `status_response()` helper remains for unit tests that want to inspect
    /// the block without running an MCP session.
    fn get_info(&self) -> ServerInfo {
        // Build the status block and convert to a serde_json object so it can
        // be inserted into `experimental`. Serialization failure here would mean
        // the generated StatusResponse type is broken — use an empty map as a
        // safe fallback rather than panicking in a library function.
        let status_value = serde_json::to_value(self.build_status_response())
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
        let status_map = match status_value {
            serde_json::Value::Object(m) => m,
            _ => serde_json::Map::new(),
        };

        // Build ServerCapabilities via the rmcp builder (required — the struct is
        // `#[non_exhaustive]` and cannot be constructed with a struct literal from
        // outside the crate). Start with tools + experimental, then insert the
        // cairn.status block into the experimental map.
        let mut caps = ServerCapabilities::builder()
            .enable_tools()
            .enable_experimental()
            .build();
        // The enable_experimental() call sets experimental to Some(BTreeMap::new()).
        // Insert our status block. The `unwrap_or_else` is unreachable in practice
        // (we just called enable_experimental), but avoids a panic in library code.
        caps.experimental
            .get_or_insert_with(BTreeMap::new)
            .insert("cairn.status".to_owned(), status_map);

        ServerInfo::new(caps)
            .with_server_info(Implementation::new("cairn", env!("CARGO_PKG_VERSION")))
    }

    /// Return all Cairn verbs as MCP tools, plus any graph tools when available.
    ///
    /// Converts each [`crate::generated::ToolDecl`] entry in [`TOOLS`] into
    /// an rmcp [`Tool`], parsing the embedded JSON-schema bytes with
    /// `serde_json`. If `materialize_graph_request` succeeds, the five
    /// `graph.*` tools are appended.
    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, rmcp::ErrorData>> + Send + '_
    {
        let request_id = context.id.to_string();
        let mut tools: Vec<Tool> = TOOLS
            .iter()
            .map(|decl| {
                // `input_schema` is `&'static [u8]` containing a valid JSON object.
                // Failure here means IDL-generated bytes are corrupt — fall back to
                // an empty schema rather than panicking in a library.
                let schema_value: serde_json::Value = serde_json::from_slice(decl.input_schema)
                    .unwrap_or_else(|_| serde_json::json!({"type": "object", "properties": {}}));
                let schema_obj = match schema_value {
                    serde_json::Value::Object(m) => m,
                    _ => serde_json::Map::new(),
                };
                Tool::new(decl.name, decl.description, Arc::new(schema_obj))
            })
            .collect();

        // Prelude tools (`handshake`) — listed iff a sqlite store is wired
        // AND the replay-challenge wiring flag is on. The latter pins the
        // honest-on-wire posture: while no signed-verb path consumes the
        // persisted nonce, advertising `handshake` would invite clients to
        // accumulate dead rows. Round-1 review #1 / brief §15.
        if crate::prelude_tools::is_enabled(
            self.sqlite_store.is_some(),
            cairn_core::status::wiring::REPLAY_CHALLENGE_WIRED,
        ) {
            for decl in crate::prelude_tools::PRELUDE_TOOLS {
                let schema_value: serde_json::Value = serde_json::from_slice(decl.input_schema)
                    .unwrap_or_else(|_| serde_json::json!({"type": "object", "properties": {}}));
                let schema_obj = match schema_value {
                    serde_json::Value::Object(m) => m,
                    _ => serde_json::Map::new(),
                };
                tools.push(Tool::new(decl.name, decl.description, Arc::new(schema_obj)));
            }
        }

        let ctx = self.auth_context_for(&request_id);
        if self.materialize_graph_request(&ctx).is_ok() {
            for decl in crate::graph_tools::GRAPH_TOOLS {
                let schema_value: serde_json::Value = serde_json::from_slice(
                    crate::graph_tools::schema_of(decl),
                )
                .unwrap_or_else(|_| serde_json::json!({"type": "object", "properties": {}}));
                let schema_obj = match schema_value {
                    serde_json::Value::Object(m) => m,
                    _ => serde_json::Map::new(),
                };
                tools.push(Tool::new(decl.name, decl.description, Arc::new(schema_obj)));
            }
        }

        std::future::ready(Ok(ListToolsResult::with_all_items(tools)))
    }

    /// Dispatch a tool call.
    ///
    /// For `graph.*` tools: uses `materialize_graph_request` to resolve the
    /// store and scope in a single pass, then routes to
    /// [`crate::graph_tools::dispatch`]. Validates that the requested verb
    /// exists in [`TOOLS`] for non-graph tools. When the verb is `"search"`
    /// and a store is wired, dispatches through
    /// [`cairn_core::verbs::search::run`]. All other verbs (or `"search"` with
    /// no store wired) fall back to [`dispatch_stub`].
    fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<CallToolResult, rmcp::ErrorData>> + Send + '_
    {
        let name = request.name.clone();
        let arguments = request.arguments.clone();
        let request_id = context.id.to_string();

        async move {
            // Prelude tool routing (`handshake`) — accept the call iff
            // (a) the tool name is in PRELUDE_TOOLS, AND
            // (b) a sqlite store is wired, AND
            // (c) `REPLAY_CHALLENGE_WIRED` is true (read at dispatch time
            //     so a future PR that flips the flag does not need to
            //     touch this branch).
            // The wired/unwired gate lives inside `prelude_tools::dispatch`
            // so production and direct unit-test entry points share one
            // implementation.
            if crate::prelude_tools::is_prelude_tool(name.as_ref()) {
                return Ok(crate::prelude_tools::dispatch(
                    name.as_ref(),
                    arguments,
                    self.sqlite_store.clone(),
                    cairn_core::status::wiring::REPLAY_CHALLENGE_WIRED,
                )
                .await);
            }

            // Graph tool routing — single-pass resolution, no TOCTOU.
            if crate::graph_tools::GRAPH_TOOLS
                .iter()
                .any(|d| d.name == name.as_ref())
            {
                let ctx = self.auth_context_for(&request_id);
                let Ok(req) = self.materialize_graph_request(&ctx) else {
                    return Ok(capability_unavailable_result(&name));
                };
                let queries = GraphQueries::new(req.store, req.allowed, req.now_ms);
                return Ok(crate::graph_tools::dispatch(&queries, &name, arguments).await);
            }

            let known = TOOLS.iter().any(|d| d.name == name.as_ref());
            let store = self.store.clone();
            let config = self.config.clone();

            if !known {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "cairn: unknown verb '{name}'. Available verbs: {}",
                    TOOLS.iter().map(|d| d.name).collect::<Vec<_>>().join(", ")
                ))]));
            }

            if name.as_ref() == "search"
                && let Some(store) = store
            {
                return Ok(handle_search(store, config, arguments).await);
            }

            Ok(dispatch_stub(&name))
        }
    }
}

/// Dispatch the `search` tool against a wired store.
///
/// Parses the MCP tool arguments into the generated search request args,
/// derives the capability set from config, calls
/// [`cairn_core::verbs::search::run`], and serializes the result into a
/// generated response envelope inside a [`CallToolResult`].
#[allow(
    clippy::too_many_lines,
    reason = "linear arg→request→outcome→envelope flow; splitting reduces clarity"
)]
async fn handle_search(
    store: Arc<dyn MemoryStore>,
    config: CairnConfig,
    arguments: Option<serde_json::Map<String, serde_json::Value>>,
) -> CallToolResult {
    use cairn_core::generated::envelope::{RequestArgs, RequestVerb, ResponseVerb};
    use cairn_core::generated::verbs::search::SearchArgsMode;

    // Parse args from the MCP tool argument map through the generated
    // envelope adapter so parse failures use the canonical Response shape.
    let args = match crate::verb_envelope::parse_args(RequestVerb::Search, arguments) {
        Ok(RequestArgs::Search(args)) => args,
        Ok(_) => {
            let response = crate::verb_envelope::invalid_args_response(
                ResponseVerb::Search,
                "args",
                "expected search arguments",
            );
            return crate::verb_envelope::call_result_from_response(response);
        }
        Err(response) => return crate::verb_envelope::call_result_from_response(response),
    };

    // Map IDL mode to core dispatcher mode.
    let mode = match args.mode {
        SearchArgsMode::Keyword => cairn_core::verbs::search::SearchMode::Keyword,
        SearchArgsMode::Semantic => cairn_core::verbs::search::SearchMode::Semantic,
        SearchArgsMode::Hybrid => cairn_core::verbs::search::SearchMode::Hybrid,
        // Forward-compat: reject unknown future variants fail-closed.
        _ => {
            let response = crate::verb_envelope::invalid_args_response(
                ResponseVerb::Search,
                "mode",
                "unknown search mode",
            );
            return crate::verb_envelope::call_result_from_response(response);
        }
    };

    // Derive the capability set the dispatcher will fail-closed against,
    // masked by the same store-capability signals `status_response` uses.
    // Dispatcher gate ⊆ advertised gate ⊆ status capabilities — three
    // views, one truth.
    // Mirror the same two-factor proxy as `build_status_response`: store
    // vector-index AND provider/model alignment. See that function's comment
    // for the full rationale.
    let store_caps = store.capabilities();
    let provider_model_ok = cairn_core::config::provider_model_aligned(&config);
    let embedding_provider_ready = store_caps.vector && provider_model_ok;
    let mut caps = config.capabilities(embedding_provider_ready);
    caps.keyword_search = caps.keyword_search && store_caps.fts;
    caps.semantic_search = caps.semantic_search && store_caps.vector;
    caps.hybrid_search = caps.hybrid_search && store_caps.fts && store_caps.vector;

    let limit = args.limit.map_or(10, |l| usize::try_from(l).unwrap_or(10));
    // Map the IDL `args.scope` into the dispatcher's auth_scope.
    // Unsupported predicates are dropped with a tracing warn (see
    // `scope_filter_to_tuple`).
    let auth_scope = scope_filter_to_tuple(args.scope.as_ref());
    let request = cairn_core::verbs::search::SearchRequest {
        query: args.query.clone(),
        mode,
        limit,
        visibility_allowlist: vec![],
        auth_scope,
        model_label: config.search.embedding_model.as_str().to_owned(),
        filter: args.filters.clone(),
        explain: args.explain.unwrap_or(false),
    };

    let outcome = match cairn_core::verbs::search::run(store.as_ref(), &config, &caps, request)
        .await
    {
        Ok(o) => o,
        Err(cairn_core::verbs::search::SearchError::CapabilityUnavailable { capability }) => {
            let response = crate::verb_envelope::capability_unavailable_response(
                ResponseVerb::Search,
                capability,
            );
            return crate::verb_envelope::call_result_from_response(response);
        }
        Err(cairn_core::verbs::search::SearchError::InvalidArgs { reason }) => {
            let response =
                crate::verb_envelope::invalid_args_response(ResponseVerb::Search, "args", &reason);
            return crate::verb_envelope::call_result_from_response(response);
        }
        Err(cairn_core::verbs::search::SearchError::InvalidFilter { reason }) => {
            let response =
                crate::verb_envelope::invalid_filter_response(ResponseVerb::Search, &reason);
            return crate::verb_envelope::call_result_from_response(response);
        }
        Err(cairn_core::verbs::search::SearchError::Store(e)) => {
            let response = crate::verb_envelope::aborted_internal(
                ResponseVerb::Search,
                &format!("store error: {e}"),
            );
            return crate::verb_envelope::call_result_from_response(response);
        }
        // Forward-compat: surface unknown error variants as internal errors.
        Err(e) => {
            let response = crate::verb_envelope::aborted_internal(
                ResponseVerb::Search,
                &format!("internal error: {e}"),
            );
            return crate::verb_envelope::call_result_from_response(response);
        }
    };

    search_outcome_to_result(outcome, mode)
}

/// Convert a successful [`cairn_core::verbs::search::SearchOutcome`] into the
/// MCP [`CallToolResult`] shape.
fn search_outcome_to_result(
    outcome: cairn_core::verbs::search::SearchOutcome,
    mode: cairn_core::verbs::search::SearchMode,
) -> CallToolResult {
    use cairn_core::generated::common::Ulid;
    use cairn_core::generated::envelope::{ResponseData, ResponseVerb};
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
                bm25_rank: e.bm25_rank.map(|r| i64::try_from(r).unwrap_or(i64::MAX)),
                semantic_rank: e
                    .semantic_rank
                    .map(|r| i64::try_from(r).unwrap_or(i64::MAX)),
                rrf_score: finite_or_zero(e.rrf_score),
                cosine: finite_option(e.cosine),
                final_score: finite_or_zero(e.final_score),
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

    let response = crate::verb_envelope::committed(
        ResponseVerb::Search,
        ResponseData::Search(data),
        to_wire(&outcome.policy_trace),
    );
    crate::verb_envelope::call_result_from_response(response)
}

/// Stub dispatcher returned while real verb wiring is pending.
///
/// Returns a [`CallToolResult`] with `is_error = true` and a message
/// explaining that the verb is not yet wired. This function is `pub` so the
/// parity test in Task 8 can call it directly.
#[must_use]
pub fn dispatch_stub(verb: &str) -> CallToolResult {
    CallToolResult::error(vec![Content::text(format!(
        "cairn {verb}: not yet implemented in this P0 scaffold. \
         Verb dispatch lands in a follow-up PR; no memory operation was performed."
    ))])
}

/// Mode-appropriate score for an MCP search hit.
///
/// Mirrors the CLI + SDK helpers: hybrid graph-only rows have
/// `bm25 = 0.0`, so emitting that value as the wire score would
/// suppress them in clients that threshold or sort by score.
/// Hybrid prefers the dispatcher's `final_score` from the explain
/// block; fall back to a rank-derived score when explain is absent.
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
    finite_or_zero(raw)
}

/// Replace non-finite floats with zero before writing generated JSON
/// envelopes. JSON Schema numbers cannot represent NaN or infinity.
#[inline]
#[must_use]
fn finite_or_zero(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
}

#[inline]
#[must_use]
fn finite_option(value: Option<f64>) -> Option<f64> {
    value.map(finite_or_zero)
}

/// Convert a domain [`cairn_core::search::DegradedLeg`] into the IDL
/// wire representation. Mirrors the SDK transport's helper — keep in
/// sync with `crates/cairn-idl/schema/verbs/search.json`.
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
        _ => DegradedLegEntryReason::SqlError,
    };
    let source_to_idl = |s: GraphSource| match s {
        GraphSource::AuthKeywordSeed => DegradedLegEntrySource::AuthKeywordSeed,
        GraphSource::AuthSemanticSeed => DegradedLegEntrySource::AuthSemanticSeed,
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
        _ => DegradedLegEntry {
            leg: DegradedLegEntryLeg::Graph,
            reason: DegradedLegEntryReason::SqlError,
            source: Some(DegradedLegEntrySource::All),
        },
    }
}

/// Map an IDL `ScopeFilter` to a domain `ScopeTuple` so MCP callers
/// thread real auth context into the search dispatcher.
///
/// Mirrors the SDK transport's `scope_filter_to_tuple`. Predicates the
/// dispatcher cannot yet honor (`kind`, `tags`, `tier`, `record_ids`)
/// are dropped with a tracing warn rather than rejected — silently
/// ignoring them was the prior behavior and forcing a hard error here
/// breaks already-deployed callers. A future PR threads them into
/// `SearchRequest.filter`.
fn scope_filter_to_tuple(
    sf: Option<&cairn_core::generated::common::ScopeFilter>,
) -> cairn_core::domain::ScopeTuple {
    let Some(sf) = sf else {
        return cairn_core::domain::ScopeTuple::default();
    };
    // `kind`/`tags`/`tier`/`record_ids` fall through silently — the
    // pre-change MCP handler ignored every scope predicate, so dropping
    // only the un-honored subset preserves wire compatibility. A future
    // PR threads them into `SearchRequest.filter`.
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

#[cfg(test)]
mod tests_plan_a {
    use super::*;
    use cairn_core::config::CairnConfig;
    use cairn_core::domain::ScopeTuple;
    use cairn_core::mcp_auth::{ConfigBackedScope, McpSessionScope};
    use cairn_test_fixtures::FixtureStore;
    use std::sync::Arc;

    fn principal() -> ScopeTuple {
        ScopeTuple {
            tenant: Some("acme".into()),
            ..ScopeTuple::default()
        }
    }

    #[test]
    fn handler_with_store_carries_scope_and_principal() {
        let store: Arc<dyn cairn_core::contract::memory_store::MemoryStore> =
            Arc::new(FixtureStore::default());
        let scope: Arc<dyn McpSessionScope> = Arc::new(ConfigBackedScope::new(principal()));
        let cfg = CairnConfig::default();
        let handler = CairnMcpHandler::with_store_and_scope(store, scope, cfg, principal());
        assert!(handler.has_store());
        assert!(handler.has_scope());
        assert_eq!(handler.principal().tenant.as_deref(), Some("acme"));
    }

    #[test]
    fn manifest_without_graph_tools_in_plan_a() {
        let store: Arc<dyn cairn_core::contract::memory_store::MemoryStore> =
            Arc::new(FixtureStore::default());
        let scope: Arc<dyn McpSessionScope> = Arc::new(ConfigBackedScope::new(principal()));
        let mut cfg = CairnConfig::default();
        cfg.mcp.stdio.single_tenant = true;
        cfg.mcp.stdio.principal = Some(principal());
        let handler = CairnMcpHandler::with_store_and_scope(store, scope, cfg, principal());
        let listed = handler.listed_tool_names();
        assert_eq!(
            listed.len(),
            crate::generated::TOOLS.len(),
            "Plan A: no graph tools added to manifest"
        );
        for tool in listed {
            assert!(
                !tool.starts_with("graph."),
                "Plan A must not list graph.* tools, got `{tool}`"
            );
        }
    }

    /// Round-2 review #3: status advertisement and `handshake` tool
    /// exposure must agree on `replay.{sequence,challenge}`. A handler
    /// constructed without a sqlite store cannot mint or redeem
    /// challenges, so the status block MUST NOT advertise either
    /// replay capability — even when (in the future) the wiring const
    /// flips to `true`.
    #[test]
    fn replay_capabilities_filtered_when_no_sqlite_store() {
        let store: Arc<dyn cairn_core::contract::memory_store::MemoryStore> =
            Arc::new(FixtureStore::default());
        let scope: Arc<dyn McpSessionScope> = Arc::new(ConfigBackedScope::new(principal()));
        let mut cfg = CairnConfig::default();
        cfg.mcp.stdio.single_tenant = true;
        cfg.mcp.stdio.principal = Some(principal());
        // `with_store_and_scope` deliberately does NOT wire a sqlite handle,
        // so the post-filter inside `build_status_response` must drop both
        // replay capabilities. The check holds regardless of the current
        // value of `REPLAY_CHALLENGE_WIRED` / `REPLAY_SEQUENCE_WIRED`.
        let handler = CairnMcpHandler::with_store_and_scope(store, scope, cfg, principal());
        let status = handler.status_response();
        for cap in &status.capabilities {
            assert!(
                !matches!(
                    cap,
                    cairn_core::generated::common::Capabilities::CairnMcpV1ReplayChallenge
                        | cairn_core::generated::common::Capabilities::CairnMcpV1ReplaySequence
                ),
                "replay.{{sequence,challenge}} must not appear in status without a sqlite store; got: {cap:?}"
            );
        }
    }

    #[test]
    fn forget_record_capability_filtered_until_mcp_dispatch_is_wired() {
        let store: Arc<dyn cairn_core::contract::memory_store::MemoryStore> =
            Arc::new(FixtureStore::default());
        let scope: Arc<dyn McpSessionScope> = Arc::new(ConfigBackedScope::new(principal()));
        let mut cfg = CairnConfig::default();
        cfg.mcp.stdio.single_tenant = true;
        cfg.mcp.stdio.principal = Some(principal());
        let handler = CairnMcpHandler::with_store_and_scope(store, scope, cfg, principal());
        let status = handler.status_response();
        assert!(
            !status
                .capabilities
                .contains(&cairn_core::generated::common::Capabilities::CairnMcpV1ForgetRecord),
            "MCP status must not advertise forget.record until MCP dispatch is wired"
        );
    }
}
