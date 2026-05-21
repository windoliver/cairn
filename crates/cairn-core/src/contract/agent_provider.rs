//! `AgentProvider` contract (brief §4 row 6).
//!
//! Agent providers spawn constrained agent-mode workers for `AgentExtractor`
//! and `AgentDreamWorker` flows. The contract surface is pure data and policy:
//! process spawning, model calls, and vault I/O live in implementation crates.

use crate::contract::version::{ContractVersion, VersionRange};

/// Contract version for `AgentProvider`. Bumps when the trait surface changes.
pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(0, 1, 0);

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

/// Stable identity for a spawned agent.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(transparent)]
pub struct AgentIdentity(String);

impl AgentIdentity {
    /// Build an agent identity.
    ///
    /// # Errors
    ///
    /// Returns [`AgentProviderError::InvalidRequest`] when the identity is
    /// empty or does not use the `agt:` prefix required for agent-mode workers.
    pub fn new(raw: impl Into<String>) -> Result<Self, AgentProviderError> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(AgentProviderError::invalid_request(
                "agent identity is empty",
            ));
        }
        if raw.chars().any(char::is_whitespace) {
            return Err(AgentProviderError::invalid_request(
                "agent identity must not contain whitespace",
            ));
        }
        if !raw.starts_with("agt:") {
            return Err(AgentProviderError::invalid_request(
                "agent identity must start with `agt:`",
            ));
        }
        if raw.strip_prefix("agt:").is_some_and(str::is_empty) {
            return Err(AgentProviderError::invalid_request(
                "agent identity must include a non-empty agent name",
            ));
        }
        Ok(Self(raw))
    }

    /// Return the identity as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> serde::Deserialize<'de> for AgentIdentity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = <String as serde::Deserialize>::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

/// Scope granted to an agent-mode run.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentScope {
    /// Mutating `cairn` verbs explicitly allowed for this run.
    pub mutations: Vec<CairnVerb>,
}

impl AgentScope {
    /// Read-only scope: no vault mutations are granted.
    #[must_use]
    pub fn read_only() -> Self {
        Self {
            mutations: Vec::new(),
        }
    }

    /// Scope that grants the supplied mutating verbs.
    #[must_use]
    pub fn with_mutations(mutations: Vec<CairnVerb>) -> Self {
        Self { mutations }
    }

    /// Returns true when the scope explicitly grants this mutating verb.
    #[must_use]
    pub fn permits_mutation(&self, verb: CairnVerb) -> bool {
        self.mutations.contains(&verb)
    }
}

/// Cairn CLI verbs visible to agent-mode tool policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CairnVerb {
    /// `cairn ingest`
    Ingest,
    /// `cairn search`
    Search,
    /// `cairn retrieve`
    Retrieve,
    /// `cairn summarize`
    Summarize,
    /// `cairn assemble_hot`
    AssembleHot,
    /// `cairn capture_trace`
    CaptureTrace,
    /// `cairn lint`
    Lint,
    /// `cairn forget`
    Forget,
}

impl CairnVerb {
    /// True when the verb mutates the vault without extra argument context.
    #[must_use]
    pub const fn is_mutating(self) -> bool {
        matches!(self, Self::Ingest | Self::CaptureTrace | Self::Forget)
    }

    /// True when the verb has any argument mode that can mutate the vault.
    #[must_use]
    pub const fn can_mutate(self) -> bool {
        matches!(
            self,
            Self::Ingest | Self::Summarize | Self::CaptureTrace | Self::Lint | Self::Forget
        )
    }
}

/// A proposed `cairn` CLI tool call, before execution.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentToolCall {
    /// Verb being requested.
    pub verb: CairnVerb,
    /// Whether `lint` writes `.cairn/lint-report.md`.
    pub write_report: bool,
    /// Whether `summarize` requests persistence.
    pub persist: bool,
}

impl AgentToolCall {
    /// Build a tool call with no special flags.
    #[must_use]
    pub const fn new(verb: CairnVerb) -> Self {
        Self {
            verb,
            write_report: false,
            persist: false,
        }
    }

