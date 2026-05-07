//! Transport-level error type for the cairn-mcp stdio server.
//!
//! Distinct from cairn domain errors, which surface as
//! `CallToolResult { is_error: true }` inside the MCP protocol.

use std::io;

/// Errors owned by the stdio transport layer.
///
/// These are failures in the MCP framing / lifecycle, not cairn verb failures.
/// Verb errors surface as `CallToolResult { is_error: true }`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TransportError {
    /// stdio I/O failure (read EOF, broken pipe, etc.).
    #[error("stdio I/O error: {0}")]
    Io(#[from] io::Error),

    /// rmcp service failed to start or was shut down abnormally.
    #[error("MCP service error: {0}")]
    Service(String),
}
