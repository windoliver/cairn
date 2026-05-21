use std::sync::Arc;

use cairn_core::config::ExtractBudget;
use cairn_core::contract::agent_provider::CONTRACT_VERSION;
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::contract::{
    AgentBudgetConsumed, AgentOutput, AgentOutputSchema, AgentProvider, AgentProviderCapabilities,
    AgentProviderError, AgentProviderPlugin, AgentRun, AgentRunMeter, AgentRunStatus,
    AgentSpawnRequest, AgentToolAttempt, AgentToolPolicyOutcome, CompletionOutput,
    CompletionRequest, LLMProvider, LlmError, evaluate_tool_policy,
};
use tokio::time::{Duration, Instant, timeout};

use crate::action::{AgentAction, parse_action};
use crate::tool::AgentToolExecutor;

/// Bundled bounded agent runtime over an [`LLMProvider`] and CLI tool executor.
pub struct CairnAgentProvider {
    llm: Arc<dyn LLMProvider>,
    tools: Arc<dyn AgentToolExecutor>,
    capabilities: AgentProviderCapabilities,
}

impl CairnAgentProvider {
    /// Build a configured bundled provider.
    #[must_use]
    pub fn new(llm: Arc<dyn LLMProvider>, tools: Arc<dyn AgentToolExecutor>) -> Self {
        Self {
            llm,
            tools,
            capabilities: capabilities(),
        }
    }
}

#[async_trait::async_trait]
impl AgentProvider for CairnAgentProvider {
    fn name(&self) -> &str {
        "cairn-agent-core"
    }

    fn capabilities(&self) -> &AgentProviderCapabilities {
        &self.capabilities
    }

    fn supported_contract_versions(&self) -> VersionRange {
        supported_versions()
    }

