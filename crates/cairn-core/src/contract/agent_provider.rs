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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
        if !raw.starts_with("agt:") {
            return Err(AgentProviderError::invalid_request(
                "agent identity must start with `agt:`",
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
            Self::Ingest | Self::Summarize | Self::CaptureTrace | Self::Forget
        )
    }
}

/// A proposed `cairn` CLI tool call, before execution.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentToolCall {
    /// Verb being requested.
    pub verb: CairnVerb,
    /// Whether `lint` is constrained to dry-run behavior.
    pub dry_run: bool,
    /// Whether `summarize` requests persistence.
    pub persist: bool,
}

impl AgentToolCall {
    /// Build a tool call with no special flags.
    #[must_use]
    pub const fn new(verb: CairnVerb) -> Self {
        Self {
            verb,
            dry_run: false,
            persist: false,
        }
    }

    /// Build `cairn lint --dry`.
    #[must_use]
    pub const fn lint_dry() -> Self {
        Self {
            verb: CairnVerb::Lint,
            dry_run: true,
            persist: false,
        }
    }

    /// Build `cairn summarize` in write/persist mode.
    #[must_use]
    pub const fn summarize_persist() -> Self {
        Self {
            verb: CairnVerb::Summarize,
            dry_run: false,
            persist: true,
        }
    }

    /// True when this call can mutate the vault.
    #[must_use]
    pub const fn is_mutating(&self) -> bool {
        self.verb.is_mutating() || matches!(self.verb, CairnVerb::Summarize) && self.persist
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
    /// allowlists, zero budgets, or non-mutating verbs listed as mutation grants.
    pub fn validate(&self) -> Result<(), AgentProviderError> {
        if self.prompt.trim().is_empty() {
            return Err(AgentProviderError::invalid_request("prompt is empty"));
        }
        if self.tool_allowlist.tools.is_empty() {
            return Err(AgentProviderError::invalid_request(
                "tool allowlist is empty",
            ));
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
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRun {
    /// Run status.
    pub status: AgentRunStatus,
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
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
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
        limit: &'static str,
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
    /// must fail closed before invoking a denied tool.
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

    #[test]
    fn read_only_allowlist_contains_only_read_verbs() {
        let allowlist = AgentToolAllowlist::read_only_cairn();
        assert!(allowlist.allows(&AgentToolCall::new(CairnVerb::Search)));
        assert!(allowlist.allows(&AgentToolCall::new(CairnVerb::Retrieve)));
        assert!(allowlist.allows(&AgentToolCall::lint_dry()));
        assert!(!allowlist.allows(&AgentToolCall::new(CairnVerb::Forget)));
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
    fn identity_rejects_empty_or_non_agent_values() {
        assert!(AgentIdentity::new("").is_err());
        assert!(AgentIdentity::new("hmn:tafeng:v1").is_err());
        assert!(AgentIdentity::new("agt:cairn-librarian:v2").is_ok());
    }
}