    /// Build read-only `cairn lint` without report writing.
    #[must_use]
    pub const fn lint_dry() -> Self {
        Self {
            verb: CairnVerb::Lint,
            write_report: false,
            persist: false,
        }
    }

    /// Build `cairn summarize` in write/persist mode.
    #[must_use]
    pub const fn summarize_persist() -> Self {
        Self {
            verb: CairnVerb::Summarize,
            write_report: false,
            persist: true,
        }
    }

    /// True when this call can mutate the vault.
    #[must_use]
    pub const fn is_mutating(&self) -> bool {
        self.verb.is_mutating()
            || matches!(self.verb, CairnVerb::Summarize) && self.persist
            || matches!(self.verb, CairnVerb::Lint) && self.write_report
    }

    /// Validate that verb-specific flags are attached only to their owning verbs.
    ///
    /// # Errors
    ///
    /// Returns [`AgentProviderError::InvalidRequest`] when `persist` is set on
    /// a non-`summarize` call or `write_report` is set on a non-`lint` call.
    pub fn validate(&self) -> Result<(), AgentProviderError> {
        if self.persist && !matches!(self.verb, CairnVerb::Summarize) {
            return Err(AgentProviderError::invalid_request(
                "persist is valid only for summarize",
            ));
        }
        if self.write_report && !matches!(self.verb, CairnVerb::Lint) {
            return Err(AgentProviderError::invalid_request(
                "write_report is valid only for lint",
            ));
        }
        Ok(())
    }
}

/// Tool calls an agent-mode run may attempt.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentToolAllowlist {
    /// Allowed calls.
    pub tools: Vec<AgentToolCall>,
}

impl AgentToolAllowlist {
    /// Default read-only `cairn` CLI allowlist.
    #[must_use]
    pub fn read_only_cairn() -> Self {
        Self {
            tools: vec![
                AgentToolCall::new(CairnVerb::Search),
                AgentToolCall::new(CairnVerb::Retrieve),
                AgentToolCall::lint_dry(),
            ],
        }
    }

    /// True when a proposed call exactly matches an allowlist entry.
    #[must_use]
    pub fn allows(&self, call: &AgentToolCall) -> bool {
        self.tools.contains(call)
    }
}

/// Cost limits for an agent-mode run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCostBudget {
    /// Maximum agent turns.
    pub max_turns: u32,
    /// Maximum tool calls.
    pub max_tool_calls: u32,
    /// Abstract provider-defined cost units, usually token-equivalent.
    pub max_cost_units: u64,
}

/// Wall-clock limit for an agent-mode run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentWallClockBudget {
    /// Maximum elapsed milliseconds.
    pub max_millis: u64,
}

/// Output mode requested by the caller.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentOutputSchema {
    /// Unstructured text output.
    Text,
    /// JSON output without a stricter schema.
    Json,
}

/// Request passed to [`AgentProvider::spawn`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSpawnRequest {
    /// Identity the spawned worker runs as.
    pub identity: AgentIdentity,
    /// Read/write scope granted to this worker.
    pub scope: AgentScope,
    /// Tool calls the worker may attempt.
    pub tool_allowlist: AgentToolAllowlist,
    /// Cost budget for the run.
    pub cost_budget: AgentCostBudget,
    /// Wall-clock budget for the run.
    pub wall_clock_budget: AgentWallClockBudget,
    /// Expected output mode.
    pub output_schema: AgentOutputSchema,
    /// Opaque task text.
    pub prompt: String,
}

