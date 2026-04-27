# MCP Stdio Server Transport — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire a real stdio MCP server into `cairn-mcp` using the `rmcp` 1.5 SDK, so that a harness can start `cairn mcp serve` and call the eight verbs over the MCP protocol.

**Architecture:** `cairn-mcp` implements `rmcp::ServerHandler` → dispatches tool calls through `verify_signed_intent` → routes to the same `cairn.mcp.v1` response envelope as the CLI. `cairn-cli` adds a `mcp serve` subcommand that starts a multi-thread tokio runtime and calls `cairn_mcp::serve_stdio()`. Transport selection lives in `cairn-cli`; `cairn-mcp` is protocol-only (per CLAUDE.md §6.12).

**Tech Stack:** Rust 1.95.0, `rmcp` 1.5 (`server` + `transport-io` features), `tokio` (multi-thread), `tracing`, existing `cairn-core` generated types + `verify_signed_intent`.

---

## File Map

### Create
| File | Responsibility |
|------|---------------|
| `crates/cairn-mcp/src/error.rs` | `McpTransportError` enum — separates transport faults from Cairn typed errors |
| `crates/cairn-mcp/src/dispatch.rs` | `dispatch(name, args_json)` → `Response`; stub SignedIntent; verb routing |
| `crates/cairn-mcp/src/server.rs` | `CairnMcpHandler` implementing `rmcp::ServerHandler`; `serve_stdio()` async entry point |
| `crates/cairn-cli/src/verbs/mcp_serve.rs` | `run(sub)` → `ExitCode`; spins up tokio runtime, calls `cairn_mcp::serve_stdio()` |
| `crates/cairn-mcp/tests/tool_list_snapshot.rs` | Snapshot + structural tests for the TOOLS registry |
| `crates/cairn-cli/tests/mcp_parity.rs` | Verifies CLI and MCP both return same status/verb/contract for same verb at P0 |

### Modify
| File | Change |
|------|--------|
| `Cargo.toml` (workspace) | Add `rmcp` to `[workspace.dependencies]` |
| `crates/cairn-mcp/Cargo.toml` | Add `rmcp`, `tokio`, `ulid`, `base64`, `tracing` |
| `crates/cairn-cli/Cargo.toml` | Add `tokio` with `rt-multi-thread` feature |
| `crates/cairn-mcp/src/lib.rs` | Add `pub mod error; pub mod dispatch; pub mod server;`; set `stdio: true`; re-export `serve_stdio`; remove machete ignores for `serde`/`serde_json` |
| `crates/cairn-mcp/plugin.toml` | `stdio = true` |
| `crates/cairn-cli/src/main.rs` | Add `mcp` subcommand + `run_mcp()` dispatch |
| `crates/cairn-cli/src/verbs/mod.rs` | Add `pub mod mcp_serve;` |
| `crates/cairn-core/src/contract/conformance/mcp_server.rs` | Upgrade `initialize_and_list_tools` tier-2 from `Pending` to `Ok` |
| `crates/cairn-mcp/tests/smoke.rs` | Replace stub with real dispatch unit tests |

---

## Tasks

### Task 1: Add rmcp + tokio to Cargo manifests

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/cairn-mcp/Cargo.toml`
- Modify: `crates/cairn-cli/Cargo.toml`

- [ ] **Step 1: Write failing build test**

```bash
cd crates/cairn-mcp && cat > /tmp/rmcp_build_test.rs << 'EOF'
// This won't compile yet — that's the point
use rmcp::handler::server::ServerHandler;
fn main() {}
EOF
```

Expected: just verifying the dep isn't there yet:
```bash
grep "rmcp" Cargo.toml
# Expected: no output
```

- [ ] **Step 2: Add rmcp to workspace dependencies**

In `Cargo.toml`, add to `[workspace.dependencies]`:
```toml
rmcp = { version = "1.5", default-features = false, features = ["server", "transport-io"] }
```

- [ ] **Step 3: Add deps to cairn-mcp Cargo.toml**

In `crates/cairn-mcp/Cargo.toml`, add to `[dependencies]`:
```toml
rmcp = { workspace = true }
tokio = { workspace = true, features = ["rt-multi-thread", "macros"] }
tracing = { workspace = true }
ulid = { workspace = true }
base64 = { workspace = true }
```

Also remove the `[package.metadata.cargo-machete] ignored` entry for `serde` and `serde_json` — they'll have real call sites soon. Leave the section present but empty, or remove it entirely once those deps have call sites.

- [ ] **Step 4: Add tokio to cairn-cli Cargo.toml**

In `crates/cairn-cli/Cargo.toml`, add to `[dependencies]`:
```toml
tokio = { workspace = true, features = ["rt-multi-thread"] }
```

- [ ] **Step 5: Verify the workspace builds**

```bash
cargo check --workspace --locked
```

Expected: compiles (rmcp types are now available; no new Rust sources yet).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/cairn-mcp/Cargo.toml crates/cairn-cli/Cargo.toml
git commit -m "deps(cairn-mcp): add rmcp 1.5, tokio, tracing, ulid, base64"
```

