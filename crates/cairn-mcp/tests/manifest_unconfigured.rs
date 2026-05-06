//! Integration test (issue #190 Plan A acceptance criterion 8):
//!
//! An MCP stdio server constructed against a [`CairnConfig`] with no
//! `[mcp.stdio]` block (or with `single_tenant = false`) lists the 8-verb
//! manifest only. [`CairnConfig::mcp_graph_tools_available`] must return
//! [`McpGraphAvailability::UnavailableSingleTenantOff`] for the same deployment shape.

use std::sync::Arc;

use cairn_core::config::CairnConfig;
use cairn_core::contract::memory_store::MemoryStoreCapabilities;
use cairn_core::domain::ScopeTuple;
use cairn_core::mcp_auth::{
    ConfigBackedScope, McpGraphAvailability, McpSessionScope, McpTransport,
};
use cairn_mcp::CairnMcpHandler;
use cairn_test_fixtures::FixtureStore;

fn graph_capable_caps() -> MemoryStoreCapabilities {
    MemoryStoreCapabilities {
        fts: true,
        vector: false,
        graph_edges: true,
        transactions: true,
        per_record_consent_model: true,
    }
}

#[test]
fn unconfigured_stdio_lists_eight_verbs_only() {
    // Default config: single_tenant = false → no scope resolver wired.
    let cfg = CairnConfig::default();
    let store: Arc<dyn cairn_core::contract::memory_store::MemoryStore> =
        Arc::new(FixtureStore::default());

    // Plan A: serve_stdio_with_store is gated behind single_tenant = true
    // in the CLI; the unwired path uses CairnMcpHandler::with_store
    // (no scope, no principal). Mirror that here.
    let handler = CairnMcpHandler::with_store(store, cfg.clone());

    let listed = handler.listed_tool_names();
    assert_eq!(
        listed.len(),
        cairn_mcp::generated::TOOLS.len(),
        "unconfigured stdio MUST list exactly the 8-verb manifest, got {listed:?}",
    );
    for tool in &listed {
        assert!(
            !tool.starts_with("graph."),
            "no graph.* tools in unconfigured manifest, got `{tool}`",
        );
    }

    // Predicate side: same config -> UnavailableSingleTenantOff
    // regardless of scope-resolver presence.
    let scope = ConfigBackedScope::new(ScopeTuple::default());
    let dyn_s: &dyn McpSessionScope = &scope;
    let avail = cfg.mcp_graph_tools_available(
        Some(dyn_s),
        McpTransport::Stdio,
        &graph_capable_caps(),
    );
    assert_eq!(
        avail,
        McpGraphAvailability::UnavailableSingleTenantOff,
        "predicate must report UnavailableSingleTenantOff for default config",
    );
}

#[test]
fn opted_in_stdio_with_graphless_store_reports_no_store_capability() {
    let mut cfg = CairnConfig::default();
    cfg.mcp.stdio.single_tenant = true;
    cfg.mcp.stdio.principal = Some(ScopeTuple {
        tenant: Some("acme".into()),
        ..ScopeTuple::default()
    });
    cfg.validate_mcp().expect("opt-in config valid");

    let scope = ConfigBackedScope::new(cfg.mcp.stdio.principal.clone().unwrap());
    let dyn_s: &dyn McpSessionScope = &scope;

    // Same as `graph_capable_caps` but graph_edges = false.
    let caps_no_graph = MemoryStoreCapabilities {
        graph_edges: false,
        ..graph_capable_caps()
    };

    let avail = cfg.mcp_graph_tools_available(
        Some(dyn_s),
        McpTransport::Stdio,
        &caps_no_graph,
    );
    assert_eq!(avail, McpGraphAvailability::UnavailableNoStoreCapability);
}

#[test]
fn opted_in_stdio_without_resolver_reports_no_scope_resolver() {
    let mut cfg = CairnConfig::default();
    cfg.mcp.stdio.single_tenant = true;
    cfg.mcp.stdio.principal = Some(ScopeTuple {
        tenant: Some("acme".into()),
        ..ScopeTuple::default()
    });
    let avail = cfg.mcp_graph_tools_available(
        None,
        McpTransport::Stdio,
        &graph_capable_caps(),
    );
    assert_eq!(avail, McpGraphAvailability::UnavailableNoScopeResolver);
}
