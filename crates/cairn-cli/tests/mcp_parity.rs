//! CLI-vs-MCP verb parity tests.
//!
//! At P0 both surfaces return `Aborted/Internal` (store not wired).
//! This test pins that equivalence so a future store wiring in issue #9
//! that updates one surface but not the other gets caught here.

#![allow(missing_docs)]

use cairn_core::generated::envelope::ResponseVerb;

/// Helper: run the CLI verb stub for `verb` and return the response.
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

            assert_eq!(
                cli.contract, mcp.contract,
                "contract mismatch for {}",
                $tool_name
            );
            assert_eq!(cli.verb, mcp.verb, "verb echo mismatch for {}", $tool_name);
            assert_eq!(cli.status, mcp.status, "status mismatch for {}", $tool_name);
            // Both must carry an Internal error at P0
            let cli_code = cli
                .error
                .as_ref()
                .and_then(|e| e["code"].as_str())
                .unwrap_or("");
            let mcp_code = mcp
                .error
                .as_ref()
                .and_then(|e| e["code"].as_str())
                .unwrap_or("");
            assert_eq!(cli_code, mcp_code, "error.code mismatch for {}", $tool_name);
        }
    };
}

parity_test!(parity_ingest, "ingest", ResponseVerb::Ingest);
parity_test!(parity_search, "search", ResponseVerb::Search);
parity_test!(parity_retrieve, "retrieve", ResponseVerb::Retrieve);
parity_test!(parity_summarize, "summarize", ResponseVerb::Summarize);
parity_test!(
    parity_assemble_hot,
    "assemble_hot",
    ResponseVerb::AssembleHot
);
parity_test!(
    parity_capture_trace,
    "capture_trace",
    ResponseVerb::CaptureTrace
);
parity_test!(parity_lint, "lint", ResponseVerb::Lint);
parity_test!(parity_forget, "forget", ResponseVerb::Forget);
