// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use cairn_core as _;

use cairn_mcp::error::McpTransportError;

#[test]
fn transport_error_displays() {
    let e = McpTransportError::Initialize("handshake failed".to_owned());
    assert!(e.to_string().contains("initialize"), "error display: {e}");
}
