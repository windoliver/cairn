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
pub mod handler;

pub use error::TransportError;
pub use handler::CairnMcpHandler;

use cairn_core::contract::mcp_server::{CONTRACT_VERSION, MCPServer, MCPServerCapabilities};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::register_plugin;
use rmcp::ServiceExt as _;

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
