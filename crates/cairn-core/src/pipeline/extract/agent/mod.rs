//! Agent extractor parser, prompt renderer, and output schema.

mod parse;
mod prompt;
mod schema;

pub use parse::{AgentEvidence, AgentParseError, ParsedAgentResponse, parse_agent_response};
pub use prompt::render_agent_extract_prompt;
pub use schema::AGENT_EXTRACTOR_OUTPUT_SCHEMA;

use std::sync::Arc;

use crate::contract::agent_provider::{
    AgentCostBudget, AgentIdentity, AgentOutput, AgentOutputSchema, AgentProvider,
    AgentProviderError, AgentRunStatus, AgentScope, AgentSpawnRequest, AgentToolAllowlist,
    AgentWallClockBudget,
};
use crate::domain::CaptureEventId;
use crate::pipeline::extract::body::BodyResolution;
use crate::pipeline::extract::{
    ExtractBudget, ExtractError, ExtractInput, ExtractOutput, ExtractResult, ExtractorWorker,
    TextSpan, TruncationReason, WorkerRole,
};

const AGENT_EXTRACTOR_IDENTITY: &str = "agt:cairn-extractor:v1";
const DEFAULT_AGENT_TURN_BUDGET: u32 = 4;
const DEFAULT_AGENT_TOOL_CALL_BUDGET: u32 = 4;

/// Agent-backed augmenting extractor over the read-only [`AgentProvider`]
/// contract.
pub struct AgentExtractor {
    provider: Arc<dyn AgentProvider>,
    budget: ExtractBudget,
    max_turns: u32,
    max_tool_calls: u32,
}

impl AgentExtractor {
    /// Construct an `AgentExtractor` over the given provider with the
    /// default agent budget.
    #[must_use]
    pub fn new(provider: Arc<dyn AgentProvider>) -> Self {
        Self {
            provider,
            budget: ExtractBudget::agent_default(),
            max_turns: DEFAULT_AGENT_TURN_BUDGET,
            max_tool_calls: DEFAULT_AGENT_TOOL_CALL_BUDGET,
        }
    }

    /// Override the budget. Returns `self` for chaining.
    #[must_use]
    pub fn with_budget(mut self, budget: ExtractBudget) -> Self {
        self.budget = budget;
        self
    }

    /// Override the agent turn and tool-call budgets. Zero normalizes to the
    /// default so provider request validation remains fail-closed.
    #[must_use]
    pub fn with_turn_budget(mut self, max_turns: u32) -> Self {
        let max_turns = if max_turns == 0 {
            DEFAULT_AGENT_TURN_BUDGET
        } else {
            max_turns
        };
        self.max_turns = max_turns;
        self.max_tool_calls = max_turns;
        self
    }

    fn spawn_request(
        &self,
        source_event: &CaptureEventId,
        prompt_source_text: &str,
        spans: &[TextSpan],
    ) -> Result<AgentSpawnRequest, AgentProviderError> {
        let request = AgentSpawnRequest {
            identity: AgentIdentity::new(AGENT_EXTRACTOR_IDENTITY)?,
            scope: AgentScope::read_only(),
            tool_allowlist: AgentToolAllowlist::read_only_cairn(),
            cost_budget: AgentCostBudget {
                max_turns: self.max_turns,
                max_tool_calls: self.max_tool_calls,
                max_cost_units: u64::from(self.budget.max_response_tokens.unwrap_or(4096).max(1)),
            },
            wall_clock_budget: AgentWallClockBudget {
                max_millis: u64::from(self.budget.max_wall_ms.max(1)),
            },
            output_schema: AgentOutputSchema::Json,
            prompt: render_agent_extract_prompt(source_event, prompt_source_text, spans),
        };
        request.validate()?;
        Ok(request)
    }
}

fn empty_result() -> ExtractResult {
    ExtractResult {
        outputs: vec![],
        discards: vec![],
        truncated: TruncationReason::None,
        llm_eligible_spans: vec![],
    }
}

fn empty_with_truncation(t: TruncationReason) -> ExtractResult {
    ExtractResult {
        truncated: t,
        ..empty_result()
    }
}

fn agent_error(source: AgentProviderError) -> ExtractError {
    ExtractError::AgentProvider {
        worker: "agent",
        source,
    }
}