    async fn spawn(&self, request: AgentSpawnRequest) -> Result<AgentRun, AgentProviderError> {
        request.validate()?;

        let mut meter = AgentRunMeter::new(&request);
        let mut tool_calls = Vec::new();
        let mut policy_trace = Vec::new();
        let mut history = Vec::new();
        let deadline = Instant::now() + Duration::from_millis(request.wall_clock_budget.max_millis);

        for _ in 0..request.cost_budget.max_turns {
            if let Err(err) = meter.charge_turn(1) {
                return Ok(aborted_run(err, &meter, tool_calls, policy_trace));
            }

            let Some(remaining) = remaining_wall_clock(deadline) else {
                return Ok(aborted_run(
                    wall_clock_exceeded(),
                    &meter,
                    tool_calls,
                    policy_trace,
                ));
            };
            let completion_request = CompletionRequest::builder()
                .prompt(render_prompt(&request.prompt, &history))
                .maybe_budget(completion_budget(&request))
                .build();
            let completion = match timeout(remaining, self.llm.complete(&completion_request)).await
            {
                Ok(Ok(completion)) => completion,
                Ok(Err(err)) => {
                    let mapped = map_llm_error(err);
                    return Ok(aborted_run(mapped, &meter, tool_calls, policy_trace));
                }
                Err(_elapsed) => {
                    return Ok(aborted_run(
                        wall_clock_exceeded(),
                        &meter,
                        tool_calls,
                        policy_trace,
                    ));
                }
            };

            let action = match completion_to_action(completion) {
                Ok(action) => action,
                Err(err) => return Ok(aborted_run(err, &meter, tool_calls, policy_trace)),
            };

            match action {
                AgentAction::Tool { tool, args } => {
                    let outcome = match evaluate_tool_policy(&request, &tool) {
                        Ok(outcome) => outcome,
                        Err(err) => {
                            tool_calls.push(AgentToolAttempt {
                                call: tool.clone(),
                                outcome: AgentToolPolicyOutcome::Denied,
                                reason: err.to_string(),
                            });
                            policy_trace.push(format!("{:?}:Denied", tool.verb));
                            return Ok(aborted_run(err, &meter, tool_calls, policy_trace));
                        }
                    };

                    if let Err(err) = meter.charge_tool_call() {
                        return Ok(aborted_run(err, &meter, tool_calls, policy_trace));
                    }

                    tool_calls.push(AgentToolAttempt {
                        call: tool.clone(),
                        outcome,
                        reason: format!("{outcome:?}"),
                    });
                    policy_trace.push(format!("{:?}:{outcome:?}", tool.verb));

                    let Some(remaining) = remaining_wall_clock(deadline) else {
                        return Ok(aborted_run(
                            wall_clock_exceeded(),
                            &meter,
                            tool_calls,
                            policy_trace,
                        ));
                    };
                    let execution = match timeout(
                        remaining,
                        self.tools.execute(&tool, args, remaining),
                    )
                    .await
                    {
                        Ok(Ok(execution)) => execution,
                        Ok(Err(err)) => {
                            return Ok(aborted_run(err, &meter, tool_calls, policy_trace));
                        }
                        Err(_elapsed) => {
                            return Ok(aborted_run(
                                wall_clock_exceeded(),
                                &meter,
                                tool_calls,
                                policy_trace,
                            ));
                        }
                    };

                    if let Err(err) = meter.charge_cost_units(execution.cost_units) {
                        return Ok(aborted_run(err, &meter, tool_calls, policy_trace));
                    }
                    history.push(render_tool_history(&tool, &execution.output));
                }
                AgentAction::Final { output } => {
                    let output = match final_output_for_schema(output, &request.output_schema) {
                        Ok(output) => output,
                        Err(err) => return Ok(aborted_run(err, &meter, tool_calls, policy_trace)),
                    };
                    let run = AgentRun {
                        status: AgentRunStatus::Completed,
                        abort_error: None,
                        output,
                        budget_consumed: meter.consumed(),
                        tool_calls,
                        policy_trace,
                    };
                    run.validate(&request.output_schema)?;
                    return Ok(run);
                }
            }
        }

        Ok(aborted_run(
            AgentProviderError::BudgetExceeded {
                limit: "turns".to_string(),
            },
            &meter,
            tool_calls,
            policy_trace,
        ))
    }
}

/// Manifest-registered provider placeholder. Hosts must construct
/// [`CairnAgentProvider`] with an LLM before it can execute.
#[derive(Debug, Default)]
pub struct UnconfiguredCairnAgentProvider;

#[async_trait::async_trait]
impl AgentProvider for UnconfiguredCairnAgentProvider {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn capabilities(&self) -> &AgentProviderCapabilities {
        static CAPS: AgentProviderCapabilities = AgentProviderCapabilities {
            honors_cost_budget: true,
            scope_enforced: true,
            mcp_tools: false,
            cli_subprocess_tools: true,
        };
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        Self::SUPPORTED_VERSIONS
    }

    async fn spawn(&self, _request: AgentSpawnRequest) -> Result<AgentRun, AgentProviderError> {
        Err(AgentProviderError::ProviderUnavailable {
            message: "cairn-agent-core requires configured LLMProvider".to_string(),
        })
    }
}

impl AgentProviderPlugin for UnconfiguredCairnAgentProvider {
    const NAME: &'static str = "cairn-agent-core";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(CONTRACT_VERSION, ContractVersion::new(0, 2, 0));
}

fn capabilities() -> AgentProviderCapabilities {
    AgentProviderCapabilities {
        honors_cost_budget: true,
        scope_enforced: true,
        mcp_tools: false,
        cli_subprocess_tools: true,
    }
}

fn supported_versions() -> VersionRange {
    VersionRange::new(CONTRACT_VERSION, ContractVersion::new(0, 2, 0))
}

