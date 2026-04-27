//! MCP transport smoke tests.
//!
//! Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use cairn_mcp::error::TransportError;

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
