//! Bundled bounded [`AgentProvider`](cairn_core::contract::AgentProvider) runtime.

use std::sync::Arc;

mod action;
mod provider;
mod tool;

pub use provider::{CairnAgentProvider, UnconfiguredCairnAgentProvider};
pub use tool::{AgentToolExecutor, CairnCliToolExecutor, ToolExecution};

use cairn_core::contract::conformance::agent_provider::{
    ALLOWLIST_REJECTS_UNLISTED_TOOL_PROMPT, BUDGET_EXHAUSTION_ABORTS_CLEANLY_PROMPT,
    MUTATING_VERB_REQUIRES_SCOPE_PROMPT, WRITES_ARE_WAL_ROUTED_PROMPT,
};
use cairn_core::contract::manifest::PluginManifest;
use cairn_core::contract::registry::{PluginError, PluginName, PluginRegistry};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::contract::{
    AgentProviderError, AgentToolCall, CairnVerb, CompletionOutput, CompletionRequest, LLMProvider,
    LLMProviderCapabilities, LlmError,
};

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

/// Register a configured deterministic provider for `cairn plugins verify`.
///
/// The normal bundled registration is intentionally unconfigured because the
/// runtime needs a deployment LLM. The conformance path supplies a tiny local
/// LLM/tool harness so `plugins verify` can exercise the real provider loop
/// without network or vault I/O.
///
/// # Errors
///
/// Returns [`PluginError`] if the manifest, plugin name, or registration fails.
pub fn register_conformance(registry: &mut PluginRegistry) -> Result<(), PluginError> {
    let name = PluginName::new("cairn-agent-core")?;
    let manifest = PluginManifest::parse_toml(MANIFEST_TOML)?;
    let llm = Arc::new(ConformanceLlm);
    let tools = Arc::new(ConformanceToolExecutor);
    registry.register_agent_provider_with_manifest(
        name,
        manifest,
        Arc::new(CairnAgentProvider::new(llm, tools)),
    )
}

struct ConformanceLlm;

#[async_trait::async_trait]
impl LLMProvider for ConformanceLlm {
    fn name(&self) -> &'static str {
        "cairn-agent-core-conformance-llm"
    }

    fn capabilities(&self) -> &LLMProviderCapabilities {
        static CAPS: LLMProviderCapabilities = LLMProviderCapabilities {
            json_mode: true,
            streaming: false,
            tool_calls: false,
        };
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
    }

    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionOutput, LlmError> {
        let prompt = request.prompt.as_str();
        let has_history = prompt.contains("Tool history:");
        let output = if prompt.contains(ALLOWLIST_REJECTS_UNLISTED_TOOL_PROMPT) {
            serde_json::json!({
                "action": "tool",
                "tool": { "verb": "forget", "write_report": false, "persist": false },
                "args": { "record_id": "01HQZX9F5N00000000000000AA" }
            })
        } else if prompt.contains(MUTATING_VERB_REQUIRES_SCOPE_PROMPT) {
            serde_json::json!({
                "action": "tool",
                "tool": { "verb": "ingest", "write_report": false, "persist": false },
                "args": { "body": "blocked mutation" }
            })
        } else if prompt.contains(BUDGET_EXHAUSTION_ABORTS_CLEANLY_PROMPT) {
            let query = if has_history {
                "second budget probe"
            } else {
                "first budget probe"
            };
            serde_json::json!({
                "action": "tool",
                "tool": { "verb": "search", "write_report": false, "persist": false },
                "args": { "query": query }
            })
        } else if prompt.contains(WRITES_ARE_WAL_ROUTED_PROMPT) {
            if has_history {
                serde_json::json!({
                    "action": "final",
                    "output": "wal-routed mutation observed"
                })
            } else {
                serde_json::json!({
                    "action": "tool",
                    "tool": { "verb": "ingest", "write_report": false, "persist": false },
                    "args": { "body": "scoped mutation" }
                })
            }
        } else {
            return Err(LlmError::InvalidJsonOutput {
                detail: "unknown agent-provider conformance prompt".to_string(),
                raw: prompt.to_string(),
            });
        };
        Ok(CompletionOutput::Json(output))
    }
}

struct ConformanceToolExecutor;

#[async_trait::async_trait]
impl AgentToolExecutor for ConformanceToolExecutor {
    async fn execute(
        &self,
        call: &AgentToolCall,
        _args: serde_json::Value,
        _wall_clock_remaining: std::time::Duration,
    ) -> Result<ToolExecution, AgentProviderError> {
        Ok(ToolExecution {
            output: serde_json::json!({
                "verb": format!("{:?}", call.verb),
                "mutation": call.verb == CairnVerb::Ingest,
            }),
            cost_units: 1,
        })
    }
}
