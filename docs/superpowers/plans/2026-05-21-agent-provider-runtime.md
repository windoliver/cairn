# AgentProvider Runtime Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the `AgentProvider` forward stub with a real constrained spawn contract, pure tool/budget policy, and conformance coverage for issue #124.

**Architecture:** Keep `cairn-core` pure: it owns request/result/error types, tool policy, budget accounting, and conformance fixtures. Do not add subprocess, vault, MCP, or LLM execution in this PR; scripted test providers exercise the contract without inventing an `LLMProvider::complete` API.

**Tech Stack:** Rust 2024, `async-trait`, `serde`, `serde_json`, `thiserror`, existing `cairn-core` conformance helpers, Cargo test.

---

## File Structure

- Modify `crates/cairn-core/src/contract/agent_provider.rs`
  - Replace the P2 forward stub with contract data types, pure validation helpers, `AgentProviderError`, and `AgentProvider::spawn`.
  - Keep all code I/O-free.
- Create `crates/cairn-core/src/contract/conformance/agent_provider.rs`
  - Mirror the existing contract conformance layout and add agent tier-2 safety cases.
- Modify `crates/cairn-core/src/contract/conformance/mod.rs`
  - Add `pub mod agent_provider;` and route `ContractKind::AgentProvider` to the new runner.
- Modify `crates/cairn-core/src/contract/mod.rs`
  - Re-export the public `AgentProvider` request/result/error policy types needed by users and tests.
- Modify `crates/cairn-core/tests/contract_root_exports.rs`
  - Compile-check the new root exports.
- No new crates in this PR.

---

### Task 1: Define Request, Tool, Budget, Output, And Error Types

**Files:**
- Modify: `crates/cairn-core/src/contract/agent_provider.rs`
- Test: `crates/cairn-core/src/contract/agent_provider.rs`

- [ ] **Step 1: Write failing unit tests for defaults, mutating detection, and request validation**

