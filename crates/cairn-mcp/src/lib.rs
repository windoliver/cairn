//! Cairn MCP adapter — stdio transport surface (§4.1, §8).
//!
//! This crate ships the IDL-generated `generated` submodule, the plugin
//! manifest, an `MCPServer` impl advertising `stdio = true`, the
//! [`CairnMcpHandler`] request handler, and the [`serve_stdio`] entry point
//! that wires the real stdin/stdout transport.
//!
//! The `generated` submodule is produced by `cairn-codegen` from the IDL.
//! SSE / HTTP-streamable transports land in a follow-up issue.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod error;
pub mod generated;
pub mod graph_tools;
pub mod handler;
pub mod relay;

pub use error::TransportError;
pub use handler::CairnMcpHandler;

use std::sync::Arc;

use cairn_core::contract::mcp_server::{CONTRACT_VERSION, MCPServer, MCPServerCapabilities};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::register_plugin;
use rmcp::ServiceExt as _;
use tokio::io::{AsyncRead, AsyncWrite};

/// Stable plugin name. Matches `name = ...` in `plugin.toml`.
pub const PLUGIN_NAME: &str = "cairn-mcp";

/// Plugin capability manifest TOML (parsed at registration time).
pub const MANIFEST_TOML: &str = include_str!("../plugin.toml");

/// Accepted host contract version range. Single source of truth for both the
/// trait impl's `supported_contract_versions()` and the const-eval guard.
pub const ACCEPTED_RANGE: VersionRange =
    VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));

/// `MCPServer` implementation that advertises the stdio transport capability.
///
/// The stdio transport is the P0 surface: `cairn mcp` reads JSON-RPC frames
/// from stdin and writes responses to stdout, making it composable with any
/// host harness that speaks MCP over stdio pipes.
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

// Compile-time guard: this crate's accepted range must include the host
// CONTRACT_VERSION. If we ever bump CONTRACT_VERSION without bumping
// ACCEPTED_RANGE, this assertion fires at build time.
const _: () = assert!(
    ACCEPTED_RANGE.accepts(CONTRACT_VERSION),
    "host CONTRACT_VERSION outside this crate's declared range"
);

register_plugin!(MCPServer, CairnMcpServer, "cairn-mcp", MANIFEST_TOML);

/// Start an MCP server on the process's own stdin / stdout and block until
/// stdin closes.
///
/// Reads newline-delimited JSON-RPC frames from `stdin`, writes responses to
/// `stdout`, and returns once the client closes its end of the pipe (or on
/// abnormal termination). The function is intentionally blocking — callers
/// should call it from a dedicated async entry-point and not combine it with
/// other long-running tasks on the same runtime.
///
/// # Errors
///
/// Returns [`TransportError::Service`] if the rmcp service fails to initialize
/// (e.g. the client sends a malformed `initialize` request) or terminates
/// abnormally. Normal stdin-EOF shutdown is not an error and returns `Ok(())`.
///
/// Transport-layer I/O errors (broken pipe, etc.) are separate from domain
/// errors: those surface inside the MCP protocol as
/// `CallToolResult { is_error: true }` and do not reach this function's return
/// value.
#[deprecated(
    since = "0.1.0",
    note = "use serve_stdio_with_store; serve_stdio returns the 8-verb manifest \
            with no store wiring and is retained only for unwired-fallback callers"
)]
pub async fn serve_stdio() -> Result<(), TransportError> {
    let handler = CairnMcpHandler::new();
    let transport = rmcp::transport::io::stdio();
    let service = handler
        .serve(transport)
        .await
        .map_err(|e| TransportError::Service(e.to_string()))?;
    service
        .waiting()
        .await
        .map_err(|e| TransportError::Service(e.to_string()))?;
    Ok(())
}

/// Plan C entry point: serve MCP over stdio with a wired store, concrete
/// sqlite store (for graph tools), scope resolver, and principal.
///
/// Internally wraps stdin in a relay (blank-line filter + 16 MiB frame cap)
/// before handing bytes to the rmcp handler.  stdout is passed through
/// unmodified.
///
/// # Errors
/// Returns [`TransportError::Service`] if the rmcp service fails or
/// terminates abnormally.  Transport-layer I/O errors surface as
/// `CallToolResult { is_error: true }` inside the protocol and do not reach
/// this function's return value.
pub async fn serve_stdio_with_store(
    store: Arc<dyn cairn_core::contract::memory_store::MemoryStore>,
    sqlite_store: Arc<cairn_store_sqlite::SqliteMemoryStore>,
    scope: Arc<dyn cairn_core::mcp_auth::McpSessionScope>,
    config: cairn_core::config::CairnConfig,
    principal: cairn_core::domain::ScopeTuple,
) -> Result<(), TransportError> {
    serve_stdio_with_store_io(
        store,
        sqlite_store,
        scope,
        config,
        principal,
        tokio::io::stdin(),
        tokio::io::stdout(),
    )
    .await
}

