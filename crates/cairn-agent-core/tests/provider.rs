//! Deterministic provider runtime tests.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cairn_agent_core::{
    AgentToolExecutor, CairnAgentProvider, CairnCliToolExecutor, ToolExecution,
    UnconfiguredCairnAgentProvider,
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
    fn name(&self) -> &'static str {
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

struct SlowLlm {
    delay: Duration,
    output: CompletionOutput,
}

impl SlowLlm {
    fn new(delay: Duration, output: CompletionOutput) -> Self {
        Self { delay, output }
    }
}

#[async_trait::async_trait]
impl LLMProvider for SlowLlm {
    fn name(&self) -> &'static str {
        "slow-llm"
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
        tokio::time::sleep(self.delay).await;
        Ok(self.output.clone())
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
        _wall_clock_remaining: Duration,
    ) -> Result<ToolExecution, AgentProviderError> {
        *self.calls.lock().expect("call counter lock is available") += 1;
        Ok(self.result.clone())
    }
}

struct SlowToolExecutor {
    delay: Duration,
    result: ToolExecution,
    honors_wall_clock: bool,
}

impl SlowToolExecutor {
    fn new(delay: Duration, result: ToolExecution) -> Self {
        Self {
            delay,
            result,
            honors_wall_clock: true,
        }
    }

    fn non_cooperative(delay: Duration, result: ToolExecution) -> Self {
        Self {
            delay,
            result,
            honors_wall_clock: false,
        }
    }
}