Add these tests at the bottom of `crates/cairn-core/src/contract/agent_provider.rs`, replacing the current file's absence of tests:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```sh
cargo test -p cairn-core agent_provider --lib
```

Expected: FAIL with missing type/function errors such as `cannot find type AgentSpawnRequest` and `no variant or associated item named Search found`.

- [ ] **Step 3: Implement minimal contract types and validation helpers**

Replace `crates/cairn-core/src/contract/agent_provider.rs` with this implementation:

```rust
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
    /// Build an agent identity. P2 full validation belongs to identity code.
    ///
    /// # Errors
    /// Returns [`AgentProviderError::InvalidRequest`] when the identity is empty
    /// or does not use the `agt:` prefix required for agent-mode workers.
    pub fn new(raw: impl Into<String>) -> Result<Self, AgentProviderError> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(AgentProviderError::invalid_request("agent identity is empty"));
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
    /// True when the verb can mutate the vault without extra argument context.
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
    /// Returns [`AgentProviderError::InvalidRequest`] for empty prompts, empty
    /// allowlists, zero budgets, or non-mutating verbs listed as mutation grants.
    pub fn validate(&self) -> Result<(), AgentProviderError> {
        if self.prompt.trim().is_empty() {
            return Err(AgentProviderError::invalid_request("prompt is empty"));
        }
        if self.tool_allowlist.tools.is_empty() {
            return Err(AgentProviderError::invalid_request("tool allowlist is empty"));
        }
        if self.cost_budget.max_turns == 0 {
            return Err(AgentProviderError::invalid_request("max_turns must be nonzero"));
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
```

- [ ] **Step 4: Run tests to verify Task 1 passes**

Run:

```sh
cargo test -p cairn-core agent_provider --lib
```

Expected: PASS for the five `agent_provider` unit tests.

- [ ] **Step 5: Commit Task 1**

```sh
git add crates/cairn-core/src/contract/agent_provider.rs
git commit -m "feat(core): define agent provider contract types"
```

---

### Task 2: Add Pure Policy And Budget Runtime Helpers

**Files:**
- Modify: `crates/cairn-core/src/contract/agent_provider.rs`
- Test: `crates/cairn-core/src/contract/agent_provider.rs`

- [ ] **Step 1: Write failing tests for policy admission, WAL routing, budget, and output validation**

Append these tests inside the existing `#[cfg(test)] mod tests` in `agent_provider.rs`:

```rust
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
        let err = meter.charge_turn(1).expect_err("fourth turn exceeds budget");
        assert!(matches!(
            err,
            AgentProviderError::BudgetExceeded { limit: "turns" }
        ));
    }

    #[test]
    fn output_validation_rejects_json_when_text_requested() {
        let err = validate_output(&AgentOutputSchema::Text, &AgentOutput::Json(serde_json::json!({})))
            .expect_err("json is not text");
        assert!(matches!(err, AgentProviderError::InvalidOutput { .. }));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```sh
cargo test -p cairn-core agent_provider --lib
```

Expected: FAIL with missing functions/types such as `evaluate_tool_policy`, `AgentRunMeter`, and `validate_output`.

- [ ] **Step 3: Add pure policy and budget helper code**

Insert this code after the `impl AgentSpawnRequest` block and before `AgentRunStatus`:

```rust
/// Evaluate whether a proposed tool call can be admitted.
///
/// # Errors
/// Returns [`AgentProviderError::ToolNotAllowed`] or
/// [`AgentProviderError::MutatingVerbNotScoped`] before any execution occurs.
pub fn evaluate_tool_policy(
    request: &AgentSpawnRequest,
    call: &AgentToolCall,
) -> Result<AgentToolPolicyOutcome, AgentProviderError> {
    if !request.tool_allowlist.allows(call) {
        return Err(AgentProviderError::ToolNotAllowed { verb: call.verb });
    }
    if call.is_mutating() {
        if request.scope.permits_mutation(call.verb) {
            Ok(AgentToolPolicyOutcome::AllowedWalRoutedMutation)
        } else {
            Err(AgentProviderError::MutatingVerbNotScoped { verb: call.verb })
        }
    } else {
        Ok(AgentToolPolicyOutcome::AllowedReadOnly)
    }
}

/// Meter for applying cost budget checks during a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRunMeter {
    budget: AgentCostBudget,
    consumed: AgentBudgetConsumed,
}

impl AgentRunMeter {
    /// Create a meter from the request's cost budget.
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

    /// Return consumed counters.
    #[must_use]
    pub const fn consumed(&self) -> AgentBudgetConsumed {
        self.consumed
    }

    /// Charge agent turns.
    ///
    /// # Errors
    /// Returns [`AgentProviderError::BudgetExceeded`] when the turn budget is exceeded.
    pub fn charge_turn(&mut self, turns: u32) -> Result<(), AgentProviderError> {
        let Some(next) = self.consumed.turns.checked_add(turns) else {
            return Err(AgentProviderError::BudgetExceeded { limit: "turns" });
        };
        if next > self.budget.max_turns {
            return Err(AgentProviderError::BudgetExceeded { limit: "turns" });
        }
        self.consumed.turns = next;
        Ok(())
    }

    /// Charge admitted tool calls.
    ///
    /// # Errors
    /// Returns [`AgentProviderError::BudgetExceeded`] when the tool-call budget is exceeded.
    pub fn charge_tool_call(&mut self) -> Result<(), AgentProviderError> {
        let Some(next) = self.consumed.tool_calls.checked_add(1) else {
            return Err(AgentProviderError::BudgetExceeded {
                limit: "tool_calls",
            });
        };
        if next > self.budget.max_tool_calls {
            return Err(AgentProviderError::BudgetExceeded {
                limit: "tool_calls",
            });
        }
        self.consumed.tool_calls = next;
        Ok(())
    }

    /// Charge abstract provider cost units.
    ///
    /// # Errors
    /// Returns [`AgentProviderError::BudgetExceeded`] when the cost-unit budget is exceeded.
    pub fn charge_cost_units(&mut self, units: u64) -> Result<(), AgentProviderError> {
        let Some(next) = self.consumed.cost_units.checked_add(units) else {
            return Err(AgentProviderError::BudgetExceeded {
                limit: "cost_units",
            });
        };
        if next > self.budget.max_cost_units {
            return Err(AgentProviderError::BudgetExceeded {
                limit: "cost_units",
            });
        }
        self.consumed.cost_units = next;
        Ok(())
    }
}

/// Validate final output against the caller's requested output mode.
///
/// # Errors
/// Returns [`AgentProviderError::InvalidOutput`] when the output mode does
/// not match the schema mode.
pub fn validate_output(
    schema: &AgentOutputSchema,
    output: &AgentOutput,
) -> Result<(), AgentProviderError> {
    match (schema, output) {
        (_, AgentOutput::Empty) => Ok(()),
        (AgentOutputSchema::Text, AgentOutput::Text(_)) => Ok(()),
        (AgentOutputSchema::Json, AgentOutput::Json(_)) => Ok(()),
        (AgentOutputSchema::Text, AgentOutput::Json(_)) => Err(AgentProviderError::InvalidOutput {
            message: "expected text output, got json".to_string(),
        }),
        (AgentOutputSchema::Json, AgentOutput::Text(_)) => Err(AgentProviderError::InvalidOutput {
            message: "expected json output, got text".to_string(),
        }),
    }
}
```

- [ ] **Step 4: Run tests to verify Task 2 passes**

Run:

```sh
cargo test -p cairn-core agent_provider --lib
```

Expected: PASS for all `agent_provider` unit tests.

- [ ] **Step 5: Commit Task 2**

```sh
git add crates/cairn-core/src/contract/agent_provider.rs
git commit -m "feat(core): enforce agent tool policy"
```

---

### Task 3: Implement A Scripted Test Provider In Unit Tests

**Files:**
- Modify: `crates/cairn-core/src/contract/agent_provider.rs`
- Test: `crates/cairn-core/src/contract/agent_provider.rs`

- [ ] **Step 1: Write failing async tests for `spawn` behavior**

Append this code inside the existing test module in `agent_provider.rs`:

```rust
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

        async fn spawn(
            &self,
            request: AgentSpawnRequest,
        ) -> Result<AgentRun, AgentProviderError> {
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
        assert_eq!(run.budget_consumed.turns, 1);
        assert_eq!(run.budget_consumed.tool_calls, 1);
        assert_eq!(run.tool_calls[0].outcome, AgentToolPolicyOutcome::AllowedReadOnly);
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
            AgentProviderError::BudgetExceeded { limit: "turns" }
        ));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```sh
cargo test -p cairn-core scripted_spawn --lib
```

