//! Graph tool registry and dispatch for the Cairn MCP adapter (§4.1, §4.2).
//!
//! Exposes [`GRAPH_TOOLS`], a static slice of [`GraphToolDecl`] entries that
//! describe the five graph tools, and [`dispatch`], which routes a tool-call
//! by name to the appropriate [`GraphQueries`] method.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// A statically-declared graph tool entry: name, description, and a lazily
/// initialised JSON-schema bytes slice (serialised schemars output).
pub struct GraphToolDecl {
    /// MCP tool name, e.g. `"graph.query"`.
    pub name: &'static str,
    /// Human-readable description sent to the host in `tools/list`.
    pub description: &'static str,
    /// Lazily-populated schema bytes (initialised on first call to
    /// [`schema_of`]).
    pub schema: &'static OnceLock<Vec<u8>>,
    /// Zero-argument closure that produces the JSON-schema bytes.
    pub schema_for: fn() -> Vec<u8>,
}

fn schema_bytes<T: JsonSchema>() -> Vec<u8> {
    let s = schemars::schema_for!(T);
    serde_json::to_vec(&s).unwrap_or_default()
}

// ----- per-tool input types -------------------------------------------------

/// Input for `graph.get_entity`: look up by `id` **or** by `name`; both
/// fields may not be present simultaneously (enforced by `deny_unknown_fields`
/// on each variant).
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
pub enum GetEntityArgs {
    /// Look up an entity by its canonical id.
    ById {
        /// Canonical entity id.
        id: String,
    },
    /// Look up an entity by its display name.
    ByName {
        /// Display name to search for.
        name: String,
    },
}

/// Input for `graph.get_neighbors`.
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetNeighborsArgs {
    /// Source/target entity id.
    pub id: String,
    /// Optional relation filter.
    #[serde(default)]
    pub relation: Option<String>,
    /// Optional minimum confidence threshold.
    #[serde(default)]
    pub min_confidence: Option<f64>,
}

/// Input for `graph.query` (BFS or DFS traversal).
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueryGraphArgs {
    /// Seed entity id to start traversal from.
    pub seed: String,
    /// Maximum hop depth (capped at 5 in dispatch).
    #[serde(default = "default_max_hops")]
    pub max_hops: u32,
    /// Maximum number of nodes to visit.
    #[serde(default = "default_node_budget")]
    pub node_budget: u32,
    /// Approximate token budget for serialised edges.
    #[serde(default = "default_token_budget")]
    pub token_budget: u32,
    /// Traversal mode: `"bfs"` (default) or `"dfs"`.
    #[serde(default)]
    pub mode: TraversalMode,
}

fn default_max_hops() -> u32 {
    3
}
fn default_node_budget() -> u32 {
    64
}
fn default_token_budget() -> u32 {
    8192
}

// Hard caps applied at dispatch (not just defaults). A caller can request a
// smaller value but cannot exceed these, so an oversized request cannot turn
// into an oversized response or an over-allocating traversal — closes the
// `graph.query` amplification path the round-3 review flagged.
/// Maximum traversal depth a `graph.query` request can ask for.
pub const MAX_HOPS_CAP: u32 = 5;
/// Maximum number of nodes the BFS/DFS driver is allowed to materialize.
pub const NODE_BUDGET_CAP: usize = 256;
/// Maximum serialized response budget enforced across the full payload.
pub const TOKEN_BUDGET_CAP: usize = 64 * 1024;
/// Maximum number of `SurpriseHit` rows the store may return.
pub const SURPRISE_LIMIT_CAP: usize = 64;
/// Maximum number of input ids `graph.surprising_connections` will consider.
pub const SURPRISE_INPUT_CAP: usize = 256;

/// Traversal strategy for `graph.query`.
#[derive(Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TraversalMode {
    /// Breadth-first search (default).
    #[default]
    Bfs,
    /// Depth-first search.
    Dfs,
}