impl AgentSpawnRequest {
    /// Validate local request invariants before a provider starts work.
    ///
    /// # Errors
    ///
    /// Returns [`AgentProviderError::InvalidRequest`] for empty prompts, empty
    /// allowlists, invalid tool flags, zero budgets, or non-mutating verbs
    /// listed as mutation grants.
    pub fn validate(&self) -> Result<(), AgentProviderError> {
        if self.prompt.trim().is_empty() {
            return Err(AgentProviderError::invalid_request("prompt is empty"));
        }
        if self.tool_allowlist.tools.is_empty() {
            return Err(AgentProviderError::invalid_request(
                "tool allowlist is empty",
            ));
        }
        for tool in &self.tool_allowlist.tools {
            tool.validate()?;
        }
        if self.cost_budget.max_turns == 0 {
            return Err(AgentProviderError::invalid_request(
                "max_turns must be nonzero",
            ));
        }
        if self.cost_budget.max_tool_calls == 0 {
            return Err(AgentProviderError::invalid_request(
                "max_tool_calls must be nonzero",
            ));
        }
        if self.cost_budget.max_cost_units == 0 {
            return Err(AgentProviderError::invalid_request(
                "max_cost_units must be nonzero",
            ));
        }
        if self.wall_clock_budget.max_millis == 0 {
            return Err(AgentProviderError::invalid_request(
                "max_millis must be nonzero",
            ));
        }
        if self.scope.mutations.iter().any(|verb| !verb.can_mutate()) {
            return Err(AgentProviderError::invalid_request(
                "scope.mutations may contain only verbs with mutating modes",
            ));
        }
        Ok(())
    }
}

/// Evaluate a proposed tool call against the request allowlist and scope.
///
/// # Errors
///
/// Returns [`AgentProviderError::InvalidRequest`] for invalid flag
/// combinations, [`AgentProviderError::ToolNotAllowed`] when the call is not
/// exactly allowlisted, or [`AgentProviderError::MutatingVerbNotScoped`] when a
/// mutating allowlisted call lacks write scope.
pub fn evaluate_tool_policy(
    request: &AgentSpawnRequest,
    call: &AgentToolCall,
) -> Result<AgentToolPolicyOutcome, AgentProviderError> {
    call.validate()?;
    if !request.tool_allowlist.allows(call) {
        return Err(AgentProviderError::ToolNotAllowed { verb: call.verb });
    }
    if call.is_mutating() {
        if request.scope.permits_mutation(call.verb) {
            return Ok(AgentToolPolicyOutcome::AllowedWalRoutedMutation);
        }
        return Err(AgentProviderError::MutatingVerbNotScoped { verb: call.verb });
    }
    Ok(AgentToolPolicyOutcome::AllowedReadOnly)
}

/// In-memory cost budget meter for an agent-mode run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRunMeter {
    budget: AgentCostBudget,
    consumed: AgentBudgetConsumed,
}

impl AgentRunMeter {
    /// Build a meter initialized from a spawn request budget.
    #[must_use]
    pub const fn new(request: &AgentSpawnRequest) -> Self {
        Self {
            budget: request.cost_budget,
            consumed: AgentBudgetConsumed {
                turns: 0,
                tool_calls: 0,
                cost_units: 0,
            },
        }
    }

    /// Return the consumed budget counters.
    #[must_use]
    pub const fn consumed(&self) -> AgentBudgetConsumed {
        self.consumed
    }

    /// Charge agent turns against the request budget.
    ///
    /// # Errors
    ///
    /// Returns [`AgentProviderError::BudgetExceeded`] if the charge would
    /// overflow or exceed `max_turns`. Failed charges do not mutate counters.
    pub fn charge_turn(&mut self, turns: u32) -> Result<(), AgentProviderError> {
        let Some(next_turns) = self.consumed.turns.checked_add(turns) else {
            return Err(Self::budget_exceeded("turns"));
        };
        if next_turns > self.budget.max_turns {
            return Err(Self::budget_exceeded("turns"));
        }
        self.consumed.turns = next_turns;
        Ok(())
    }

    /// Charge one admitted tool call against the request budget.
    ///
    /// # Errors
    ///
    /// Returns [`AgentProviderError::BudgetExceeded`] if the charge would
    /// overflow or exceed `max_tool_calls`. Failed charges do not mutate
    /// counters.
    pub fn charge_tool_call(&mut self) -> Result<(), AgentProviderError> {
        let Some(next_tool_calls) = self.consumed.tool_calls.checked_add(1) else {
            return Err(Self::budget_exceeded("tool_calls"));
        };
        if next_tool_calls > self.budget.max_tool_calls {
            return Err(Self::budget_exceeded("tool_calls"));
        }
        self.consumed.tool_calls = next_tool_calls;
        Ok(())
    }

