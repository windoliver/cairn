//! `McpServer` contract (brief §4 row 5).
//!
//! P0: stdio + SSE transports; eight core verbs + opt-in extensions.
//! Implementation lives in `cairn-mcp` (#64); transports + handshake
//! parity tests in #65, #66, #67.

use crate::contract::version::{ContractVersion, VersionRange};

/// Contract version for `McpServer`. Bumps when the trait surface changes.
pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(0, 1, 0);

/// Static capability declaration for a `McpServer` impl.
// Four flags cover distinct MCP transport/extension dimensions; a state
// machine adds indirection with no clarity gain here.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct McpServerCapabilities {
    /// Whether the server supports the stdio transport.
    pub stdio: bool,
    /// Whether the server supports the SSE transport.
    pub sse: bool,
    /// Whether the server supports the HTTP streamable transport.
    pub http_streamable: bool,
    /// Whether the server supports opt-in MCP extensions.
    pub extensions: bool,
}

/// MCP server contract — protocol binding over the eight Cairn verbs.
///
/// Brief §4 row 5: P0 is stdio + SSE transports (#64). HTTP streamable
/// and extension negotiation are P1.
#[async_trait::async_trait]
pub trait McpServer: Send + Sync {
    /// Stable identifier of the registered plugin instance.
    fn name(&self) -> &str;

    /// Static capability advertisement (brief §4.1).
    fn capabilities(&self) -> &McpServerCapabilities;

    /// Range of `McpServer::CONTRACT_VERSION` values this impl accepts.
    fn supported_contract_versions(&self) -> VersionRange;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubMcp;

    #[async_trait::async_trait]
    impl McpServer for StubMcp {
        fn name(&self) -> &'static str {
            "stub-mcp"
        }
        fn capabilities(&self) -> &McpServerCapabilities {
            static CAPS: McpServerCapabilities = McpServerCapabilities {
                stdio: true,
                sse: false,
                http_streamable: false,
                extensions: false,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
        }
    }

    #[test]
    fn dyn_compatible() {
        let m: Box<dyn McpServer> = Box::new(StubMcp);
        assert_eq!(m.name(), "stub-mcp");
        assert!(m.supported_contract_versions().accepts(CONTRACT_VERSION));
    }
}