/// Input for `graph.timeline`.
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TimelineArgs {
    /// Entity id whose timeline to fetch.
    pub id: String,
    /// Include historical (superseded) edges.
    #[serde(default)]
    pub include_history: bool,
    /// Include expired/tombstoned edges.
    #[serde(default)]
    pub include_expired: bool,
}

/// Input for `graph.surprising_connections`.
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SurprisingArgs {
    /// Entity ids to score connections between.
    pub ids: Vec<String>,
    /// Maximum number of hits to return.
    #[serde(default = "default_surprise_limit")]
    pub limit: u32,
}

fn default_surprise_limit() -> u32 {
    16
}

// ----- registry -------------------------------------------------------------

macro_rules! tool {
    ($name:literal, $desc:literal, $args:ty) => {{
        static SCHEMA: OnceLock<Vec<u8>> = OnceLock::new();
        GraphToolDecl {
            name: $name,
            description: $desc,
            schema: &SCHEMA,
            schema_for: || schema_bytes::<$args>(),
        }
    }};
}

/// Static registry of all five graph tools.
pub static GRAPH_TOOLS: &[GraphToolDecl] = &[
    tool!(
        "graph.query",
        "BFS/DFS from a seed entity within hop and token budget. \
         Discovered nodes are returned id-only to avoid cross-scope name leaks.",
        QueryGraphArgs
    ),
    tool!(
        "graph.get_entity",
        "Look up an entity by id (returns {id, edge_count}) or name \
         (returns {id, echoed_name, edge_count}).",
        GetEntityArgs
    ),
    tool!(
        "graph.get_neighbors",
        "Return one-hop in-scope edges incident to the given id. Optional \
         relation and min_confidence filters.",
        GetNeighborsArgs
    ),
    tool!(
        "graph.timeline",
        "Return all currently-visible edges for an entity ordered by valid_at. \
         include_history and include_expired flags opt into broader windows; \
         scope filter is unconditional.",
        TimelineArgs
    ),
    tool!(
        "graph.surprising_connections",
        "Score in-scope edges between input entities; cross-scope (relative \
         to the modal records.scope) gets a 2× confidence bonus.",
        SurprisingArgs
    ),
];

/// Return (and lazily initialise) the JSON-schema bytes for a [`GraphToolDecl`].
#[must_use]
pub fn schema_of(decl: &GraphToolDecl) -> &'static [u8] {
    decl.schema.get_or_init(|| (decl.schema_for)())
}

// ----- dispatch -------------------------------------------------------------

use cairn_store_sqlite::entity_graph::queries::GraphQueries;
use rmcp::model::{CallToolResult, Content};
use serde_json::{Map, Value};