fn eligible_prompt_source(body: &str, spans: &[TextSpan]) -> String {
    let body_len = u32::try_from(body.len()).unwrap_or(u32::MAX);
    if spans.len() == 1 && spans[0].start == 0 && spans[0].end == body_len {
        return body.to_owned();
    }

    let mut source = String::new();
    source.push_str(
        "Only the following eligible excerpts are available. Output spans must use original source byte offsets.\n",
    );
    for span in spans {
        let start = span.start as usize;
        let end = span.end as usize;
        let Some(excerpt) = body.get(start..end) else {
            tracing::warn!(
                start = span.start,
                end = span.end,
                "agent.eligible_span_not_utf8_boundary"
            );
            continue;
        };
        source.push_str("\noriginal_bytes ");
        source.push_str(&span.start.to_string());
        source.push_str("..");
        source.push_str(&span.end.to_string());
        source.push_str(":\n");
        source.push_str(excerpt);
        source.push('\n');
    }
    source
}

#[async_trait::async_trait]
impl ExtractorWorker for AgentExtractor {
    fn name(&self) -> &'static str {
        "agent"
    }

    fn role(&self) -> WorkerRole {
        WorkerRole::Augmenting
    }

    fn budget(&self) -> ExtractBudget {
        self.budget
    }

    async fn extract(&self, input: &ExtractInput<'_>) -> Result<ExtractResult, ExtractError> {
        let body = match &input.body {
            BodyResolution::NotApplicable => return Ok(empty_result()),
            BodyResolution::Failed(e) => {
                return Err(ExtractError::BodyResolution {
                    event_id: input.event.event_id.as_str().to_owned(),
                    source: e.clone(),
                });
            }
            BodyResolution::Resolved(rb) => rb.text(),
        };

        if body.is_empty() {
            return Ok(empty_result());
        }

        let eligible_spans = match &input.eligible_spans {
            None => {
                let body_len = u32::try_from(body.len()).unwrap_or(u32::MAX);
                if body_len == 0 {
                    return Ok(empty_result());
                }
                vec![TextSpan::new(0, body_len)]
            }
            Some(spans) if spans.is_empty() => return Ok(empty_result()),
            Some(spans) => spans.clone(),
        };

        let request = self
            .spawn_request(
                &input.event.event_id,
                &eligible_prompt_source(body, &eligible_spans),
                &eligible_spans,
            )
            .map_err(agent_error)?;

        if self
            .budget
            .max_prompt_bytes
            .is_some_and(|cap| request.prompt.len() > cap as usize)
        {
            tracing::warn!(
                reason = "agent.prompt_size_byte_cap_skip",
                prompt_bytes = request.prompt.len(),
                cap = self.budget.max_prompt_bytes,
            );
            return Ok(empty_with_truncation(TruncationReason::MaxWallMs {
                elapsed_ms: 0,
            }));
        }

        let timeout_dur =
            std::time::Duration::from_millis(u64::from(self.budget.max_wall_ms.max(1)));
        let started = std::time::Instant::now();
        let run = tokio::time::timeout(timeout_dur, self.provider.spawn(request))
            .await
            .map_err(|_| {
                let elapsed_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
                ExtractError::BudgetExceeded {
                    worker: "agent",
                    elapsed_ms,
                }
            })?
            .map_err(agent_error)?;

        if run.status == AgentRunStatus::Aborted {
            let source = run
                .abort_error
                .unwrap_or_else(|| AgentProviderError::InvalidOutput {
                    message: "aborted agent run did not include abort_error".to_owned(),
                });
            return Err(agent_error(source));
        }

        let output_schema = AgentOutputSchema::Json;
        run.validate(&output_schema).map_err(agent_error)?;

        let AgentOutput::Json(value) = run.output else {
            return Err(agent_error(AgentProviderError::InvalidOutput {
                message: "completed agent run did not return json output".to_owned(),
            }));
        };

        let parsed = parse_agent_response(&input.event.event_id, body, value).map_err(|err| {
            agent_error(AgentProviderError::InvalidOutput {
                message: err.to_string(),
            })
        })?;

        let mut outputs: Vec<ExtractOutput> = parsed
            .drafts
            .into_iter()
            .map(ExtractOutput::Draft)
            .collect();
        let truncated = if outputs.len() > usize::from(self.budget.max_drafts) {
            outputs.truncate(usize::from(self.budget.max_drafts));
            TruncationReason::MaxDrafts
        } else {
            TruncationReason::None
        };

        Ok(ExtractResult {
            outputs,
            discards: parsed.discards,
            truncated,
            llm_eligible_spans: vec![],
        })
    }
}
