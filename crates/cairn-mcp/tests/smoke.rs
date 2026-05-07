// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use cairn_core as _;

use cairn_mcp::error::TransportError;

#[test]
fn transport_error_displays() {
    let e = TransportError::Service("handshake failed".to_owned());
    assert!(e.to_string().contains("service"), "error display: {e}");
}