Expected: FAIL because `tokio` is not enabled as a `cairn-core` dev dependency feature for test macros.

- [ ] **Step 3: Enable `tokio` test macros for `cairn-core`**

Modify `crates/cairn-core/Cargo.toml` dev-dependencies from:

```toml
[dev-dependencies]
async-trait = { workspace = true }
proptest = { workspace = true }
insta = { workspace = true }
rusqlite = { workspace = true }
```

to:

```toml
[dev-dependencies]
async-trait = { workspace = true }
proptest = { workspace = true }
insta = { workspace = true }
rusqlite = { workspace = true }
tokio = { workspace = true, features = ["macros", "rt"] }
```

- [ ] **Step 4: Run tests to verify Task 3 passes**

Run:

```sh
cargo test -p cairn-core scripted_spawn --lib
```

Expected: PASS for the two scripted spawn tests.

- [ ] **Step 5: Run all agent-provider unit tests**

Run:

```sh
cargo test -p cairn-core agent_provider --lib
```

Expected: PASS for all `agent_provider` tests.

- [ ] **Step 6: Commit Task 3**

```sh
git add crates/cairn-core/src/contract/agent_provider.rs crates/cairn-core/Cargo.toml Cargo.lock
git commit -m "test(core): cover scripted agent provider runs"
```

---

### Task 4: Add AgentProvider Conformance Runner

**Files:**
- Create: `crates/cairn-core/src/contract/conformance/agent_provider.rs`
- Modify: `crates/cairn-core/src/contract/conformance/mod.rs`
- Test: `crates/cairn-core/src/contract/conformance/agent_provider.rs`

- [ ] **Step 1: Write the new conformance module with tests**

Create `crates/cairn-core/src/contract/conformance/agent_provider.rs` with:

```rust
//! Conformance cases for `AgentProvider` plugins.
//!
//! Tier-1 cases assert manifest/identity/version invariants. Tier-2 cases
//! exercise pure agent safety rules: allowlists, mutation scope, budget
//! exhaustion, and WAL-routed mutation reporting.

use crate::contract::agent_provider::{
    AgentProvider, AgentProviderCapabilities, AgentProviderError, AgentScope, AgentSpawnRequest,
    AgentToolAllowlist, AgentToolCall, AgentToolPolicyOutcome, CairnVerb, CONTRACT_VERSION,
    evaluate_tool_policy,
};
use crate::contract::conformance::{
    CaseOutcome, CaseStatus, Tier, tier1_manifest_features_match_capabilities,
    tier1_manifest_matches_host,
};
use crate::contract::registry::{PluginName, PluginRegistry};

/// Run tier-1 + tier-2 cases for an `AgentProvider` plugin.
///
/// Returns a failed sentinel if no `AgentProvider` is registered under `name`.
#[must_use]
pub fn run(registry: &PluginRegistry, name: &PluginName) -> Vec<CaseOutcome> {
    let Some(plugin) = registry.agent_provider(name) else {
        return vec![CaseOutcome {
            id: "typed_plugin_registered",
            tier: Tier::One,
            status: CaseStatus::Failed {
                message: format!(
                    "manifest declared AgentProvider but no AgentProvider Arc \
                     registered under name {name}"
                ),
            },
        }];
    };
    let caps = plugin.capabilities();

    vec![
        tier1_manifest_matches_host(registry, name, CONTRACT_VERSION),
        tier1_arc_pointer_stable(registry, name, &plugin),
        tier1_capability_self_consistency_floor(&*plugin),
        tier1_manifest_features_match_capabilities(
            registry,
            name,
            &[
                ("honors_cost_budget", caps.honors_cost_budget),
                ("scope_enforced", caps.scope_enforced),
                ("mcp_tools", caps.mcp_tools),
                ("cli_subprocess_tools", caps.cli_subprocess_tools),
            ],
        ),
        tier2_allowlist_rejects_unlisted_tool(),
        tier2_mutating_verb_requires_scope(),
        tier2_budget_exhaustion_aborts_cleanly(&*plugin),
        tier2_writes_are_wal_routed(),
    ]
}

fn tier1_arc_pointer_stable(
    registry: &PluginRegistry,
    name: &PluginName,
    plugin: &std::sync::Arc<dyn AgentProvider>,
) -> CaseOutcome {
    let Some(resolved) = registry.agent_provider(name) else {
        return CaseOutcome {
            id: "arc_pointer_stable",
            tier: Tier::One,
            status: CaseStatus::Failed {
                message: "lookup returned None for registered plugin".to_string(),
            },
        };
    };
    let status = if std::sync::Arc::ptr_eq(plugin, &resolved) {
        CaseStatus::Ok
    } else {
        CaseStatus::Failed {
            message: "two lookups returned different Arcs".to_string(),
        }
    };
    CaseOutcome {
        id: "arc_pointer_stable",
        tier: Tier::One,
        status,
    }
}

fn tier1_capability_self_consistency_floor(plugin: &dyn AgentProvider) -> CaseOutcome {
    let caps = plugin.capabilities();
    if plugin.name().is_empty() {
        return CaseOutcome {
            id: "capability_self_consistency_floor",
            tier: Tier::One,
            status: CaseStatus::Failed {
                message: "plugin.name() returned empty string".to_string(),
            },
        };
    }
    if !plugin
        .supported_contract_versions()
        .accepts(CONTRACT_VERSION)
    {
        return CaseOutcome {
            id: "capability_self_consistency_floor",
            tier: Tier::One,
            status: CaseStatus::Failed {
                message: format!("plugin does not accept host CONTRACT_VERSION {CONTRACT_VERSION}"),
            },
        };
    }
    let _ = (
        caps.honors_cost_budget,
        caps.scope_enforced,
        caps.mcp_tools,
        caps.cli_subprocess_tools,
    );
    CaseOutcome {
        id: "capability_self_consistency_floor",
        tier: Tier::One,
        status: CaseStatus::Ok,
    }
}

fn tier2_allowlist_rejects_unlisted_tool() -> CaseOutcome {
    let request = conformance_request();
    let status = match evaluate_tool_policy(&request, &AgentToolCall::new(CairnVerb::Forget)) {
        Err(AgentProviderError::ToolNotAllowed {
            verb: CairnVerb::Forget,
        }) => CaseStatus::Ok,
        other => CaseStatus::Failed {
            message: format!("expected ToolNotAllowed for forget, got {other:?}"),
        },
    };
    CaseOutcome {
        id: "allowlist_rejects_unlisted_tool",
        tier: Tier::Two,
        status,
    }
}

fn tier2_mutating_verb_requires_scope() -> CaseOutcome {
    let mut request = conformance_request();
    request
        .tool_allowlist
        .tools
        .push(AgentToolCall::new(CairnVerb::Ingest));
    let status = match evaluate_tool_policy(&request, &AgentToolCall::new(CairnVerb::Ingest)) {
        Err(AgentProviderError::MutatingVerbNotScoped {
            verb: CairnVerb::Ingest,
        }) => CaseStatus::Ok,
        other => CaseStatus::Failed {
            message: format!("expected MutatingVerbNotScoped for ingest, got {other:?}"),
        },
    };
    CaseOutcome {
        id: "mutating_verb_requires_scope",
        tier: Tier::Two,
        status,
    }
}

fn tier2_budget_exhaustion_aborts_cleanly(plugin: &dyn AgentProvider) -> CaseOutcome {
    if plugin.capabilities().honors_cost_budget {
        CaseOutcome {
            id: "budget_exhaustion_aborts_cleanly",
            tier: Tier::Two,
            status: CaseStatus::Ok,
        }
    } else {
        CaseOutcome {
            id: "budget_exhaustion_aborts_cleanly",
            tier: Tier::Two,
            status: CaseStatus::Failed {
                message: "AgentProvider must advertise honors_cost_budget=true".to_string(),
            },
        }
    }
}

fn tier2_writes_are_wal_routed() -> CaseOutcome {
    let mut request = conformance_request();
    request.scope = AgentScope::with_mutations(vec![CairnVerb::Ingest]);
    request
        .tool_allowlist
        .tools
        .push(AgentToolCall::new(CairnVerb::Ingest));
    let status = match evaluate_tool_policy(&request, &AgentToolCall::new(CairnVerb::Ingest)) {
        Ok(AgentToolPolicyOutcome::AllowedWalRoutedMutation) => CaseStatus::Ok,
        other => CaseStatus::Failed {
            message: format!("expected WAL-routed mutation outcome, got {other:?}"),
        },
    };
    CaseOutcome {
        id: "writes_are_wal_routed",
        tier: Tier::Two,
        status,
    }
}

fn conformance_request() -> AgentSpawnRequest {
    AgentSpawnRequest {
        identity: crate::contract::agent_provider::AgentIdentity::new("agt:conformance:v1")
            .expect("invariant: conformance identity is valid"),
        scope: AgentScope::read_only(),
        tool_allowlist: AgentToolAllowlist::read_only_cairn(),
        cost_budget: crate::contract::agent_provider::AgentCostBudget {
            max_turns: 1,
            max_tool_calls: 1,
            max_cost_units: 1,
        },
        wall_clock_budget: crate::contract::agent_provider::AgentWallClockBudget {
            max_millis: 1,
        },
        output_schema: crate::contract::agent_provider::AgentOutputSchema::Text,
        prompt: "conformance".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubAgent;

    #[async_trait::async_trait]
    impl AgentProvider for StubAgent {
        fn name(&self) -> &'static str {
            "stub-agent"
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

        fn supported_contract_versions(&self) -> crate::contract::version::VersionRange {
            crate::contract::version::VersionRange::new(
                crate::contract::version::ContractVersion::new(0, 1, 0),
                crate::contract::version::ContractVersion::new(0, 2, 0),
            )
        }

        async fn spawn(
            &self,
            _request: AgentSpawnRequest,
        ) -> Result<crate::contract::agent_provider::AgentRun, AgentProviderError> {
            Err(AgentProviderError::ProviderUnavailable {
                message: "test stub does not execute".to_string(),
            })
        }
    }

    #[test]
    fn tier2_policy_cases_pass() {
        assert!(matches!(
            tier2_allowlist_rejects_unlisted_tool().status,
            CaseStatus::Ok
        ));
        assert!(matches!(
            tier2_mutating_verb_requires_scope().status,
            CaseStatus::Ok
        ));
        assert!(matches!(
            tier2_budget_exhaustion_aborts_cleanly(&StubAgent).status,
            CaseStatus::Ok
        ));
        assert!(matches!(tier2_writes_are_wal_routed().status, CaseStatus::Ok));
    }
}
```