    /// Charge provider-defined cost units against the request budget.
    ///
    /// # Errors
    ///
    /// Returns [`AgentProviderError::BudgetExceeded`] if the charge would
    /// overflow or exceed `max_cost_units`. Failed charges do not mutate
    /// counters.
    pub fn charge_cost_units(&mut self, units: u64) -> Result<(), AgentProviderError> {
        let Some(next_cost_units) = self.consumed.cost_units.checked_add(units) else {
            return Err(Self::budget_exceeded("cost_units"));
        };
        if next_cost_units > self.budget.max_cost_units {
            return Err(Self::budget_exceeded("cost_units"));
        }
        self.consumed.cost_units = next_cost_units;
        Ok(())
    }

    fn budget_exceeded(limit: &str) -> AgentProviderError {
        AgentProviderError::BudgetExceeded {
            limit: limit.to_string(),
        }
    }
}

/// Validate a provider output value against the requested output schema.
///
/// # Errors
///
/// Returns [`AgentProviderError::InvalidOutput`] when non-empty output does not
/// match the requested schema.
pub fn validate_output(
    schema: &AgentOutputSchema,
    output: &AgentOutput,
) -> Result<(), AgentProviderError> {
    match (schema, output) {
        (_, AgentOutput::Empty)
        | (AgentOutputSchema::Text, AgentOutput::Text(_))
        | (AgentOutputSchema::Json, AgentOutput::Json(_)) => Ok(()),
        (AgentOutputSchema::Text, AgentOutput::Json(_)) => Err(AgentProviderError::InvalidOutput {
            message: "text output schema rejects json output".to_string(),
        }),
        (AgentOutputSchema::Json, AgentOutput::Text(_)) => Err(AgentProviderError::InvalidOutput {
            message: "json output schema rejects text output".to_string(),
        }),
    }
}

/// Agent run status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunStatus {
    /// Run completed and produced output.
    Completed,
    /// Run aborted with a typed error.
    Aborted,
}

/// Agent run output.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentOutput {
    /// No output, normally because the run aborted.
    Empty,
    /// Text output.
    Text(String),
    /// JSON output.
    Json(serde_json::Value),
}

/// Consumed budget counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentBudgetConsumed {
    /// Turns consumed.
    pub turns: u32,
    /// Tool calls admitted.
    pub tool_calls: u32,
    /// Cost units consumed.
    pub cost_units: u64,
}

/// Policy outcome for a proposed tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentToolPolicyOutcome {
    /// Call is allowed and read-only.
    AllowedReadOnly,
    /// Call is allowed and must route through the WAL-backed write path.
    AllowedWalRoutedMutation,
    /// Call was denied before execution.
    Denied,
}

/// Recorded tool attempt.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentToolAttempt {
    /// Proposed call.
    pub call: AgentToolCall,
    /// Policy outcome.
    pub outcome: AgentToolPolicyOutcome,
    /// Short reason string for traces and conformance reports.
    pub reason: String,
}

/// Completed or aborted agent run.
///
/// `Err(_)` from [`AgentProvider::spawn`] is for providers aborting before they
/// can return trace or budget state. `Ok(AgentRun { status: Aborted,
/// abort_error: Some(_), .. })` is for providers returning trace and budget
/// state with a typed abort reason.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRun {
    /// Run status.
    pub status: AgentRunStatus,
    /// Typed abort reason when `status` is [`AgentRunStatus::Aborted`].
    pub abort_error: Option<AgentProviderError>,
    /// Final output, if any.
    pub output: AgentOutput,
    /// Budget consumed before completion or abort.
    pub budget_consumed: AgentBudgetConsumed,
    /// Tool calls attempted by the agent.
    pub tool_calls: Vec<AgentToolAttempt>,
    /// Compact policy trace.
    pub policy_trace: Vec<String>,
}