#[async_trait::async_trait]
impl AgentToolExecutor for SlowToolExecutor {
    async fn execute(
        &self,
        _call: &AgentToolCall,
        _args: serde_json::Value,
        wall_clock_remaining: Duration,
    ) -> Result<ToolExecution, AgentProviderError> {
        if self.honors_wall_clock && self.delay > wall_clock_remaining {
            tokio::time::sleep(wall_clock_remaining).await;
            return Err(AgentProviderError::BudgetExceeded {
                limit: "wall_clock".to_string(),
            });
        }
        tokio::time::sleep(self.delay).await;
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

fn request_with_wall_clock(
    output_schema: AgentOutputSchema,
    max_turns: u32,
    max_millis: u64,
) -> AgentSpawnRequest {
    AgentSpawnRequest {
        wall_clock_budget: AgentWallClockBudget { max_millis },
        ..request(output_schema, max_turns)
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

async fn assert_policy_denial_before_executor_runs(
    tool: serde_json::Value,
    args: serde_json::Value,
    expected: impl FnOnce(&AgentProviderError) -> bool,
) {
    let llm = Arc::new(SequenceLlm::new([CompletionOutput::Json(json!({
        "action": "tool",
        "tool": tool,
        "args": args
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
    let err = run
        .abort_error
        .as_ref()
        .expect("policy denial should preserve the abort error");
    assert!(expected(err), "unexpected abort error: {err:?}");
    assert_eq!(tools.call_count(), 0);
}

#[tokio::test]
async fn provider_blocks_forget_before_executor_runs() {
    assert_policy_denial_before_executor_runs(
        json!({ "verb": "forget", "write_report": false, "persist": false }),
        json!({ "record_id": "01HQZX9F5N00000000000000AA" }),
        |err| {
            matches!(
                err,
                AgentProviderError::ToolNotAllowed {
                    verb: CairnVerb::Forget
                } | AgentProviderError::MutatingVerbNotScoped {
                    verb: CairnVerb::Forget
                }
            )
        },
    )
    .await;
}

#[tokio::test]
async fn provider_blocks_lint_write_report_before_executor_runs() {
    assert_policy_denial_before_executor_runs(
        json!({ "verb": "lint", "write_report": true, "persist": false }),
        json!({ "plan": "weekly" }),
        |err| {
            matches!(
                err,
                AgentProviderError::ToolNotAllowed {
                    verb: CairnVerb::Lint
                } | AgentProviderError::MutatingVerbNotScoped {
                    verb: CairnVerb::Lint
                }
            )
        },
    )
    .await;
}

#[tokio::test]
async fn provider_blocks_search_persist_flag_before_executor_runs() {
    assert_policy_denial_before_executor_runs(
        json!({ "verb": "search", "write_report": false, "persist": true }),
        json!({ "query": "refund shard" }),
        |err| {
            matches!(
                err,
                AgentProviderError::InvalidRequest { message }
                    if message == "persist is valid only for summarize"
            )
        },
    )
    .await;
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
async fn provider_returns_final_text_for_text_schema() {
    let llm = Arc::new(SequenceLlm::new([CompletionOutput::Json(json!({
        "action": "final",
        "output": "done"
    }))]));
    let tools = Arc::new(RecordingToolExecutor::new(ToolExecution {
        output: json!({}),
        cost_units: 0,
    }));
    let provider = CairnAgentProvider::new(llm, tools);

    let run = provider
        .spawn(request(AgentOutputSchema::Text, 2))
        .await
        .expect("run returns trace state");

    assert_eq!(run.status, AgentRunStatus::Completed);
    assert_eq!(run.output, AgentOutput::Text("done".to_string()));
}

#[tokio::test]
async fn provider_aborts_final_non_string_for_text_schema() {
    let llm = Arc::new(SequenceLlm::new([CompletionOutput::Json(json!({
        "action": "final",
        "output": { "not": "text" }
    }))]));
    let tools = Arc::new(RecordingToolExecutor::new(ToolExecution {
        output: json!({}),
        cost_units: 0,
    }));
    let provider = CairnAgentProvider::new(llm, tools);

    let run = provider
        .spawn(request(AgentOutputSchema::Text, 2))
        .await
        .expect("invalid final output returns aborted run");

    assert_eq!(run.status, AgentRunStatus::Aborted);
    assert!(matches!(
        run.abort_error,
        Some(AgentProviderError::InvalidOutput { .. })
    ));
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
async fn provider_aborts_when_llm_exceeds_wall_clock_budget() {
    let llm = Arc::new(SlowLlm::new(
        Duration::from_millis(50),
        CompletionOutput::Json(json!({
            "action": "final",
            "output": { "answer": "late" }
        })),
    ));
    let tools = Arc::new(RecordingToolExecutor::new(ToolExecution {
        output: json!({}),
        cost_units: 0,
    }));
    let provider = CairnAgentProvider::new(llm, tools);

    let run = provider
        .spawn(request_with_wall_clock(AgentOutputSchema::Json, 2, 5))
        .await
        .expect("wall-clock exhaustion returns aborted run");

    assert_eq!(run.status, AgentRunStatus::Aborted);
    assert!(matches!(
        run.abort_error,
        Some(AgentProviderError::BudgetExceeded { ref limit }) if limit == "wall_clock"
    ));
}

#[tokio::test]
async fn provider_aborts_when_tool_exceeds_wall_clock_budget() {
    let llm = Arc::new(SequenceLlm::new([CompletionOutput::Json(json!({
        "action": "tool",
        "tool": { "verb": "search", "write_report": false, "persist": false },
        "args": { "query": "budget" }
    }))]));
    let tools = Arc::new(SlowToolExecutor::new(
        Duration::from_millis(50),
        ToolExecution {
            output: json!({ "records": [] }),
            cost_units: 1,
        },
    ));
    let provider = CairnAgentProvider::new(llm, tools);

    let run = provider
        .spawn(request_with_wall_clock(AgentOutputSchema::Json, 2, 5))
        .await
        .expect("wall-clock exhaustion returns aborted run");

    assert_eq!(run.status, AgentRunStatus::Aborted);
    assert!(matches!(
        run.abort_error,
        Some(AgentProviderError::BudgetExceeded { ref limit }) if limit == "wall_clock"
    ));
}

#[tokio::test]
async fn provider_times_out_non_cooperative_tool_executor() {
    let llm = Arc::new(SequenceLlm::new([CompletionOutput::Json(json!({
        "action": "tool",
        "tool": { "verb": "search", "write_report": false, "persist": false },
        "args": { "query": "budget" }
    }))]));
    let tools = Arc::new(SlowToolExecutor::non_cooperative(
        Duration::from_millis(100),
        ToolExecution {
            output: json!({ "records": [] }),
            cost_units: 1,
        },
    ));
    let provider = CairnAgentProvider::new(llm, tools);
    let started = std::time::Instant::now();

    let run = provider
        .spawn(request_with_wall_clock(AgentOutputSchema::Json, 2, 10))
        .await
        .expect("provider-level timeout returns aborted run");

    assert!(
        started.elapsed() < Duration::from_millis(80),
        "provider should not wait for a non-cooperative executor"
    );
    assert_eq!(run.status, AgentRunStatus::Aborted);
    assert!(matches!(
        run.abort_error,
        Some(AgentProviderError::BudgetExceeded { ref limit }) if limit == "wall_clock"
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn provider_kills_cli_subprocess_when_tool_exceeds_wall_clock_budget() {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let temp_root = std::env::temp_dir().join(format!(
        "cairn-agent-core-timeout-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&temp_root).expect("create temp root");
    let script_path = temp_root.join("sleeping-cairn");
    let marker_path = temp_root.join("marker");
    let mut script = std::fs::File::create(&script_path).expect("create script");
    writeln!(
        script,
        "#!/bin/sh\nsleep 1\nprintf marker > '{}'\nprintf '{{\"ok\":true}}\\n'\n",
        marker_path.display()
    )
    .expect("write script");
    let mut perms = std::fs::metadata(&script_path)
        .expect("script metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script_path, perms).expect("chmod script");

    let llm = Arc::new(SequenceLlm::new([CompletionOutput::Json(json!({
        "action": "tool",
        "tool": { "verb": "search", "write_report": false, "persist": false },
        "args": { "query": "budget" }
    }))]));
    let tools = Arc::new(CairnCliToolExecutor::new(script_path.to_string_lossy()));
    let provider = CairnAgentProvider::new(llm, tools);
    let started = std::time::Instant::now();

    let run = provider
        .spawn(request_with_wall_clock(AgentOutputSchema::Json, 2, 300))
        .await
        .expect("wall-clock exhaustion returns aborted run");

    assert!(
        started.elapsed() < Duration::from_millis(700),
        "provider should return before the script sleep completes"
    );
    assert_eq!(run.status, AgentRunStatus::Aborted);
    assert!(matches!(
        run.abort_error,
        Some(AgentProviderError::BudgetExceeded { ref limit }) if limit == "wall_clock"
    ));

    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(
        !marker_path.exists(),
        "timed-out subprocess should be killed before writing marker"
    );
    let _ = std::fs::remove_dir_all(temp_root);
}

#[tokio::test]
async fn unconfigured_provider_registers_but_spawn_unavailable() {
    let mut registry = PluginRegistry::new();
    cairn_agent_core::register(&mut registry).expect("provider registers");
    let name = PluginName::new("cairn-agent-core").expect("valid plugin name");
    assert!(registry.agent_provider(&name).is_some());

    let provider = UnconfiguredCairnAgentProvider;

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