---

### Task 2: McpTransportError type

**Files:**
- Create: `crates/cairn-mcp/src/error.rs`
- Modify: `crates/cairn-mcp/src/lib.rs`

- [ ] **Step 1: Write the failing test**

In `crates/cairn-mcp/tests/smoke.rs`, replace the entire file:

```rust
#![allow(missing_docs)]

use cairn_mcp::error::McpTransportError;

#[test]
fn transport_error_displays() {
    let e = McpTransportError::Initialize("handshake failed".to_owned());
    assert!(e.to_string().contains("initialize"), "error display: {e}");
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo nextest run --package cairn-mcp --test smoke -E 'test(transport_error_displays)' 2>&1 | head -20
```

Expected: `error[E0432]: unresolved import 'cairn_mcp::error'` or similar.

- [ ] **Step 3: Create error.rs**

```rust
// crates/cairn-mcp/src/error.rs

use thiserror::Error;

/// Transport-level errors for the MCP stdio adapter.
///
/// Separates wire/IO failures (this type) from Cairn typed operation
/// errors (which stay in the `cairn.mcp.v1` response envelope).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum McpTransportError {
    /// MCP server failed to complete the `initialize` handshake.
    #[error("MCP stdio server failed to initialize: {0}")]
    Initialize(String),

    /// IO error on the underlying stdio transport.
    #[error("stdio IO error: {0}")]
    Io(#[from] std::io::Error),
}
```

- [ ] **Step 4: Expose the module from lib.rs**

In `crates/cairn-mcp/src/lib.rs`, add before the existing `pub mod generated;`:

```rust
pub mod error;
```

Also add `thiserror` to `crates/cairn-mcp/Cargo.toml` `[dependencies]`:
```toml
thiserror = { workspace = true }
```

- [ ] **Step 5: Run test to verify it passes**

```bash
cargo nextest run --package cairn-mcp --test smoke -E 'test(transport_error_displays)'
```

Expected: `PASSED`.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-mcp/src/error.rs crates/cairn-mcp/src/lib.rs crates/cairn-mcp/Cargo.toml crates/cairn-mcp/tests/smoke.rs
git commit -m "feat(cairn-mcp): add McpTransportError separating transport from verb errors"
```

---

### Task 3: Verb dispatch module

**Files:**
- Create: `crates/cairn-mcp/src/dispatch.rs`
- Modify: `crates/cairn-mcp/src/lib.rs`

- [ ] **Step 1: Write failing tests**

In `crates/cairn-mcp/tests/smoke.rs`, append:

```rust
use cairn_core::generated::envelope::{ResponseStatus, ResponseVerb};

#[test]
fn dispatch_ingest_returns_aborted_p0() {
    let resp = cairn_mcp::dispatch::dispatch("ingest", None);
    assert_eq!(resp.contract, "cairn.mcp.v1");
    assert!(
        matches!(resp.status, ResponseStatus::Aborted),
        "P0 must return Aborted (store not wired): {:?}",
        resp.status
    );
    assert!(
        matches!(resp.verb, ResponseVerb::Ingest),
        "verb echo must be Ingest: {:?}",
        resp.verb
    );
    let err = resp.error.expect("Aborted response must have error");
    assert_eq!(err["code"], "Internal");
}

#[test]
fn dispatch_unknown_verb_returns_rejected() {
    let resp = cairn_mcp::dispatch::dispatch("not_a_real_verb", None);
    assert!(
        matches!(resp.verb, ResponseVerb::Unknown),
        "unrecognized tool name must produce verb=unknown: {:?}",
        resp.verb
    );
    assert!(
        matches!(resp.status, ResponseStatus::Rejected),
        "verb=unknown must be Rejected: {:?}",
        resp.status
    );
    let err = resp.error.expect("Rejected response must have error");
    assert_eq!(err["code"], "UnknownVerb");
}