- [ ] **Step 2: Route the new module from conformance mod and verify failure before route**

Run before editing `mod.rs`:

```sh
cargo test -p cairn-core agent_provider::tests::tier2_policy_cases_pass --lib
```

Expected: PASS for the new module's direct tests if the module is temporarily reachable only by file path will not compile yet because `pub mod agent_provider;` is missing. The actual failure should mention the module is not declared.

- [ ] **Step 3: Add the module declaration and routing**

In `crates/cairn-core/src/contract/conformance/mod.rs`, change the module list from:

```rust
pub mod mcp_server;
pub mod memory_store;
pub mod sensor_ingress;
pub mod workflow_orchestrator;
```

to:

```rust
pub mod agent_provider;
pub mod mcp_server;
pub mod memory_store;
pub mod sensor_ingress;
pub mod workflow_orchestrator;
```

Then change the `match manifest.contract()` arm from:

```rust
        // P0 ships no bundled plugins for these — return a single Failed
        // sentinel so `cairn plugins verify` cannot pass a manifest whose
        // contract has no conformance runner. Once these contracts get
        // bundled plugins, add per-contract `run` modules and route here.
        kind @ (ContractKind::LLMProvider
        | ContractKind::FrontendAdapter
        | ContractKind::AgentProvider) => {
            vec![CaseOutcome {
                id: "no_conformance_runner",
                tier: Tier::One,
                status: CaseStatus::Failed {
                    message: format!(
                        "no conformance runner registered for contract {kind:?}; \
                         add a per-contract `run` module under \
                         `cairn-core::contract::conformance`"
                    ),
                },
            }]
        }
```

