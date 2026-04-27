//! `cairn mcp serve` — start the Cairn MCP stdio server.

use std::process::ExitCode;

use cairn_core::config::CairnConfig;

/// Run the MCP stdio server.
///
/// Blocks until stdin closes or the tokio runtime shuts down.
/// Exits 0 on clean shutdown, 69 (`EX_UNAVAILABLE`) on transport error.
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
