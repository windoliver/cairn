//! Transport-level errors for the MCP stdio adapter.

use thiserror::Error;

/// Transport-level errors for the Cairn MCP stdio adapter.
///
/// Separates wire/IO failures (this type) from Cairn typed operation
/// errors, which stay inside the `cairn.mcp.v1` response envelope.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum McpTransportError {
    /// MCP server failed to complete the `initialize` handshake.
    #[error("MCP stdio server failed to initialize: {0}")]
    Initialize(String),

    /// MCP service failed after initialization.
    #[error("MCP stdio service failed: {0}")]
    Service(String),

    /// IO error on the underlying stdio transport.
    #[error("stdio IO error: {0}")]
    Io(#[from] std::io::Error),
}