/// Route an MCP `tools/call` request to the appropriate [`GraphQueries`]
/// method, deserialise the arguments, await the async query, and serialise
/// the result into a [`CallToolResult`].
///
/// # Errors
///
/// This function never returns a Rust error. All failure modes (unknown tool,
/// invalid arguments, store errors) are returned as
/// `CallToolResult { is_error: Some(true), ... }`.
pub async fn dispatch(
    queries: &GraphQueries,
    name: &str,
    arguments: Option<Map<String, Value>>,
) -> CallToolResult {
    let args = arguments.unwrap_or_default();
    let result: Result<Value, String> = match name {
        "graph.get_entity" => match serde_json::from_value::<GetEntityArgs>(Value::Object(args)) {
            Ok(GetEntityArgs::ById { id }) => queries
                .get_entity_by_id(id)
                .await
                .map_err(|e| e.to_string())
                .and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string())),
            Ok(GetEntityArgs::ByName { name }) => queries
                .get_entity_by_name(name)
                .await
                .map_err(|e| e.to_string())
                .and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string())),
            Err(e) => Err(format!("invalid args: {e}")),
        },
        "graph.get_neighbors" => {
            match serde_json::from_value::<GetNeighborsArgs>(Value::Object(args)) {
                Ok(a) => queries
                    .get_neighbors(a.id, a.relation, a.min_confidence)
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string())),
                Err(e) => Err(format!("invalid args: {e}")),
            }
        }
        "graph.query" => match serde_json::from_value::<QueryGraphArgs>(Value::Object(args)) {
            Ok(a) => {
                let max_hops = a.max_hops.min(MAX_HOPS_CAP);
                // Hard cap node_budget at the dispatch boundary so a caller
                // cannot drive the server to allocate an arbitrarily large
                // traversal: the graph driver itself trusts its `node_budget`
                // input to be already-clamped, so the cap belongs here.
                let node_budget =
                    usize::try_from(a.node_budget).unwrap_or(usize::MAX).min(NODE_BUDGET_CAP);
                let token_budget = usize::try_from(a.token_budget)
                    .unwrap_or(usize::MAX)
                    .min(TOKEN_BUDGET_CAP);
                let res = match a.mode {
                    TraversalMode::Bfs => queries.query_bfs(a.seed, max_hops, node_budget).await,
                    TraversalMode::Dfs => queries.query_dfs(a.seed, max_hops, node_budget).await,
                };
                res.map_err(|e| e.to_string()).and_then(|sg| {
                    // Apply the byte budget across the **full payload**, not
                    // edges alone — otherwise a caller can DoS by requesting a
                    // small token_budget but a large node_budget. We trim
                    // nodes first (they are id-only and cheap), then split the
                    // remaining budget for edges.
                    let nodes_serialized: usize = sg
                        .nodes
                        .iter()
                        .filter_map(|n| serde_json::to_string(n).ok())
                        .map(|s| s.len() + 1)
                        .sum::<usize>()
                        .saturating_add(2); // "[]"
                    let trimmed_nodes =
                        GraphQueries::truncate_to_token_budget(&sg.nodes, token_budget);
                    let nodes_used = trimmed_nodes
                        .iter()
                        .filter_map(|n| serde_json::to_string(n).ok())
                        .map(|s| s.len() + 1)
                        .sum::<usize>()
                        .saturating_add(2);
                    let remaining = token_budget.saturating_sub(nodes_used);
                    let trimmed_edges =
                        GraphQueries::truncate_to_token_budget(&sg.edges, remaining);
                    let _ = nodes_serialized; // kept for future telemetry
                    serde_json::to_value(serde_json::json!({
                        "nodes": trimmed_nodes,
                        "edges": trimmed_edges,
                    }))
                    .map_err(|e| e.to_string())
                })
            }
            Err(e) => Err(format!("invalid args: {e}")),
        },
        "graph.timeline" => match serde_json::from_value::<TimelineArgs>(Value::Object(args)) {
            Ok(a) => queries
                .timeline(a.id, a.include_history, a.include_expired)
                .await
                .map_err(|e| e.to_string())
                .and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string())),
            Err(e) => Err(format!("invalid args: {e}")),
        },
        "graph.surprising_connections" => {
            match serde_json::from_value::<SurprisingArgs>(Value::Object(args)) {
                Ok(a) => {
                    // Server-side caps: a hostile or accidental large `limit`
                    // would otherwise force the store to materialize an
                    // unbounded result Vec; an oversized `ids` list bloats the
                    // SQL placeholder expansion and the JSON inputs to
                    // `json_each(?)`. Clamp both before they reach the store.
                    let limit = usize::try_from(a.limit)
                        .unwrap_or(usize::MAX)
                        .min(SURPRISE_LIMIT_CAP);
                    let mut ids = a.ids;
                    if ids.len() > SURPRISE_INPUT_CAP {
                        ids.truncate(SURPRISE_INPUT_CAP);
                    }
                    queries
                        .surprising_connections(ids, limit)
                        .await
                        .map_err(|e| e.to_string())
                        .and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string()))
                }
                Err(e) => Err(format!("invalid args: {e}")),
            }
        }
        _ => Err(format!("unknown graph tool: {name}")),
    };

    match result {
        Ok(v) => CallToolResult::structured(v),
        Err(e) => CallToolResult::error(vec![Content::text(e)]),
    }
}