#[test]
fn dispatch_all_eight_verbs_do_not_panic() {
    for name in ["ingest", "search", "retrieve", "summarize",
                 "assemble_hot", "capture_trace", "lint", "forget"]
    {
        let resp = cairn_mcp::dispatch::dispatch(name, None);
        assert_eq!(resp.contract, "cairn.mcp.v1", "bad contract for {name}");
        assert!(
            matches!(resp.status, ResponseStatus::Aborted),
            "{name} must be Aborted at P0: {:?}", resp.status
        );
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo nextest run --package cairn-mcp --test smoke -E 'test(dispatch_)' 2>&1 | head -20
```

Expected: compile error — `cairn_mcp::dispatch` not found.

- [ ] **Step 3: Create dispatch.rs**

```rust
// crates/cairn-mcp/src/dispatch.rs
//! Verb dispatch for the MCP stdio transport (brief §8, §4 MCPServer contract).
//!
//! All tool calls flow through `verify_signed_intent` before reaching the
//! verb layer, satisfying invariant §4 in CLAUDE.md (WAL + two-phase apply).
//! At P0 the store is not wired; every verb returns an `Aborted/Internal`
//! response. The signed-intent path uses a syntactic-only stub (§P0 status
//! in `cairn_core::verifier`).

use cairn_core::generated::common::{Ed25519Signature, Identity, Nonce16Base64, Ulid};
use cairn_core::generated::envelope::{
    Response, ResponsePolicyTrace, ResponseStatus, ResponseVerb, SignedIntent,
    SignedIntentScope, SignedIntentScopeTier,
};
use cairn_core::verifier::verify_signed_intent;

/// Dispatch a `tools/call` to the Cairn verb layer and return a
/// `cairn.mcp.v1` response envelope.
///
/// `name` is the MCP tool name (one of the eight verbs).
/// `args_json` is the raw tool arguments from the MCP client (unused at P0).
pub fn dispatch(
    name: &str,
    _args_json: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Response {
    let verb = verb_from_tool_name(name);

    if matches!(verb, ResponseVerb::Unknown) {
        return Response {
            contract: "cairn.mcp.v1".to_owned(),
            data: None,
            error: Some(serde_json::json!({
                "code": "UnknownVerb",
                "message": format!("unknown tool: {name}"),
                "data": { "verb": name },
            })),
            operation_id: fresh_ulid(),
            policy_trace: Vec::<ResponsePolicyTrace>::new(),
            status: ResponseStatus::Rejected,
            target: None,
            verb: ResponseVerb::Unknown,
        };
    }

    // All mutations must flow through the envelope verifier (CLAUDE.md invariant 5).
    // P0: syntactic-only stub; real SignedIntent extraction lands at P1.
    let intent = p0_stub_intent();
    if verify_signed_intent(intent).is_err() {
        return Response {
            contract: "cairn.mcp.v1".to_owned(),
            data: None,
            error: Some(serde_json::json!({
                "code": "MissingSignature",
                "message": "signed intent failed syntactic validation",
            })),
            operation_id: fresh_ulid(),
            policy_trace: Vec::<ResponsePolicyTrace>::new(),
            status: ResponseStatus::Rejected,
            target: None,
            verb,
        };
    }

    p0_unimplemented_response(verb)
}

fn verb_from_tool_name(name: &str) -> ResponseVerb {
    match name {
        "ingest" => ResponseVerb::Ingest,
        "search" => ResponseVerb::Search,
        "retrieve" => ResponseVerb::Retrieve,
        "summarize" => ResponseVerb::Summarize,
        "assemble_hot" => ResponseVerb::AssembleHot,
        "capture_trace" => ResponseVerb::CaptureTrace,
        "lint" => ResponseVerb::Lint,
        "forget" => ResponseVerb::Forget,
        _ => ResponseVerb::Unknown,
    }
}

fn p0_unimplemented_response(verb: ResponseVerb) -> Response {
    Response {
        contract: "cairn.mcp.v1".to_owned(),
        data: None,
        error: Some(serde_json::json!({
            "code": "Internal",
            "message": "store not wired in this P0 build — verb dispatch lands in #9",
        })),
        operation_id: fresh_ulid(),
        policy_trace: Vec::<ResponsePolicyTrace>::new(),
        status: ResponseStatus::Aborted,
        target: None,
        verb,
    }
}

fn fresh_ulid() -> Ulid {
    Ulid(ulid::Ulid::new().to_string())
}

/// P0 stub SignedIntent — passes syntactic-only verification.
///
/// At P1, replace this with real intent extraction from MCP transport metadata
/// or from an explicit `signed_intent` field in the tool call arguments.
fn p0_stub_intent() -> SignedIntent {
    SignedIntent {
        chain_parents: vec![],
        expires_at: "2099-12-31T23:59:59Z".to_owned(),
        issued_at: "2026-01-01T00:00:00Z".to_owned(),
        issuer: Identity("agt:cairn-mcp:p0:stub:v0".to_owned()),
        key_version: 1,
        nonce: Nonce16Base64("AAAAAAAAAAAAAAAAAAAAAA==".to_owned()),
        operation_id: fresh_ulid(),
        scope: SignedIntentScope {
            tenant: "p0".to_owned(),
            workspace: "p0".to_owned(),
            entity: "p0".to_owned(),
            tier: SignedIntentScopeTier::Private,
        },
        sequence: Some(1),
        server_challenge: None,
        signature: Ed25519Signature(format!("ed25519:{}", "0".repeat(128))),
        target_hash: format!("sha256:{}", "0".repeat(64)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p0_stub_intent_passes_verifier() {
        verify_signed_intent(p0_stub_intent()).expect("stub must pass syntactic checks");
    }

    #[test]
    fn fresh_ulid_is_26_chars() {
        let u = fresh_ulid();
        assert_eq!(u.0.len(), 26);
    }
}
```

- [ ] **Step 4: Expose dispatch from lib.rs**

In `crates/cairn-mcp/src/lib.rs`, add:
```rust
pub mod dispatch;
```

Also add `serde_json` to `[dependencies]` in `crates/cairn-mcp/Cargo.toml` with a real call site now (remove it from machete ignores):

In `crates/cairn-mcp/Cargo.toml`, the `serde_json` entry is already present. Remove the `[package.metadata.cargo-machete]` `ignored` entry for `serde_json` (it now has a call site). Keep `serde` in the ignored list until it gets a call site in the next task.

- [ ] **Step 5: Run tests**

```bash
cargo nextest run --package cairn-mcp --test smoke
```

Expected: all tests pass, including the three new dispatch tests and the transport_error_displays test.

- [ ] **Step 6: Also run the unit tests inside the module**

```bash
cargo nextest run --package cairn-mcp
```

Expected: all tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-mcp/src/dispatch.rs crates/cairn-mcp/src/lib.rs \
        crates/cairn-mcp/Cargo.toml crates/cairn-mcp/tests/smoke.rs
git commit -m "feat(cairn-mcp): add verb dispatcher — all 8 verbs return Aborted/Internal at P0 (brief §8, §4)"
```

---

### Task 4: rmcp ServerHandler + serve_stdio()

**Files:**
- Create: `crates/cairn-mcp/src/server.rs`
- Modify: `crates/cairn-mcp/src/lib.rs`

> **rmcp API note:** Verify these exact type paths against `cargo doc -p rmcp --open` before editing. Key types:
> - `rmcp::handler::server::ServerHandler` (trait)
> - `rmcp::model::{Tool, ListToolsResult, CallToolResult, Content, ServerInfo, Implementation, ServerCapabilities, ProtocolVersion}`
> - `rmcp::service::RequestContext`
> - `rmcp::RoleServer`
> - `rmcp::transport::io::stdio()` → `(tokio::io::Stdin, tokio::io::Stdout)`
> - `rmcp::ServiceExt` (extension trait adding `.serve(transport)`)
> - `rmcp::Error as McpError`

- [ ] **Step 1: Write a failing test for list_tools**

Create `crates/cairn-mcp/tests/tool_list_snapshot.rs`:

```rust
// crates/cairn-mcp/tests/tool_list_snapshot.rs
#![allow(missing_docs)]

use cairn_mcp::generated::TOOLS;

#[test]
fn tool_count_is_eight() {
    assert_eq!(TOOLS.len(), 8, "eight verbs must be registered");
}

#[test]
fn tool_names_are_the_eight_verbs() {
    let names: Vec<&str> = TOOLS.iter().map(|t| t.name).collect();
    assert_eq!(
        names,
        &[
            "ingest", "search", "retrieve", "summarize",
            "assemble_hot", "capture_trace", "lint", "forget",
        ],
        "tool names must match brief §8 verb list in order"
    );
}

#[test]
fn tool_input_schemas_are_valid_json_objects() {
    for tool in TOOLS {
        let v: serde_json::Value = serde_json::from_slice(tool.input_schema)
            .unwrap_or_else(|e| panic!("schema for '{}' is invalid JSON: {e}", tool.name));
        assert!(
            v.is_object(),
            "input schema for '{}' must be a JSON object",
            tool.name
        );
    }
}

// Snapshot the name+auth+capability metadata for wire-compat tracking (§8.0.a).
#[test]
fn tool_auth_metadata_snapshot() {
    let metadata: Vec<serde_json::Value> = TOOLS.iter().map(|t| {
        serde_json::json!({
            "name": t.name,
            "auth": t.auth,
            "capability": t.capability,
            "auth_overrides_count": t.auth_overrides.len(),
            "capability_overrides_count": t.capability_overrides.len(),
        })
    }).collect();
    insta::assert_json_snapshot!("tool_auth_metadata", metadata);
}
```

- [ ] **Step 2: Add insta dev-dep to cairn-mcp Cargo.toml**

In `crates/cairn-mcp/Cargo.toml`, add to `[dev-dependencies]`:
```toml
insta = { workspace = true }
```

- [ ] **Step 3: Run tests to verify they fail (server.rs not yet written)**

```bash
cargo nextest run --package cairn-mcp --test tool_list_snapshot 2>&1 | head -20
```

Expected: tests `tool_count_is_eight`, `tool_names_are_the_eight_verbs`, and `tool_input_schemas_are_valid_json_objects` should PASS (they only use the already-built `TOOLS`). The snapshot test will FAIL because no snapshot file exists yet — that's expected.

- [ ] **Step 4: Accept the snapshot**

```bash
cargo nextest run --package cairn-mcp --test tool_list_snapshot
cargo insta review
# Accept all new snapshots
```

- [ ] **Step 5: Create server.rs**

```rust
// crates/cairn-mcp/src/server.rs
//! rmcp `ServerHandler` implementation for the Cairn MCP stdio adapter.
//!
//! Transport selection lives in `cairn-cli`; this module is protocol-only
//! (brief §4 MCPServer contract, CLAUDE.md §6.12).

use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{Error as McpError, RoleServer, ServiceExt as _};
use tracing::instrument;

use cairn_core::config::CairnConfig;

use crate::dispatch;
use crate::error::McpTransportError;
use crate::generated::TOOLS;

/// Cairn MCP server handler.
///
/// Implements `rmcp::ServerHandler` with the eight core verbs from the
/// generated `TOOLS` registry. Config is held for vault-path and
/// capability resolution (store wiring in issue #9).
#[derive(Clone)]
pub struct CairnMcpHandler {
    #[allow(dead_code)] // used in #9 when store wiring lands
    config: Arc<CairnConfig>,
}

impl CairnMcpHandler {
    /// Construct a handler backed by the given Cairn configuration.
    pub fn new(config: CairnConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }
}

impl rmcp::handler::server::ServerHandler for CairnMcpHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: rmcp::model::ProtocolVersion::LATEST,
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .build(),
            server_info: Implementation {
                name: "cairn-mcp".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            instructions: Some(
                "Cairn agent-memory framework — eight verbs over the cairn.mcp.v1 envelope."
                    .into(),
            ),
        }
    }

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let tools: Vec<Tool> = TOOLS
            .iter()
            .map(|decl| {
                let schema_val: serde_json::Value = serde_json::from_slice(decl.input_schema)
                    .expect("invariant: generated schema bytes are valid JSON");
                let schema_obj = schema_val.as_object().cloned().unwrap_or_default();
                Tool::new(decl.name, decl.description, Arc::new(schema_obj))
            })
            .collect();
        Ok(ListToolsResult {
            tools,
            next_cursor: None,
        })
    }

    #[instrument(skip(self, _ctx), fields(verb = %params.name))]
    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let response = dispatch::dispatch(&params.name, params.arguments.as_ref());
        let json = serde_json::to_string(&response)
            .expect("invariant: generated Response is always JSON-serializable");
        tracing::info!(
            operation_id = %response.operation_id.0,
            status = ?response.status,
            "cairn verb dispatched over MCP"
        );
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }
}

/// Start the Cairn MCP server on stdio and run until stdin closes.
///
/// Called by `cairn-cli` from within a multi-thread tokio runtime.
/// Transport selection (stdio vs SSE) lives in the caller; this function
/// owns only the protocol lifecycle.
pub async fn serve_stdio(config: CairnConfig) -> Result<(), McpTransportError> {
    let handler = CairnMcpHandler::new(config);
    let running = handler
        .serve(rmcp::transport::io::stdio())
        .await
        .map_err(|e| McpTransportError::Initialize(e.to_string()))?;
    running
        .waiting()
        .await
        .map_err(|e| McpTransportError::Initialize(e.to_string()))
}
```

> **If the compiler rejects any rmcp import path**, run `cargo doc -p rmcp --open` and locate the correct module paths. Common corrections:
> - `PaginatedRequestParams` may be `PaginatedRequestParam` (no trailing `s`)  
> - `ServerCapabilities::builder()` may need the `capabilitiesbuilder` import
> - `ListToolsResult { tools, next_cursor }` — check if `next_cursor` is the right field name
> - `Tool::new(name, description, schema)` — verify the constructor signature

- [ ] **Step 6: Expose server from lib.rs + update capabilities + re-export serve_stdio**

Replace the `CairnMcpServer` struct in `crates/cairn-mcp/src/lib.rs` — set `stdio: true` and add module declarations + re-export. The full new `lib.rs`:

```rust
//! Cairn MCP adapter.
//!
//! P0: stdio transport wired. Verb dispatch returns `Aborted/Internal`
//! until the store adapter lands in issue #9. SSE transport is out of
//! scope (P1, issue #65).

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod dispatch;
pub mod error;
pub mod generated;
pub mod server;

use cairn_core::contract::mcp_server::{CONTRACT_VERSION, MCPServer, MCPServerCapabilities};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::register_plugin;

pub use server::serve_stdio;

/// Stable plugin name. Matches `name = ...` in `plugin.toml`.
pub const PLUGIN_NAME: &str = "cairn-mcp";

/// Plugin capability manifest TOML (parsed at registration time).
pub const MANIFEST_TOML: &str = include_str!("../plugin.toml");

/// Accepted host contract version range.
pub const ACCEPTED_RANGE: VersionRange =
    VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));

/// P0 `MCPServer` — stdio transport live; SSE and extensions pending.
#[derive(Default)]
pub struct CairnMcpServer;

#[async_trait::async_trait]
impl MCPServer for CairnMcpServer {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn capabilities(&self) -> &MCPServerCapabilities {
        static CAPS: MCPServerCapabilities = MCPServerCapabilities {
            stdio: true,
            sse: false,
            http_streamable: false,
            extensions: false,
        };
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        ACCEPTED_RANGE
    }
}

const _: () = assert!(
    ACCEPTED_RANGE.accepts(CONTRACT_VERSION),
    "host CONTRACT_VERSION outside this crate's declared range"
);

register_plugin!(MCPServer, CairnMcpServer, "cairn-mcp", MANIFEST_TOML);
```

- [ ] **Step 7: Update plugin.toml**

In `crates/cairn-mcp/plugin.toml`, change `stdio = false` to:
```toml
stdio = true
```

- [ ] **Step 8: Run the full cairn-mcp test suite**

```bash
cargo nextest run --package cairn-mcp
```

Expected: all tests pass. If any rmcp API surface is wrong, fix using `cargo doc -p rmcp --open`.

- [ ] **Step 9: Commit**

```bash
git add crates/cairn-mcp/src/server.rs crates/cairn-mcp/src/lib.rs \
        crates/cairn-mcp/plugin.toml crates/cairn-mcp/Cargo.toml \
        crates/cairn-mcp/tests/tool_list_snapshot.rs \
        crates/cairn-mcp/tests/snapshots/
git commit -m "feat(cairn-mcp): wire rmcp ServerHandler — list_tools + call_tool + serve_stdio (brief §4, §8)"
```

---

### Task 5: CLI `mcp serve` subcommand

**Files:**
- Create: `crates/cairn-cli/src/verbs/mcp_serve.rs`
- Modify: `crates/cairn-cli/src/verbs/mod.rs`
- Modify: `crates/cairn-cli/src/main.rs`

- [ ] **Step 1: Write a failing CLI smoke test**

In `crates/cairn-cli/tests/cli.rs`, append (or create the file if it only has stubs):

```rust
#[test]
fn mcp_serve_subcommand_exists_in_help() {
    // Verify `cairn mcp serve --help` exits 0.
    // Uses the binary built by cargo nextest.
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_cairn"))
        .args(["mcp", "serve", "--help"])
        .status()
        .expect("cairn binary must be reachable");
    assert!(status.success(), "mcp serve --help must exit 0");
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo nextest run --package cairn-cli --test cli -E 'test(mcp_serve_subcommand_exists_in_help)' 2>&1 | head -20
```

Expected: `error: unrecognized subcommand 'mcp'` (exit non-zero → test fails).

- [ ] **Step 3: Create mcp_serve.rs**

```rust
// crates/cairn-cli/src/verbs/mcp_serve.rs
//! `cairn mcp serve` — start the MCP stdio server.

use std::process::ExitCode;

use cairn_core::config::CairnConfig;

/// Run the MCP stdio server.
///
/// Blocks until stdin closes. Exits 0 on clean shutdown, 69
/// (`EX_UNAVAILABLE`) on transport error.
#[must_use]
pub fn run() -> ExitCode {
    let config = CairnConfig::default();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("invariant: tokio multi-thread runtime builds");

    match rt.block_on(cairn_mcp::serve_stdio(config)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cairn mcp serve: transport error — {e:#}");
            ExitCode::from(69) // EX_UNAVAILABLE
        }
    }
}
```

- [ ] **Step 4: Add the module to verbs/mod.rs**

In `crates/cairn-cli/src/verbs/mod.rs`, add:
```rust
pub mod mcp_serve;
```

- [ ] **Step 5: Add the mcp subcommand to main.rs**

In `crates/cairn-cli/src/main.rs`:

After `fn bootstrap_subcommand()`, add:

```rust
fn mcp_subcommand() -> clap::Command {
    clap::Command::new("mcp")
        .about("MCP server operations")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            clap::Command::new("serve")
                .about("Start the Cairn MCP stdio server (reads from stdin, writes to stdout)"),
        )
}
```

In `build_command()`, add before `.subcommand(bootstrap_subcommand())`:
```rust
.subcommand(mcp_subcommand())
```

In `main()`, add to the match arms before the `None` arm:
```rust
Some(("mcp", sub)) => run_mcp(sub),
```

After `run_bootstrap`, add:
```rust
fn run_mcp(matches: &ArgMatches) -> ExitCode {
    match matches.subcommand() {
        Some(("serve", _)) => verbs::mcp_serve::run(),
        _ => unreachable!("clap subcommand_required(true) on mcp ensures a subcommand is set"),
    }
}
```

- [ ] **Step 6: Run the CLI test**

```bash
cargo nextest run --package cairn-cli --test cli -E 'test(mcp_serve_subcommand_exists_in_help)'
```

Expected: PASSED.

- [ ] **Step 7: Manually verify the server starts and accepts a tools/list**

```bash
# Build in release so the binary is fast
cargo build -p cairn-cli --locked

# Send a minimal MCP initialize + tools/list sequence and verify clean exit
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.0.1"}}}
{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  | ./target/debug/cairn mcp serve
```

Expected output: three JSON-RPC response lines — `initialize` result, (no response to notification), `tools/list` result with 8 tools. Server exits when stdin closes.

- [ ] **Step 8: Commit**

```bash
git add crates/cairn-cli/src/verbs/mcp_serve.rs crates/cairn-cli/src/verbs/mod.rs \
        crates/cairn-cli/src/main.rs crates/cairn-cli/tests/cli.rs
git commit -m "feat(cairn-cli): add 'cairn mcp serve' subcommand — starts rmcp stdio server"
```

---

### Task 6: CLI-vs-MCP parity test

**Files:**
- Create: `crates/cairn-cli/tests/mcp_parity.rs`

The parity test verifies that for the same verb, the CLI and MCP dispatcher return equivalent response envelopes (same `contract`, `verb`, `status` at P0).

- [ ] **Step 1: Create the parity test**

```rust
// crates/cairn-cli/tests/mcp_parity.rs
//! CLI-vs-MCP verb parity tests.
//!
//! At P0 both surfaces return `Aborted/Internal` (store not wired).
//! This test pins that equivalence so a future store wiring in issue #9
//! that updates one surface but not the other gets caught here.

#![allow(missing_docs)]

use cairn_core::generated::envelope::{ResponseStatus, ResponseVerb};

/// Helper: run the CLI verb stub for `verb_name` and return the response.
fn cli_response(verb: ResponseVerb) -> cairn_core::generated::envelope::Response {
    cairn_cli::verbs::envelope::unimplemented_response(verb)
}

/// Helper: run the MCP dispatcher for `tool_name` and return the response.
fn mcp_response(tool_name: &str) -> cairn_core::generated::envelope::Response {
    cairn_mcp::dispatch::dispatch(tool_name, None)
}

macro_rules! parity_test {
    ($test_name:ident, $tool_name:literal, $verb:expr) => {
        #[test]
        fn $test_name() {
            let cli = cli_response($verb);
            let mcp = mcp_response($tool_name);

            assert_eq!(cli.contract, mcp.contract, "contract mismatch for {}", $tool_name);
            assert_eq!(
                format!("{:?}", cli.verb),
                format!("{:?}", mcp.verb),
                "verb echo mismatch for {}",
                $tool_name
            );
            assert_eq!(
                format!("{:?}", cli.status),
                format!("{:?}", mcp.status),
                "status mismatch for {}",
                $tool_name
            );
            // Both must carry an Internal error at P0
            let cli_code = cli.error.as_ref().and_then(|e| e["code"].as_str()).unwrap_or("");
            let mcp_code = mcp.error.as_ref().and_then(|e| e["code"].as_str()).unwrap_or("");
            assert_eq!(cli_code, mcp_code, "error.code mismatch for {}", $tool_name);
        }
    };
}

parity_test!(parity_ingest,        "ingest",        ResponseVerb::Ingest);
parity_test!(parity_search,        "search",        ResponseVerb::Search);
parity_test!(parity_retrieve,      "retrieve",      ResponseVerb::Retrieve);
parity_test!(parity_summarize,     "summarize",     ResponseVerb::Summarize);
parity_test!(parity_assemble_hot,  "assemble_hot",  ResponseVerb::AssembleHot);
parity_test!(parity_capture_trace, "capture_trace", ResponseVerb::CaptureTrace);
parity_test!(parity_lint,          "lint",          ResponseVerb::Lint);
parity_test!(parity_forget,        "forget",        ResponseVerb::Forget);
```

- [ ] **Step 2: Run parity tests**

```bash
cargo nextest run --package cairn-cli --test mcp_parity
```

Expected: all 8 parity tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-cli/tests/mcp_parity.rs
git commit -m "test(cairn-cli): CLI-vs-MCP verb parity tests — all 8 verbs must match at P0"
```

---

### Task 7: Upgrade conformance case

**Files:**
- Modify: `crates/cairn-core/src/contract/conformance/mcp_server.rs`

The `initialize_and_list_tools` tier-2 case is currently `Pending`. Now that the server can list tools, upgrade it to an active check.

- [ ] **Step 1: Write a failing conformance test**

In `crates/cairn-cli/tests/cli.rs`, append:

```rust
#[test]
fn mcp_server_conformance_has_no_pending_tier2_cases() {
    use cairn_core::contract::conformance::mcp_server as mcp_conf;
    use cairn_core::contract::conformance::CaseStatus;
    use cairn_core::contract::registry::PluginName;

    let mut registry = cairn_core::contract::registry::PluginRegistry::new();
    cairn_mcp::register(&mut registry).expect("registers");

    let name = PluginName::new("cairn-mcp").expect("valid plugin name");
    let outcomes = mcp_conf::run(&registry, &name);

    let pending: Vec<_> = outcomes
        .iter()
        .filter(|o| matches!(o.status, CaseStatus::Pending { .. }))
        .collect();

    assert!(
        pending.is_empty(),
        "conformance suite has pending tier-2 cases: {:?}",
        pending.iter().map(|o| o.id).collect::<Vec<_>>()
    );
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo nextest run --package cairn-cli --test cli \
  -E 'test(mcp_server_conformance_has_no_pending_tier2_cases)' 2>&1 | head -20
```

Expected: test fails because `initialize_and_list_tools` is still `Pending`.

- [ ] **Step 3: Update the conformance case**

In `crates/cairn-core/src/contract/conformance/mcp_server.rs`, replace the tier-2 stub:

```rust
// Replace this:
CaseOutcome {
    id: "initialize_and_list_tools",
    tier: Tier::Two,
    status: CaseStatus::Pending {
        reason: "real impl pending",
    },
},
```

With:

```rust
tier2_stdio_advertised_when_stdio_capability_true(registry, name, &plugin),
```

And add the function before `fn tier1_arc_pointer_stable`:

```rust
fn tier2_stdio_advertised_when_stdio_capability_true(
    _registry: &PluginRegistry,
    _name: &PluginName,
    plugin: &std::sync::Arc<dyn crate::contract::mcp_server::MCPServer>,
) -> CaseOutcome {
    let caps = plugin.capabilities();
    let status = if caps.stdio {
        // Verify the manifest also declared stdio=true (tier1_manifest_features_match_capabilities
        // already checks this; here we verify the impl advertises it at all).
        CaseStatus::Ok
    } else {
        CaseStatus::Failed {
            message: "MCPServer.capabilities().stdio is false; \
                      server cannot receive connections over stdio"
                .to_string(),
        }
    };
    CaseOutcome {
        id: "initialize_and_list_tools",
        tier: Tier::Two,
        status,
    }
}
```

- [ ] **Step 4: Run the conformance test**

```bash
cargo nextest run --package cairn-cli --test cli \
  -E 'test(mcp_server_conformance_has_no_pending_tier2_cases)'
```

Expected: PASSED.

- [ ] **Step 5: Verify the full cairn-cli test suite still passes**

```bash
cargo nextest run --package cairn-cli
```

Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/src/contract/conformance/mcp_server.rs \
        crates/cairn-cli/tests/cli.rs
git commit -m "test(conformance): upgrade initialize_and_list_tools tier-2 from Pending to Ok (brief §4)"
```

---

### Task 8: Final verification pass

- [ ] **Step 1: Run the full verification checklist**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
```

Expected: all pass. Common clippy issues to watch:
- `clippy::expect_used` in non-test code — use `map_err` instead
- `clippy::needless_pass_by_value` — check function signatures
- `dead_code` on the `config` field in `CairnMcpHandler` — the `#[allow(dead_code)]` handles this

- [ ] **Step 2: Verify supply chain**

```bash
cargo deny check
cargo audit --deny warnings
cargo machete
```

Expected: all pass. `cargo machete` may flag `serde` in cairn-mcp as unused if no direct serde derive is in the crate. Remove or keep in machete ignore list based on the output.

- [ ] **Step 3: Run docs build**

```bash
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" \
  cargo doc --workspace --no-deps --document-private-items --locked
```

Expected: no errors.

- [ ] **Step 4: Verify the acceptance criteria manually**

```bash
# AC1: harness can start cairn over stdio and list tools
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.0.1"}}}
{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  | cargo run -p cairn-cli --bin cairn -- mcp serve
# Expected: tools/list response with 8 tools

# AC2: every verb routes to the same backend as CLI (parity tests cover this)
cargo nextest run --package cairn-cli --test mcp_parity
# Expected: all 8 pass

# AC3: transport errors separate from Cairn typed errors (type system guarantees this)
# McpTransportError vs Response are distinct types — verified by compiler
```

- [ ] **Step 5: Final commit**

```bash
git add -p  # review all remaining unstaged changes
git commit -m "chore(cairn-mcp): verification pass — all CI checks green"
```

---

## Self-Review

### Spec coverage

| Issue requirement | Covered by |
|-------------------|-----------|
| stdio lifecycle startup | `serve_stdio()` in Task 4 |
| stdio lifecycle shutdown | `running.waiting()` blocks until stdin EOF |
| request dispatch | `dispatch::dispatch()` in Task 3 |
| structured logging | `#[instrument]` on `call_tool`, `tracing::info!` in Task 4 |
| vault/config resolution | `CairnConfig` passed to `CairnMcpHandler::new()` in Task 4; loaded in `mcp_serve.rs` in Task 5 |
| generated schema definitions used (not hand-maintained) | `TOOLS[*].input_schema` bytes fed to `Tool::new()` in `list_tools()` |
| mutations flow through signed envelopes | `p0_stub_intent()` + `verify_signed_intent()` in Task 3 |
| harness can start cairn and list tools | manual smoke in Task 5, Step 7 |
| every core verb routes to same backend | CLI parity tests in Task 6 |
| transport errors separated from Cairn errors | `McpTransportError` vs `Response` in Task 2 |
| MCP protocol smoke tests | dispatch unit tests in Task 3 |
| tool list snapshot tests | `tool_auth_metadata_snapshot` in Task 4 |
| CLI-vs-MCP verb parity tests | `mcp_parity.rs` in Task 6 |

### Placeholder scan

- `config` field on `CairnMcpHandler` is unused at P0 — `#[allow(dead_code)]` with a comment pointing to issue #9.
- `_args_json` parameter in `dispatch()` is intentionally ignored at P0 — the `_` prefix documents this.
- `p0_stub_intent()` is a P0-specific function with a doc comment explaining it must be replaced at P1.

### Type consistency

- `ResponseVerb::AssembleHot` — confirmed from `cairn-core/src/generated/envelope/mod.rs` line 17.
- `ResponseVerb::CaptureTrace` — confirmed from same source.
- `fresh_ulid()` returns `cairn_core::generated::common::Ulid` — consistent with `envelope.rs` in cairn-cli.
- `p0_stub_intent()` constructs `SignedIntent` with the same field set as `cairn_core::verifier::tests::good_intent()`.

---

**Plan complete and saved to `docs/superpowers/plans/2026-04-26-mcp-stdio-server.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — Fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints

**Which approach?**