fn completion_budget(request: &AgentSpawnRequest) -> Option<ExtractBudget> {
    Some(ExtractBudget {
        max_tokens: Some(request.cost_budget.max_cost_units.min(u64::from(u32::MAX)) as u32),
        max_wall_ms: Some(
            request
                .wall_clock_budget
                .max_millis
                .min(u64::from(u32::MAX)) as u32,
        ),
        max_turns: Some(request.cost_budget.max_turns),
    })
}

fn completion_to_action(completion: CompletionOutput) -> Result<AgentAction, AgentProviderError> {
    match completion {
        CompletionOutput::Json(value) => parse_action(value),
        CompletionOutput::Text(text) => {
            let value = serde_json::from_str(&text).map_err(|source| {
                AgentProviderError::InvalidOutput {
                    message: source.to_string(),
                }
            })?;
            parse_action(value)
        }
        _ => Err(AgentProviderError::InvalidOutput {
            message: "unsupported completion output variant".to_string(),
        }),
    }
}

fn final_output_for_schema(
    output: serde_json::Value,
    schema: &AgentOutputSchema,
) -> Result<AgentOutput, AgentProviderError> {
    match schema {
        AgentOutputSchema::Json => Ok(AgentOutput::Json(output)),
        AgentOutputSchema::Text => output
            .as_str()
            .map(|text| AgentOutput::Text(text.to_string()))
            .ok_or_else(|| AgentProviderError::InvalidOutput {
                message: "text output schema requires string final output".to_string(),
            }),
    }
}

fn remaining_wall_clock(deadline: Instant) -> Option<Duration> {
    deadline.checked_duration_since(Instant::now())
}

fn wall_clock_exceeded() -> AgentProviderError {
    AgentProviderError::BudgetExceeded {
        limit: "wall_clock".to_string(),
    }
}

fn render_prompt(prompt: &str, history: &[String]) -> String {
    if history.is_empty() {
        return format!(
            "{prompt}\n\nRespond with JSON action: {{\"action\":\"tool\", ...}} or {{\"action\":\"final\", \"output\": ...}}."
        );
    }
    format!(
        "{prompt}\n\nTool history:\n{}\n\nRespond with the next JSON action.",
        history.join("\n")
    )
}

fn render_tool_history(
    tool: &cairn_core::contract::AgentToolCall,
    output: &serde_json::Value,
) -> String {
    let compact = serde_json::to_string(output).unwrap_or_else(|_| "null".to_string());
    format!("{:?}: {compact}", tool.verb)
}

fn aborted_run(
    err: AgentProviderError,
    meter: &AgentRunMeter,
    tool_calls: Vec<AgentToolAttempt>,
    policy_trace: Vec<String>,
) -> AgentRun {
    AgentRun {
        status: AgentRunStatus::Aborted,
        abort_error: Some(err),
        output: AgentOutput::Empty,
        budget_consumed: consumed(meter),
        tool_calls,
        policy_trace,
    }
}

fn consumed(meter: &AgentRunMeter) -> AgentBudgetConsumed {
    meter.consumed()
}

fn map_llm_error(err: LlmError) -> AgentProviderError {
    match err {
        LlmError::BudgetExceeded => AgentProviderError::BudgetExceeded {
            limit: "llm".to_string(),
        },
        LlmError::NotConfigured { remediation } => AgentProviderError::ProviderUnavailable {
            message: remediation,
        },
        LlmError::ProviderUnreachable { detail } => {
            AgentProviderError::ProviderUnavailable { message: detail }
        }
        LlmError::AuthDenied => AgentProviderError::ProviderUnavailable {
            message: "llm auth denied".to_string(),
        },
        LlmError::CapabilityMissing { capability } => AgentProviderError::ProviderUnavailable {
            message: format!("llm capability missing: {capability}"),
        },
        LlmError::InvalidJsonOutput { detail, raw } => AgentProviderError::InvalidOutput {
            message: if raw.is_empty() {
                detail
            } else {
                format!("{detail}: {raw}")
            },
        },
        _ => AgentProviderError::ProviderUnavailable {
            message: "unsupported llm error".to_string(),
        },
    }
}