/// Errors returned by `AgentProvider` implementations and policy helpers.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AgentProviderError {
    /// Request failed local validation.
    #[error("invalid agent spawn request: {message}")]
    InvalidRequest {
        /// Validation failure.
        message: String,
    },
    /// Tool was not present in the allowlist.
    #[error("tool not allowed: {verb:?}")]
    ToolNotAllowed {
        /// Rejected verb.
        verb: CairnVerb,
    },
    /// Mutating verb was allowlisted but not granted by scope.
    #[error("mutating verb not scoped: {verb:?}")]
    MutatingVerbNotScoped {
        /// Rejected verb.
        verb: CairnVerb,
    },
    /// Cost budget was exhausted.
    #[error("agent budget exceeded: {limit}")]
    BudgetExceeded {
        /// Budget dimension that was exceeded.
        limit: String,
    },
    /// Wall-clock budget was exhausted.
    #[error("agent wall-clock budget exceeded")]
    WallClockExceeded,
    /// Output failed requested schema validation.
    #[error("invalid agent output: {message}")]
    InvalidOutput {
        /// Validation failure.
        message: String,
    },
    /// Provider cannot run in this build or configuration.
    #[error("agent provider unavailable: {message}")]
    ProviderUnavailable {
        /// Availability failure.
        message: String,
    },
}

impl AgentProviderError {
    fn invalid_request(message: impl Into<String>) -> Self {
        Self::InvalidRequest {
            message: message.into(),
        }
    }
}

/// Agent provider contract: autonomous workers with bounded tools and budgets.
#[async_trait::async_trait]
pub trait AgentProvider: Send + Sync {
    /// Stable identifier of the registered plugin instance.
    fn name(&self) -> &str;

    /// Static capability advertisement (brief §4.1).
    fn capabilities(&self) -> &AgentProviderCapabilities;

    /// Range of `AgentProvider::CONTRACT_VERSION` values this impl accepts.
    fn supported_contract_versions(&self) -> VersionRange;

    /// Spawn a constrained agent-mode worker.
    ///
    /// # Errors
    ///
    /// Returns typed provider, policy, budget, and output errors. Implementations
    /// must fail closed before invoking a denied tool. Return `Err(_)` when the
    /// provider aborts before it can return trace or budget state; return an
    /// aborted [`AgentRun`] with `abort_error` when trace and budget state are
    /// available.
    async fn spawn(&self, request: AgentSpawnRequest) -> Result<AgentRun, AgentProviderError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_request() -> AgentSpawnRequest {
        AgentSpawnRequest {
            identity: AgentIdentity::new("agt:cairn-librarian:v2").expect("valid identity"),
            scope: AgentScope::read_only(),
            tool_allowlist: AgentToolAllowlist::read_only_cairn(),
            cost_budget: AgentCostBudget {
                max_turns: 3,
                max_tool_calls: 3,
                max_cost_units: 100,
            },
            wall_clock_budget: AgentWallClockBudget { max_millis: 1_000 },
            output_schema: AgentOutputSchema::Text,
            prompt: "Inspect the vault".to_string(),
        }
    }

    #[derive(Debug, Clone)]
    enum ScriptStep {
        Turn { cost_units: u64 },
        Tool(AgentToolCall),
        Output(AgentOutput),
    }

    struct ScriptedAgentProvider {
        steps: Vec<ScriptStep>,
    }

    impl ScriptedAgentProvider {
        fn new(steps: Vec<ScriptStep>) -> Self {
            Self { steps }
        }
    }

    #[async_trait::async_trait]
    impl AgentProvider for ScriptedAgentProvider {
        fn name(&self) -> &'static str {
            "scripted-agent"
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
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
        }