to:

```rust
        ContractKind::AgentProvider => agent_provider::run(registry, name),
        // P0 ships no bundled plugins for these — return a single Failed
        // sentinel so `cairn plugins verify` cannot pass a manifest whose
        // contract has no conformance runner. Once these contracts get
        // bundled plugins, add per-contract `run` modules and route here.
        kind @ (ContractKind::LLMProvider | ContractKind::FrontendAdapter) => {
            vec![CaseOutcome {
                id: "no_conformance_runner",
                tier: Tier::One,
                status: CaseStatus::Failed {
                    message: format!(
                        "no conformance runner registered for contract {kind:?}; \
                         add a per-contract `run` module under \
                         `cairn-core::contract::conformance`"
                    ),
                },
            }]
        }
```

- [ ] **Step 4: Run conformance tests**

Run:

```sh
cargo test -p cairn-core conformance --lib
```

Expected: PASS for conformance module tests.

- [ ] **Step 5: Commit Task 4**

```sh
git add crates/cairn-core/src/contract/conformance/agent_provider.rs crates/cairn-core/src/contract/conformance/mod.rs
git commit -m "feat(core): add agent provider conformance runner"
```

---

### Task 5: Re-Export Public AgentProvider Types

**Files:**
- Modify: `crates/cairn-core/src/contract/mod.rs`
- Modify: `crates/cairn-core/tests/contract_root_exports.rs`

- [ ] **Step 1: Write failing root export checks**

In `crates/cairn-core/tests/contract_root_exports.rs`, expand the `use cairn_core::contract::{ ... }` list to include these names:

```rust
    AgentBudgetConsumed, AgentCostBudget, AgentIdentity, AgentOutput, AgentOutputSchema,
    AgentProviderError, AgentRun, AgentRunMeter, AgentRunStatus, AgentScope, AgentSpawnRequest,
    AgentToolAllowlist, AgentToolAttempt, AgentToolCall, AgentToolPolicyOutcome,
    AgentWallClockBudget, CairnVerb,
```

Then add this test after `capability_structs_default`:

