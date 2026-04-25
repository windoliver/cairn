//! `AgentProvider` contract — P2 forward stub (brief §4 row 6).
//!
//! Surface frozen here so the registry has a slot for agent-mode workers.
//! Full method surface, cost-budget enforcement, and conformance suite
//! land in #124 / #125.

use crate::contract::version::{ContractVersion, VersionRange};

/// Contract version for `AgentProvider`. Bumps when the trait surface changes.
pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(0, 0, 1);

/// Static capability declaration for an `AgentProvider` impl.
// Four flags cover distinct agent safety/capability dimensions; a state
// machine adds indirection with no clarity gain here.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AgentProviderCapabilities {
    /// Whether the agent respects a caller-supplied cost budget.
    pub honors_cost_budget: bool,
    /// Whether the agent enforces scope restrictions on its actions.
    pub scope_enforced: bool,
    /// Whether the agent can invoke MCP tools.
    pub mcp_tools: bool,
    /// Whether the agent can invoke CLI subprocess tools.
    pub cli_subprocess_tools: bool,
}

/// Agent provider contract — autonomous agent workers with bounded scope.
///
/// Brief §4 row 6: P2 forward stub. Method surface, cost-budget enforcement,
/// and conformance suite land in #124 / #125.
#[async_trait::async_trait]
pub trait AgentProvider: Send + Sync {
    /// Stable identifier of the registered plugin instance.
    fn name(&self) -> &str;

    /// Static capability advertisement (brief §4.1).
    fn capabilities(&self) -> &AgentProviderCapabilities;

    /// Range of `AgentProvider::CONTRACT_VERSION` values this impl accepts.
    fn supported_contract_versions(&self) -> VersionRange;
}
