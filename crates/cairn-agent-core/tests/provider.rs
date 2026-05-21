//! Deterministic provider runtime tests.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use cairn_agent_core::{
    AgentToolExecutor, CairnAgentProvider, ToolExecution, UnconfiguredCairnAgentProvider,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::contract::{
    AgentCostBudget, AgentIdentity, AgentOutput, AgentOutputSchema, AgentProvider,
    AgentProviderCapabilities, AgentProviderError, AgentRunStatus, AgentScope, AgentSpawnRequest,
    AgentToolAllowlist, AgentToolCall, AgentWallClockBudget, CairnVerb, CompletionOutput,
    CompletionRequest, LLMProvider, LLMProviderCapabilities, LlmError,
};
use cairn_core::contract::{PluginName, PluginRegistry};
use serde_json::json;

struct SequenceLlm {
    outputs: Mutex<VecDeque<CompletionOutput>>,
}

impl SequenceLlm {
    fn new(outputs: impl IntoIterator<Item = CompletionOutput>) -> Self {
        Self {
            outputs: Mutex::new(outputs.into_iter().collect()),
        }
    }
}

#[async_trait::async_trait]
impl LLMProvider for SequenceLlm {
    fn name(&self) -> &str {
        "sequence-llm"
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

    async fn complete(&self, _req: &CompletionRequest) -> Result<CompletionOutput, LlmError> {
        self.outputs
            .lock()
            .expect("sequence lock is available")
            .pop_front()
            .ok_or_else(|| LlmError::InvalidJsonOutput {
                detail: "sequence exhausted".to_string(),
                raw: String::new(),
            })
    }
}

struct RecordingToolExecutor {
    calls: Mutex<u32>,
    result: ToolExecution,
}

impl RecordingToolExecutor {
    fn new(result: ToolExecution) -> Self {
        Self {
            calls: Mutex::new(0),
            result,
        }
    }

    fn call_count(&self) -> u32 {
        *self.calls.lock().expect("call counter lock is available")
    }
}

#[async_trait::async_trait]
impl AgentToolExecutor for RecordingToolExecutor {
    async fn execute(
        &self,
        _call: &AgentToolCall,
        _args: serde_json::Value,
    ) -> Result<ToolExecution, AgentProviderError> {
        *self.calls.lock().expect("call counter lock is available") += 1;
        Ok(self.result.clone())
    }
}

fn request(output_schema: AgentOutputSchema, max_turns: u32) -> AgentSpawnRequest {
    AgentSpawnRequest {
        identity: AgentIdentity::new("agt:cairn-agent-core:test").expect("valid identity"),
        scope: AgentScope::read_only(),
        tool_allowlist: AgentToolAllowlist::read_only_cairn(),
        cost_budget: AgentCostBudget {
            max_turns,
            max_tool_calls: 4,
            max_cost_units: 100,
        },
        wall_clock_budget: AgentWallClockBudget { max_millis: 1_000 },
        output_schema,
        prompt: "Find related records".to_string(),
    }
}

#[tokio::test]
async fn provider_rejects_unlisted_tool_before_executor_runs() {
    let llm = Arc::new(SequenceLlm::new([CompletionOutput::Json(json!({
        "action": "tool",
        "tool": { "verb": "ingest", "write_report": false, "persist": false },
        "args": { "path": "notes.md" }
    }))]));
    let tools = Arc::new(RecordingToolExecutor::new(ToolExecution {
        output: json!({ "unexpected": true }),
        cost_units: 1,
    }));
    let provider = CairnAgentProvider::new(llm, tools.clone());

    let run = provider
        .spawn(request(AgentOutputSchema::Json, 3))
        .await
        .expect("policy denial returns an aborted run");

    assert_eq!(run.status, AgentRunStatus::Aborted);
    assert!(matches!(
        run.abort_error,
        Some(AgentProviderError::ToolNotAllowed {
            verb: CairnVerb::Ingest
        })
    ));
    assert_eq!(tools.call_count(), 0);
}

#[tokio::test]
async fn provider_returns_final_json_and_consumed_budget() {
    let llm = Arc::new(SequenceLlm::new([
        CompletionOutput::Json(json!({
            "action": "tool",
            "tool": { "verb": "search", "write_report": false, "persist": false },
            "args": { "query": "budget" }
        })),
        CompletionOutput::Json(json!({
            "action": "final",
            "output": { "answer": "found", "records": 1 }
        })),
    ]));
    let tools = Arc::new(RecordingToolExecutor::new(ToolExecution {
        output: json!({ "records": [{ "id": "rec_1" }] }),
        cost_units: 7,
    }));
    let provider = CairnAgentProvider::new(llm, tools);

    let run = provider
        .spawn(request(AgentOutputSchema::Json, 3))
        .await
        .expect("run completes");

    assert_eq!(run.status, AgentRunStatus::Completed);
    assert!(matches!(run.output, AgentOutput::Json(_)));
    assert_eq!(run.budget_consumed.turns, 2);
    assert_eq!(run.budget_consumed.tool_calls, 1);
    assert_eq!(run.budget_consumed.cost_units, 7);
}

#[tokio::test]
async fn provider_aborts_when_turn_budget_exhausted() {
    let llm = Arc::new(SequenceLlm::new([
        CompletionOutput::Json(json!({
            "action": "tool",
            "tool": { "verb": "search", "write_report": false, "persist": false },
            "args": { "query": "budget" }
        })),
        CompletionOutput::Json(json!({
            "action": "final",
            "output": { "answer": "too late" }
        })),
    ]));
    let tools = Arc::new(RecordingToolExecutor::new(ToolExecution {
        output: json!({ "records": [] }),
        cost_units: 1,
    }));
    let provider = CairnAgentProvider::new(llm, tools);

    let run = provider
        .spawn(request(AgentOutputSchema::Json, 1))
        .await
        .expect("budget exhaustion returns an aborted run");

    assert_eq!(run.status, AgentRunStatus::Aborted);
    assert!(matches!(
        run.abort_error,
        Some(AgentProviderError::BudgetExceeded { ref limit }) if limit == "turns"
    ));
    assert_eq!(run.budget_consumed.turns, 1);
}

#[tokio::test]
async fn unconfigured_provider_registers_but_spawn_unavailable() {
    let mut registry = PluginRegistry::new();
    cairn_agent_core::register(&mut registry).expect("provider registers");
    let name = PluginName::new("cairn-agent-core").expect("valid plugin name");
    assert!(registry.agent_provider(&name).is_some());

    let provider = UnconfiguredCairnAgentProvider::default();

    assert_eq!(provider.name(), "cairn-agent-core");
    assert_eq!(
        *provider.capabilities(),
        AgentProviderCapabilities {
            honors_cost_budget: true,
            scope_enforced: true,
            mcp_tools: false,
            cli_subprocess_tools: true,
        }
    );

    let err = provider
        .spawn(request(AgentOutputSchema::Text, 1))
        .await
        .expect_err("unconfigured provider cannot spawn");

    assert!(matches!(
        err,
        AgentProviderError::ProviderUnavailable { ref message }
            if message == "cairn-agent-core requires configured LLMProvider"
    ));
}
