//! Transport-level errors for the MCP stdio adapter.

use thiserror::Error;

/// Transport-level errors for the Cairn MCP stdio adapter.
///
/// Separates wire/IO failures (this type) from Cairn typed operation
/// errors, which stay inside the `cairn.mcp.v1` response envelope.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum McpTransportError {
    /// MCP service failed to initialize or terminated abnormally.
    #[error("MCP stdio service failed: {0}")]
    Service(String),

    /// IO error on the underlying stdio transport.
    #[error("stdio IO error: {0}")]
    Io(#[from] std::io::Error),
}