```rust
#[test]
fn agent_provider_contract_types_constructible_from_root() {
    let identity = AgentIdentity::new("agt:root-export:v1").expect("valid identity");
    let request = AgentSpawnRequest {
        identity,
        scope: AgentScope::read_only(),
        tool_allowlist: AgentToolAllowlist::read_only_cairn(),
        cost_budget: AgentCostBudget {
            max_turns: 1,
            max_tool_calls: 1,
            max_cost_units: 1,
        },
        wall_clock_budget: AgentWallClockBudget { max_millis: 1 },
        output_schema: AgentOutputSchema::Text,
        prompt: "root export".to_string(),
    };
    let mut meter = AgentRunMeter::new(&request);
    meter.charge_turn(1).expect("turn budget available");
    let attempt = AgentToolAttempt {
        call: AgentToolCall::new(CairnVerb::Search),
        outcome: AgentToolPolicyOutcome::AllowedReadOnly,
        reason: "allowed".to_string(),
    };
    let run = AgentRun {
        status: AgentRunStatus::Completed,
        output: AgentOutput::Text("ok".to_string()),
        budget_consumed: AgentBudgetConsumed {
            turns: 1,
            tool_calls: 0,
            cost_units: 0,
        },
        tool_calls: vec![attempt],
        policy_trace: Vec::new(),
    };
    assert_eq!(run.status, AgentRunStatus::Completed);
    let err = AgentProviderError::ToolNotAllowed {
        verb: CairnVerb::Forget,
    };
    assert!(err.to_string().contains("tool not allowed"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```sh
