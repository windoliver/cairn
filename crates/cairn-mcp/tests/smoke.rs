//! MCP transport smoke tests.
//!
//! Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use cairn_mcp::error::TransportError;
use cairn_mcp::generated::TOOLS;
use cairn_mcp::handler::{CairnMcpHandler, dispatch_stub};

#[test]
fn transport_error_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<TransportError>();
}

#[test]
fn transport_error_io_display() {
    let e = TransportError::Io(std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "broken pipe",
    ));
    assert!(e.to_string().contains("stdio I/O error"));
}

#[test]
fn transport_error_service_display() {
    let e = TransportError::Service("service failed".to_string());
    assert!(e.to_string().contains("MCP service error"));
    assert!(e.to_string().contains("service failed"));
}

// ── CairnMcpHandler ─────────────────────────────────────────────────────────

#[test]
fn handler_is_default_and_new() {
    // Verify both construction paths compile and produce the same type.
    let a = CairnMcpHandler;
    let b = CairnMcpHandler::new();
    // Unit struct — Debug repr is identical.
    assert_eq!(format!("{a:?}"), format!("{b:?}"));
}

#[test]
fn handler_get_info_advertises_tools() {
    use rmcp::ServerHandler as _;
    let h = CairnMcpHandler::new();
    let info = h.get_info();
    assert!(
        info.capabilities.tools.is_some(),
        "server info must advertise tools capability"
    );
    assert_eq!(info.server_info.name, "cairn");
}

#[test]
fn list_tools_returns_eight_verbs() {
    // TOOLS has 8 entries (one per verb).  The conversion logic in
    // `list_tools` iterates TOOLS, so verifying the count here is
    // sufficient without needing a live RequestContext.
    assert_eq!(TOOLS.len(), 8, "TOOLS must contain exactly 8 verbs");
}

#[test]
fn tools_list_matches_generated_tools_constant() {
    // The handler converts TOOLS 1-for-1; verify the verb names are
    // the canonical set from the design brief §8.
    let names: Vec<&str> = TOOLS.iter().map(|d| d.name).collect();
    for expected in &[
        "ingest",
        "search",
        "retrieve",
        "summarize",
        "assemble_hot",
        "capture_trace",
        "lint",
        "forget",
    ] {
        assert!(
            names.contains(expected),
            "TOOLS must include verb '{expected}'"
        );
    }
}

#[test]
fn dispatch_stub_is_error_result() {
    let result = dispatch_stub("ingest");
    assert_eq!(
        result.is_error,
        Some(true),
        "dispatch_stub must set is_error = true"
    );
    assert!(
        !result.content.is_empty(),
        "dispatch_stub must include content"
    );
    let text = format!("{:?}", result.content);
    assert!(
        text.contains("ingest"),
        "dispatch_stub content must mention the verb name"
    );
    assert!(
        text.contains("not yet implemented"),
        "dispatch_stub content must mention the placeholder message"
    );
}
