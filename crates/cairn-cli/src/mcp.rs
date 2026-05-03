//! `cairn mcp` subcommand — drives the MCP stdio transport.
//!
//! Creates a dedicated multi-thread tokio runtime (the MCP server is
//! long-lived) and blocks until stdin closes or the client sends a
//! shutdown notification.

use std::process::ExitCode;

/// Run the MCP stdio server.
///
/// Blocks until the MCP client closes stdin or sends a shutdown
/// notification. Exit codes:
/// - `0` — clean shutdown
/// - `69` (`EX_UNAVAILABLE`) — transport startup or I/O failure
#[must_use]
pub fn run() -> ExitCode {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("invariant: tokio multi-thread runtime always builds");

    match rt.block_on(cairn_mcp::serve_stdio()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cairn mcp: {e:#}");
            ExitCode::from(69) // EX_UNAVAILABLE
        }
    }
}
