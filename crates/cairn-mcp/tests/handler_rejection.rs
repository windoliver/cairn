//! MCP capability-rejection envelope (issue #53).
//!
//! Pinned as `#[ignore]` until a wired-store rejection path lands. The
//! handler.rs `handle_search` `CapabilityUnavailable` arm carries
//! `cairn_core::status::remediation_for(capability)` and appends it to
//! the `CallToolResult` text — the live behavior is exercised through
//! `crates/cairn-mcp/src/handler.rs::handle_search` + #61 follow-up.

#[test]
#[ignore = "requires wired-store CapabilityUnavailable path; tracked in #61 follow-up"]
fn mcp_search_semantic_rejection_carries_remediation() {
    // Placeholder: when search dispatch can be driven without a wired
    // store inside this test, exercise CapabilityUnavailable end-to-end
    // and assert the CallToolResult.text contains both the capability
    // string and the registered remediation hint.
}