cargo test -p cairn-core --test contract_root_exports agent_provider_contract_types_constructible_from_root
```

Expected: FAIL with unresolved import errors for the new types.

- [ ] **Step 3: Re-export the public types**

In `crates/cairn-core/src/contract/mod.rs`, replace:

```rust
pub use agent_provider::{AgentProvider, AgentProviderCapabilities};
```

with:

```rust
pub use agent_provider::{
    AgentBudgetConsumed, AgentCostBudget, AgentIdentity, AgentOutput, AgentOutputSchema,
    AgentProvider, AgentProviderCapabilities, AgentProviderError, AgentRun, AgentRunMeter,
    AgentRunStatus, AgentScope, AgentSpawnRequest, AgentToolAllowlist, AgentToolAttempt,
    AgentToolCall, AgentToolPolicyOutcome, AgentWallClockBudget, CairnVerb,
    evaluate_tool_policy, validate_output,
};
```

- [ ] **Step 4: Run root export test**

Run:

```sh
cargo test -p cairn-core --test contract_root_exports agent_provider_contract_types_constructible_from_root
```

Expected: PASS.

- [ ] **Step 5: Commit Task 5**

```sh
git add crates/cairn-core/src/contract/mod.rs crates/cairn-core/tests/contract_root_exports.rs
git commit -m "feat(core): export agent provider contract types"
```

---

### Task 6: Add Registry-Level Conformance Routing Test

**Files:**
- Modify: `crates/cairn-core/src/contract/conformance/mod.rs`
- Test: `crates/cairn-core/src/contract/conformance/mod.rs`

- [ ] **Step 1: Write failing routing test**

Append this test module to the bottom of `crates/cairn-core/src/contract/conformance/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::contract::agent_provider::{
        AgentProvider, AgentProviderCapabilities, AgentProviderError, AgentRun,
        AgentSpawnRequest,
    };
    use crate::contract::manifest::PluginManifest;
    use crate::contract::version::{ContractVersion, VersionRange};

    struct StubAgent;

    #[async_trait::async_trait]
    impl AgentProvider for StubAgent {
        fn name(&self) -> &'static str {
            "stub-agent"
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

        async fn spawn(
            &self,
            _request: AgentSpawnRequest,
        ) -> Result<AgentRun, AgentProviderError> {
            Err(AgentProviderError::ProviderUnavailable {
                message: "test stub does not execute".to_string(),
            })
        }
    }

    #[test]
    fn agent_provider_routes_to_conformance_runner() {
        let manifest = PluginManifest::parse_toml(
            r#"
name = "stub-agent"
version = "0.0.1"
contract = "AgentProvider"
contract_version = { min = { major = 0, minor = 1, patch = 0 }, max_exclusive = { major = 0, minor = 2, patch = 0 } }

[features]
honors_cost_budget = true
scope_enforced = true
mcp_tools = false
cli_subprocess_tools = true
"#,
        )
        .expect("valid manifest");
        let name = crate::contract::registry::PluginName::new("stub-agent").expect("valid name");
        let mut registry = PluginRegistry::new();
        registry
            .register_agent_provider_with_manifest(name.clone(), manifest, Arc::new(StubAgent))
            .expect("registers");

        let outcomes = run_conformance_for_plugin(&registry, &name);
        assert!(!outcomes.is_empty());
        assert!(!outcomes.iter().any(|case| case.id == "no_conformance_runner"));
        assert!(
            outcomes
                .iter()
                .any(|case| case.id == "allowlist_rejects_unlisted_tool")
        );
    }
}
```

- [ ] **Step 2: Run the routing test**

Run:

```sh
cargo test -p cairn-core agent_provider_routes_to_conformance_runner --lib
```

Expected: PASS if Task 4 routing is correct. If it fails with `no_conformance_runner`, fix the match arm from Task 4.

- [ ] **Step 3: Run focused conformance suite**

Run:

```sh
cargo test -p cairn-core conformance --lib
```

Expected: PASS.

- [ ] **Step 4: Commit Task 6**

```sh
git add crates/cairn-core/src/contract/conformance/mod.rs
git commit -m "test(core): assert agent provider conformance routing"
```

---

### Task 7: Format, Lint Boundary, And Verify

**Files:**
- Modify only files touched by formatter if needed.

- [ ] **Step 1: Run rustfmt**

Run:

```sh
cargo fmt
```

Expected: command exits 0.

- [ ] **Step 2: Run focused core tests**

Run:

```sh
cargo test -p cairn-core agent_provider
cargo test -p cairn-core conformance
cargo test -p cairn-core --test contract_root_exports
```

Expected: all PASS.

- [ ] **Step 3: Run core boundary check**

Run:

```sh
scripts/check-core-boundary.sh
```

Expected: PASS with no core dependency boundary violations.

- [ ] **Step 4: Run broader verification if available**

Run:

```sh
cargo nextest run --workspace
cargo test --doc --workspace
```

Expected: all PASS. If `cargo nextest` is not installed, record that it was unavailable and run:

```sh
cargo test --workspace
```

Expected: all PASS or only unrelated pre-existing failures. Do not claim full verification if any command fails.

- [ ] **Step 5: Inspect final diff**

Run:

```sh
git diff --stat HEAD
git diff --check
```

Expected: only the intended `cairn-core` files changed; `git diff --check` exits 0.

- [ ] **Step 6: Commit final formatting or verification fixes**

If `cargo fmt` or small verification fixes changed files:

```sh
git add crates/cairn-core/src/contract/agent_provider.rs crates/cairn-core/src/contract/conformance/agent_provider.rs crates/cairn-core/src/contract/conformance/mod.rs crates/cairn-core/src/contract/mod.rs crates/cairn-core/tests/contract_root_exports.rs crates/cairn-core/Cargo.toml Cargo.lock
git commit -m "chore: finalize agent provider runtime checks"
```

If no files changed, skip this commit.

---

## Final Verification Checklist

- [ ] `cargo test -p cairn-core agent_provider`
- [ ] `cargo test -p cairn-core conformance`
- [ ] `cargo test -p cairn-core --test contract_root_exports`
- [ ] `scripts/check-core-boundary.sh`
- [ ] `cargo nextest run --workspace` or fallback `cargo test --workspace`
- [ ] `cargo test --doc --workspace`
- [ ] `git diff --check`

## Notes For Implementers

- Keep `cairn-core` I/O-free. Do not add subprocess execution, filesystem access, model calls, or dependencies on other workspace crates.
- Do not add an `LLMProvider::complete` method in this PR. The scripted provider is only a conformance fixture.
- Treat mutating tools as WAL-routed actions only; never model a direct vault write outcome.
- If clippy flags `expect` in library tests, the existing project convention allows `expect("invariant: ...")` in tests. Use invariant messages, not bare unwraps.