/// Internal helper — same as [`serve_stdio_with_store`] but accepts arbitrary
/// `AsyncRead` / `AsyncWrite` so integration tests can drive it in-process.
///
/// `input` is wrapped in a relay that drops blank lines and enforces the
/// 16 MiB per-frame cap before bytes reach the rmcp codec.
///
/// # Errors
/// Same as [`serve_stdio_with_store`].
pub async fn serve_stdio_with_store_io<I, O>(
    store: Arc<dyn cairn_core::contract::memory_store::MemoryStore>,
    sqlite_store: Arc<cairn_store_sqlite::SqliteMemoryStore>,
    scope: Arc<dyn cairn_core::mcp_auth::McpSessionScope>,
    config: cairn_core::config::CairnConfig,
    principal: cairn_core::domain::ScopeTuple,
    input: I,
    output: O,
) -> Result<(), TransportError>
where
    I: AsyncRead + Unpin + Send + 'static,
    O: AsyncWrite + Unpin + Send + 'static,
{
    // Relay: filter blank lines from `input`, write surviving frames to the
    // framer-reader half that rmcp reads from.
    let (framer_reader, relay_writer) = tokio::io::duplex(64 * 1024);
    let mut relay_task =
        tokio::spawn(async move { relay::run_relay(input, relay_writer).await });

    let handler =
        CairnMcpHandler::with_store_scope_and_sqlite(store, sqlite_store, scope, config, principal);

    // rmcp's `IntoTransport` is implemented for `(R, W)` tuples where R:
    // `AsyncRead + Unpin + Send + 'static` and W: `AsyncWrite + Unpin + Send +
    // 'static` — pass the relay-filtered reader and the original output writer.
    let serve_result = handler
        .serve((framer_reader, output))
        .await
        .map_err(|e| TransportError::Service(e.to_string()));

    let waiting_result: Result<(), TransportError> = match serve_result {
        Ok(service) => service
            .waiting()
            .await
            .map(|_| ())
            .map_err(|e| TransportError::Service(e.to_string())),
        Err(e) => Err(e),
    };

    // Drain the relay task and fold its outcome into our return value. A
    // FrameTooLarge or I/O failure on the input side must surface as an error
    // — otherwise rmcp sees clean EOF on its reader after the relay drops the
    // writer and `waiting_result` is `Ok(())`, masking the protocol violation.
    //
    // The previous implementation gated this on a single `is_finished()`
    // probe, which races with task completion: a relay that has just hit
    // `FrameTooLarge` may not have its `JoinHandle` flagged finished yet,
    // and the abort-then-Ok branch would mask the error as a clean shutdown.
    // Instead, give the relay a brief grace window to drain — in any
    // protocol-violation path the relay finishes the moment it drops its
    // writer, which is also what causes rmcp to see EOF and complete
    // `waiting()`. If the grace window elapses, the rmcp side shut down
    // cleanly via a notification while the client is still holding stdin
    // open; abort to avoid hanging on stdin EOF that may never arrive.
    // Aborting only kicks in after we have proven the relay has nothing to
    // surface, so a real `FrameTooLarge` / I/O error is always observed.
    let relay_drain_deadline = std::time::Duration::from_millis(250);
    let relay_result: Result<(), TransportError> =
        match tokio::time::timeout(relay_drain_deadline, &mut relay_task).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(e))) => Err(e),
            Ok(Err(join_err)) if join_err.is_cancelled() => Ok(()),
            Ok(Err(join_err)) => Err(TransportError::Service(join_err.to_string())),
            Err(_elapsed) => {
                // rmcp shut down cleanly via notification while the
                // client kept stdin open; abort the still-tailing relay
                // and treat its cancellation as a clean shutdown.
                relay_task.abort();
                match (&mut relay_task).await {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(e)) => Err(e),
                    Err(join_err) if join_err.is_cancelled() => Ok(()),
                    Err(join_err) => Err(TransportError::Service(join_err.to_string())),
                }
            }
        };

    waiting_result.and(relay_result)
}