        async fn spawn(&self, request: AgentSpawnRequest) -> Result<AgentRun, AgentProviderError> {
            run_scripted_agent(&request, &self.steps)
        }
    }

    fn run_scripted_agent(
        request: &AgentSpawnRequest,
        steps: &[ScriptStep],
    ) -> Result<AgentRun, AgentProviderError> {
        request.validate()?;
        let mut meter = AgentRunMeter::new(request);
        let mut output = AgentOutput::Empty;
        let mut attempts = Vec::new();
        let mut policy_trace = Vec::new();

        for step in steps {
            match step {
                ScriptStep::Turn { cost_units } => {
                    meter.charge_turn(1)?;
                    meter.charge_cost_units(*cost_units)?;
                }
                ScriptStep::Tool(call) => {
                    let outcome = evaluate_tool_policy(request, call)?;
                    meter.charge_tool_call()?;
                    attempts.push(AgentToolAttempt {
                        call: call.clone(),
                        outcome,
                        reason: format!("{outcome:?}"),
                    });
                    policy_trace.push(format!("{:?}:{outcome:?}", call.verb));
                }
                ScriptStep::Output(next) => {
                    validate_output(&request.output_schema, next)?;
                    output = next.clone();
                }
            }
        }

        Ok(AgentRun {
            status: AgentRunStatus::Completed,
            abort_error: None,
            output,
            budget_consumed: meter.consumed(),
            tool_calls: attempts,
            policy_trace,
        })
    }

    #[tokio::test]
    async fn scripted_spawn_completes_read_only_flow() {
        let provider = ScriptedAgentProvider::new(vec![
            ScriptStep::Turn { cost_units: 10 },
            ScriptStep::Tool(AgentToolCall::new(CairnVerb::Search)),
            ScriptStep::Output(AgentOutput::Text("done".to_string())),
        ]);
        let run = provider
            .spawn(base_request())
            .await
            .expect("read-only flow completes");
        assert_eq!(run.status, AgentRunStatus::Completed);
        assert_eq!(run.abort_error, None);
        assert_eq!(run.budget_consumed.turns, 1);
        assert_eq!(run.budget_consumed.tool_calls, 1);
        assert_eq!(run.budget_consumed.cost_units, 10);
        assert_eq!(
            run.tool_calls[0].outcome,
            AgentToolPolicyOutcome::AllowedReadOnly
        );
    }

    #[tokio::test]
    async fn scripted_spawn_aborts_on_budget_exhaustion() {
        let provider = ScriptedAgentProvider::new(vec![
            ScriptStep::Turn { cost_units: 1 },
            ScriptStep::Turn { cost_units: 1 },
            ScriptStep::Turn { cost_units: 1 },
            ScriptStep::Turn { cost_units: 1 },
        ]);
        let err = provider
            .spawn(base_request())
            .await
            .expect_err("fourth turn exceeds budget");
        assert!(matches!(
            err,
            AgentProviderError::BudgetExceeded { ref limit } if limit == "turns"
        ));
    }

    #[test]
    fn read_only_allowlist_contains_only_read_verbs() {
        let allowlist = AgentToolAllowlist::read_only_cairn();
        assert!(allowlist.allows(&AgentToolCall::new(CairnVerb::Search)));
        assert!(allowlist.allows(&AgentToolCall::new(CairnVerb::Retrieve)));
        assert!(allowlist.allows(&AgentToolCall::lint_dry()));
        assert!(!allowlist.allows(&AgentToolCall::new(CairnVerb::Forget)));
        assert!(!allowlist.allows(&AgentToolCall {
            verb: CairnVerb::Lint,
            write_report: true,
            persist: false,
        }));
    }

    #[test]
    fn mutating_verbs_are_identified() {
        assert!(CairnVerb::Ingest.is_mutating());
        assert!(CairnVerb::CaptureTrace.is_mutating());
        assert!(CairnVerb::Forget.is_mutating());
        assert!(!CairnVerb::Search.is_mutating());
        assert!(!CairnVerb::Retrieve.is_mutating());
        assert!(!CairnVerb::Lint.is_mutating());
        assert!(!AgentToolCall::new(CairnVerb::Summarize).is_mutating());
        assert!(AgentToolCall::summarize_persist().is_mutating());
        assert!(
            AgentToolCall {
                verb: CairnVerb::Lint,
                write_report: true,
                persist: false,
            }
            .is_mutating()
        );
        assert!(!AgentToolCall::lint_dry().write_report);
    }

    #[test]
    fn request_validation_rejects_empty_prompt() {
        let mut request = base_request();
        request.prompt.clear();
        let err = request.validate().expect_err("empty prompt rejects");
        assert!(matches!(err, AgentProviderError::InvalidRequest { .. }));
    }

    #[test]
    fn request_validation_rejects_zero_budget() {
        let mut request = base_request();
        request.cost_budget.max_turns = 0;
        let err = request.validate().expect_err("zero turn budget rejects");
        assert!(matches!(err, AgentProviderError::InvalidRequest { .. }));
    }

    #[test]
    fn request_validation_rejects_empty_allowlist() {
        let mut request = base_request();
        request.tool_allowlist.tools.clear();
        let err = request.validate().expect_err("empty allowlist rejects");
        assert!(matches!(err, AgentProviderError::InvalidRequest { .. }));
    }

    #[test]
    fn request_validation_rejects_zero_tool_call_budget() {
        let mut request = base_request();
        request.cost_budget.max_tool_calls = 0;
        let err = request
            .validate()
            .expect_err("zero tool call budget rejects");
        assert!(matches!(err, AgentProviderError::InvalidRequest { .. }));
    }

    #[test]
    fn request_validation_rejects_zero_cost_unit_budget() {
        let mut request = base_request();
        request.cost_budget.max_cost_units = 0;
        let err = request.validate().expect_err("zero cost budget rejects");
        assert!(matches!(err, AgentProviderError::InvalidRequest { .. }));
    }

    #[test]
    fn request_validation_rejects_zero_wall_clock_budget() {
        let mut request = base_request();
        request.wall_clock_budget.max_millis = 0;
        let err = request
            .validate()
            .expect_err("zero wall-clock budget rejects");
        assert!(matches!(err, AgentProviderError::InvalidRequest { .. }));
    }

    #[test]
    fn request_validation_rejects_non_mutating_scope_entries() {
        let mut request = base_request();
        request.scope = AgentScope::with_mutations(vec![CairnVerb::Search]);
        let err = request
            .validate()
            .expect_err("non-mutating scope entries reject");
        assert!(matches!(err, AgentProviderError::InvalidRequest { .. }));
    }

    #[test]
    fn request_validation_rejects_persist_on_non_summarize_tool() {
        let mut request = base_request();
        request.tool_allowlist.tools.push(AgentToolCall {
            verb: CairnVerb::Search,
            write_report: false,
            persist: true,
        });
        let err = request
            .validate()
            .expect_err("persist flag rejects outside summarize");
        assert!(matches!(err, AgentProviderError::InvalidRequest { .. }));
    }

    #[test]
    fn request_validation_rejects_write_report_on_non_lint_tool() {
        let mut request = base_request();
        request.tool_allowlist.tools.push(AgentToolCall {
            verb: CairnVerb::Search,
            write_report: true,
            persist: false,
        });
        let err = request
            .validate()
            .expect_err("write_report flag rejects outside lint");
        assert!(matches!(err, AgentProviderError::InvalidRequest { .. }));
    }

    #[test]
    fn policy_rejects_unallowlisted_tool() {
        let request = base_request();
        let err = evaluate_tool_policy(&request, &AgentToolCall::new(CairnVerb::Forget))
            .expect_err("forget is not allowlisted");
        assert!(matches!(
            err,
            AgentProviderError::ToolNotAllowed {
                verb: CairnVerb::Forget
            }
        ));
    }

    #[test]
    fn policy_rejects_allowlisted_mutation_without_scope() {
        let mut request = base_request();
        request
            .tool_allowlist
            .tools
            .push(AgentToolCall::new(CairnVerb::Ingest));
        let err = evaluate_tool_policy(&request, &AgentToolCall::new(CairnVerb::Ingest))
            .expect_err("ingest needs write scope");
        assert!(matches!(
            err,
            AgentProviderError::MutatingVerbNotScoped {
                verb: CairnVerb::Ingest
            }
        ));
    }

    #[test]
    fn policy_marks_scoped_mutation_as_wal_routed() {
        let mut request = base_request();
        request.scope = AgentScope::with_mutations(vec![CairnVerb::Ingest]);
        request
            .tool_allowlist
            .tools
            .push(AgentToolCall::new(CairnVerb::Ingest));
        let outcome = evaluate_tool_policy(&request, &AgentToolCall::new(CairnVerb::Ingest))
            .expect("scoped ingest is admitted");
        assert_eq!(outcome, AgentToolPolicyOutcome::AllowedWalRoutedMutation);
    }

    #[test]
    fn metered_context_rejects_turn_budget_overrun() {
        let request = base_request();
        let mut meter = AgentRunMeter::new(&request);
        meter.charge_turn(1).expect("first turn admitted");
        meter.charge_turn(1).expect("second turn admitted");
        meter.charge_turn(1).expect("third turn admitted");
        let err = meter
            .charge_turn(1)
            .expect_err("fourth turn exceeds budget");
        assert!(matches!(
            err,
            AgentProviderError::BudgetExceeded { ref limit } if limit == "turns"
        ));
    }

    #[test]
    fn output_validation_rejects_json_when_text_requested() {
        let err = validate_output(
            &AgentOutputSchema::Text,
            &AgentOutput::Json(serde_json::json!({})),
        )
        .expect_err("json is not text");
        assert!(matches!(err, AgentProviderError::InvalidOutput { .. }));
    }

    #[test]
    fn identity_rejects_empty_or_non_agent_values() {
        assert!(AgentIdentity::new("").is_err());
        assert!(AgentIdentity::new("agt:").is_err());
        assert!(AgentIdentity::new("agt:cairn librarian:v2").is_err());
        assert!(AgentIdentity::new("hmn:tafeng:v1").is_err());
        assert!(AgentIdentity::new("agt:cairn-librarian:v2").is_ok());
    }

    #[test]
    fn identity_deserialization_rejects_non_agent_values() {
        assert!(serde_json::from_str::<AgentIdentity>(r#""hmn:tafeng:v1""#).is_err());
    }

    #[test]
    fn aborted_run_can_carry_typed_error() {
        let run = AgentRun {
            status: AgentRunStatus::Aborted,
            abort_error: Some(AgentProviderError::BudgetExceeded {
                limit: "turns".to_string(),
            }),
            output: AgentOutput::Empty,
            budget_consumed: AgentBudgetConsumed {
                turns: 1,
                tool_calls: 0,
                cost_units: 0,
            },
            tool_calls: Vec::new(),
            policy_trace: vec!["turn budget exceeded".to_string()],
        };

        assert_eq!(run.status, AgentRunStatus::Aborted);
        assert_eq!(
            run.abort_error,
            Some(AgentProviderError::BudgetExceeded {
                limit: "turns".to_string(),
            })
        );
    }

    #[test]
    fn aborted_run_serializes_abort_error() {
        let run = AgentRun {
            status: AgentRunStatus::Aborted,
            abort_error: Some(AgentProviderError::BudgetExceeded {
                limit: "turns".to_string(),
            }),
            output: AgentOutput::Empty,
            budget_consumed: AgentBudgetConsumed {
                turns: 1,
                tool_calls: 0,
                cost_units: 0,
            },
            tool_calls: Vec::new(),
            policy_trace: vec!["turn budget exceeded".to_string()],
        };

        let value = serde_json::to_value(&run).expect("agent run serializes");

        assert_eq!(
            value,
            serde_json::json!({
                "status": "aborted",
                "abort_error": {
                    "kind": "budget_exceeded",
                    "limit": "turns",
                },
                "output": "empty",
                "budget_consumed": {
                    "turns": 1,
                    "tool_calls": 0,
                    "cost_units": 0,
                },
                "tool_calls": [],
                "policy_trace": ["turn budget exceeded"],
            })
        );
        assert_eq!(value["status"], "aborted");
        assert_eq!(value["abort_error"]["kind"], "budget_exceeded");
        assert_eq!(value["abort_error"]["limit"], "turns");

        let roundtrip: AgentRun =
            serde_json::from_value(value).expect("serialized agent run deserializes");
        assert_eq!(roundtrip, run);
    }
}
