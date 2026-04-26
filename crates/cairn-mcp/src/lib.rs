//! Cairn MCP adapter (P0 scaffold).
//!
//! P0: no transports yet — this crate ships the IDL-generated `generated`
//! submodule, the plugin manifest, a stub `MCPServer` impl with all
//! capability flags `false`, and a `register()` entry point. Real stdio +
//! SSE wiring lands in #64.
//!
//! The `generated` submodule is produced by `cairn-codegen` from the IDL.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod generated;

use cairn_core::contract::mcp_server::{CONTRACT_VERSION, MCPServer, MCPServerCapabilities};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::register_plugin;

/// Stable plugin name. Matches `name = ...` in `plugin.toml`.
pub const PLUGIN_NAME: &str = "cairn-mcp";

/// Plugin capability manifest TOML (parsed at registration time).
pub const MANIFEST_TOML: &str = include_str!("../plugin.toml");

/// Accepted host contract version range. Single source of truth for both the
/// trait impl's `supported_contract_versions()` and the const-eval guard.
pub const ACCEPTED_RANGE: VersionRange =
    VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));

/// P0 stub `MCPServer`. All capability flags are `false`; transport
/// wiring lands in #64.
#[derive(Default)]
pub struct CairnMcpServer;

#[async_trait::async_trait]
impl MCPServer for CairnMcpServer {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn capabilities(&self) -> &MCPServerCapabilities {
        static CAPS: MCPServerCapabilities = MCPServerCapabilities {
            stdio: false,
            sse: false,
            http_streamable: false,
            extensions: false,
        };
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        ACCEPTED_RANGE
    }
}

// Compile-time guard: this crate's accepted range must include the host
// CONTRACT_VERSION. If we ever bump CONTRACT_VERSION without bumping
// ACCEPTED_RANGE, this assertion fires at build time.
const _: () = assert!(
    ACCEPTED_RANGE.accepts(CONTRACT_VERSION),
    "host CONTRACT_VERSION outside this crate's declared range"
);

register_plugin!(MCPServer, CairnMcpServer, "cairn-mcp", MANIFEST_TOML);
