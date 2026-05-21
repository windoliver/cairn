//! Bundled bounded [`AgentProvider`](cairn_core::contract::AgentProvider) runtime.

mod action;
mod provider;
mod tool;

pub use provider::{CairnAgentProvider, UnconfiguredCairnAgentProvider};
pub use tool::{AgentToolExecutor, CairnCliToolExecutor, ToolExecution};

const MANIFEST_TOML: &str = r#"
name = "cairn-agent-core"
contract = "AgentProvider"

[contract_version_range.min]
major = 0
minor = 1
patch = 0

[contract_version_range.max_exclusive]
major = 0
minor = 2
patch = 0

[features]
honors_cost_budget = true
scope_enforced = true
mcp_tools = false
cli_subprocess_tools = true
"#;

cairn_core::register_plugin!(
    AgentProvider,
    UnconfiguredCairnAgentProvider,
    "cairn-agent-core",
    MANIFEST_TOML
);
