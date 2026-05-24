# Agent Worker Audit And Canary Controls Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add body-free agent-worker audit metrics, deterministic canary controls, and lint/evaluation report projection for issue #126.

**Architecture:** Keep the first implementation pure and deterministic. `cairn-core::domain` owns audit aggregation and canary decisions, the generated `lint` DTO gets an optional machine-readable audit report field through IDL/codegen, and `cairn-workflows::evaluation` includes the same audit aggregate in report records through an in-memory injection point for tests and host wiring.

**Tech Stack:** Rust 2024, serde, serde_json, existing Cairn generated IDL/codegen, existing lint report renderer, existing EvaluationWorkflow handler, Cargo tests.

---

## File Structure

- Create `crates/cairn-core/src/domain/agent_audit.rs`
  - Body-free agent-worker audit records, typed failure modes, aggregate summaries, and mapping from `AgentProviderError`.
- Create `crates/cairn-core/src/domain/agent_canary.rs`
  - Pure canary policy validation, dispatch decisions, and aggregate-threshold decisions.
- Modify `crates/cairn-core/src/domain/mod.rs`
  - Export `agent_audit`, `agent_canary`, and the public audit/canary types.
- Modify `crates/cairn-idl/schema/verbs/lint.json`
  - Add generated JSON shape for `Data.agent_worker_audit`.
- Regenerate generated artifacts with `cargo run -p cairn-idl --bin cairn-codegen`.
- Modify generated output from codegen under:
  - `crates/cairn-core/src/generated/verbs/lint.rs`
  - `crates/cairn-core/src/generated/schemas/verbs/lint.json`
  - `crates/cairn-mcp/src/generated/schemas/verbs/lint.json`
  - `skills/cairn/.version` if codegen refreshes it
- Modify `crates/cairn-core/src/verbs/lint/mod.rs`
  - Build the generated audit report from an `AgentWorkerAuditSummary`; attach a no-data report to ordinary `run_checks` output.
- Modify `crates/cairn-core/src/verbs/lint/report.rs`
  - Render an `Agent worker audit` markdown section when `agent_worker_audit` is present.
- Modify `crates/cairn-workflows/src/evaluation/handler.rs`
  - Add an in-memory audit-record injection point, include the aggregate in `EvaluationReport`, include it in report target keys, and render it in the report body.

## Task 1: Core Agent Worker Audit Model

**Files:**
- Create: `crates/cairn-core/src/domain/agent_audit.rs`
- Modify: `crates/cairn-core/src/domain/mod.rs`
- Test: `crates/cairn-core/src/domain/agent_audit.rs`

- [ ] **Step 1: Write failing audit aggregation tests**

Create `crates/cairn-core/src/domain/agent_audit.rs` with tests first:

```rust
use std::collections::BTreeMap;

use crate::contract::agent_provider::{AgentBudgetConsumed, AgentProviderError, CairnVerb};
use crate::domain::ScopeTuple;

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(agent: &str) -> ScopeTuple {
        ScopeTuple {
            tenant: Some("tenant-a".to_owned()),
            workspace: Some("workspace-a".to_owned()),
            agent: Some(agent.to_owned()),
            ..ScopeTuple::default()
        }
    }

    fn record(
        operation_id: &str,
        status: AgentWorkerStatus,
        generated_candidates: u64,
        accepted_candidates: u64,
        cost_units: u64,
        failure_mode: Option<AgentWorkerFailureMode>,
    ) -> AgentWorkerAuditRecord {
        AgentWorkerAuditRecord {
            operation_id: operation_id.to_owned(),
            worker_kind: AgentWorkerKind::Extractor,
            worker_name: "agent_extractor".to_owned(),
            agent_identity: "agt:cairn-extractor:v1".to_owned(),
            scope: Some(scope("agt:cairn-extractor:v1")),
            status,
            generated_candidates,
            accepted_candidates,
            budget_consumed: AgentBudgetConsumed {
                turns: 2,
                tool_calls: 3,
                cost_units,
            },
            failure_mode,
            canary_label: Some("canary-05".to_owned()),
        }
    }

    #[test]
    fn aggregate_tracks_cost_candidates_acceptance_and_failures() {
        let records = vec![
            record("op-1", AgentWorkerStatus::Completed, 4, 2, 100, None),
            record(
                "op-2",
                AgentWorkerStatus::Aborted,
                0,
                0,
                50,
                Some(AgentWorkerFailureMode::BudgetExceeded),
            ),
        ];

        let summary = AgentWorkerAuditSummary::from_records(&records);

        assert_eq!(summary.total_runs, 2);
        assert_eq!(summary.completed_runs, 1);
        assert_eq!(summary.failed_runs, 1);
        assert_eq!(summary.generated_candidates, 4);
        assert_eq!(summary.accepted_candidates, 2);
        assert_eq!(summary.acceptance_rate, Some(0.5));
        assert_eq!(summary.turns, 4);
        assert_eq!(summary.tool_calls, 6);
        assert_eq!(summary.cost_units, 150);
        assert_eq!(
            summary.failure_modes.get(&AgentWorkerFailureMode::BudgetExceeded),
            Some(&1)
        );
        assert_eq!(summary.workers[0].worker_name, "agent_extractor");
    }

    #[test]
    fn aggregate_reports_no_acceptance_rate_without_generated_candidates() {
        let records = vec![record("op-1", AgentWorkerStatus::Completed, 0, 0, 10, None)];

        let summary = AgentWorkerAuditSummary::from_records(&records);

        assert_eq!(summary.generated_candidates, 0);
        assert_eq!(summary.accepted_candidates, 0);
        assert_eq!(summary.acceptance_rate, None);
    }

    #[test]
    fn record_preserves_identity_and_scope_for_aborted_runs() {
        let record = record(
            "op-1",
            AgentWorkerStatus::Aborted,
            0,
            0,
            1,
            Some(AgentWorkerFailureMode::ProviderUnavailable),
        );

        assert_eq!(record.agent_identity, "agt:cairn-extractor:v1");
        assert_eq!(
            record.scope.as_ref().and_then(|scope| scope.agent.as_deref()),
            Some("agt:cairn-extractor:v1")
        );
        assert_eq!(record.failure_mode, Some(AgentWorkerFailureMode::ProviderUnavailable));
    }

    #[test]
    fn provider_errors_map_to_failure_modes() {
        assert_eq!(
            AgentWorkerFailureMode::from_provider_error(&AgentProviderError::ToolNotAllowed {
                verb: CairnVerb::Forget,
            }),
            AgentWorkerFailureMode::ToolNotAllowed
        );
        assert_eq!(
            AgentWorkerFailureMode::from_provider_error(
                &AgentProviderError::MutatingVerbNotScoped {
                    verb: CairnVerb::Ingest,
                },
            ),
            AgentWorkerFailureMode::MutatingVerbNotScoped
        );
    }
}
```

- [ ] **Step 2: Run audit tests to verify RED**

Run:

```bash
cargo test -p cairn-core domain::agent_audit --lib
```

Expected: FAIL with unresolved items such as `AgentWorkerStatus`, `AgentWorkerAuditRecord`, and `AgentWorkerAuditSummary`.

- [ ] **Step 3: Implement the audit domain model**

Replace the top of `crates/cairn-core/src/domain/agent_audit.rs` above the test module with:

```rust
//! Body-free agent-worker audit records and aggregates for issue #126.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::contract::agent_provider::{AgentBudgetConsumed, AgentProviderError};
use crate::domain::ScopeTuple;

/// Agent-mode worker class that produced an audit record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentWorkerKind {
    /// Agent-mode extractor worker.
    Extractor,
    /// Agent-mode dream worker.
    Dream,
}

/// Host-visible status for one logical worker invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentWorkerStatus {
    /// Worker completed and returned control to the host.
    Completed,
    /// Host rejected the worker before it could run.
    Rejected,
    /// Worker aborted with a typed provider or host failure.
    Aborted,
    /// Worker output was rolled back by canary controls.
    RolledBack,
}

impl AgentWorkerStatus {
    /// Returns true when this status counts as a failed run for canary gates.
    #[must_use]
    pub const fn is_failure(self) -> bool {
        matches!(self, Self::Rejected | Self::Aborted | Self::RolledBack)
    }
}

/// Compact, body-free agent failure mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentWorkerFailureMode {
    /// Cost budget was exceeded.
    BudgetExceeded,
    /// Wall-clock budget was exceeded.
    WallClockExceeded,
    /// Agent tried to invoke a tool outside its allowlist.
    ToolNotAllowed,
    /// Agent tried to invoke a mutating verb without mutating scope.
    MutatingVerbNotScoped,
    /// Agent output did not match the requested schema.
    InvalidOutput,
    /// Provider could not run in this build or configuration.
    ProviderUnavailable,
    /// Host rejected every generated candidate.
    HostRejectedCandidates,
    /// Failure did not map to a narrower stable category.
    Unknown,
}

impl AgentWorkerFailureMode {
    /// Map an AgentProvider failure to the reportable audit category.
    #[must_use]
    pub fn from_provider_error(error: &AgentProviderError) -> Self {
        match error {
            AgentProviderError::BudgetExceeded { .. } => Self::BudgetExceeded,
            AgentProviderError::WallClockExceeded => Self::WallClockExceeded,
            AgentProviderError::ToolNotAllowed { .. } => Self::ToolNotAllowed,
            AgentProviderError::MutatingVerbNotScoped { .. } => Self::MutatingVerbNotScoped,
            AgentProviderError::InvalidOutput { .. } => Self::InvalidOutput,
            AgentProviderError::ProviderUnavailable { .. } => Self::ProviderUnavailable,
            AgentProviderError::InvalidRequest { .. } => Self::Unknown,
        }
    }

    /// Stable snake-case label for report maps.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BudgetExceeded => "budget_exceeded",
            Self::WallClockExceeded => "wall_clock_exceeded",
            Self::ToolNotAllowed => "tool_not_allowed",
            Self::MutatingVerbNotScoped => "mutating_verb_not_scoped",
            Self::InvalidOutput => "invalid_output",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::HostRejectedCandidates => "host_rejected_candidates",
            Self::Unknown => "unknown",
        }
    }
}

/// One body-free audit record for one logical agent-worker invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentWorkerAuditRecord {
    /// Stable operation or workflow id for correlation.
    pub operation_id: String,
    /// Agent-mode worker class.
    pub worker_kind: AgentWorkerKind,
    /// Stable worker label.
    pub worker_name: String,
    /// `agt:` identity used by the worker.
    pub agent_identity: String,
    /// Body-free tenant/workspace/user/agent scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<ScopeTuple>,
    /// Host-visible status.
    pub status: AgentWorkerStatus,
    /// Number of candidates produced by the worker.
    pub generated_candidates: u64,
    /// Number of generated candidates accepted by the host pipeline.
    pub accepted_candidates: u64,
    /// AgentProvider budget consumed by the run.
    pub budget_consumed: AgentBudgetConsumed,
    /// Compact failure mode when the worker failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_mode: Option<AgentWorkerFailureMode>,
    /// Optional rollout cohort label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canary_label: Option<String>,
}

/// Aggregate for one `(worker_kind, worker_name, canary_label)` group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentWorkerGroupSummary {
    /// Worker class for this group.
    pub worker_kind: AgentWorkerKind,
    /// Stable worker label.
    pub worker_name: String,
    /// Optional rollout cohort label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canary_label: Option<String>,
    /// Number of records in this group.
    pub total_runs: u64,
    /// Accepted candidates in this group.
    pub accepted_candidates: u64,
    /// Generated candidates in this group.
    pub generated_candidates: u64,
}

/// Body-free aggregate for operator reports and canary controls.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentWorkerAuditSummary {
    /// Total observed worker runs.
    pub total_runs: u64,
    /// Runs with `Completed` status.
    pub completed_runs: u64,
    /// Runs with rejected, aborted, or rolled-back status.
    pub failed_runs: u64,
    /// Generated candidates across all records.
    pub generated_candidates: u64,
    /// Accepted candidates across all records.
    pub accepted_candidates: u64,
    /// `accepted_candidates / generated_candidates`, absent when no candidates were generated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_rate: Option<f64>,
    /// Agent turns consumed.
    pub turns: u64,
    /// Agent tool calls consumed.
    pub tool_calls: u64,
    /// Provider-defined cost units consumed.
    pub cost_units: u64,
    /// Failure counts by stable failure category.
    pub failure_modes: BTreeMap<AgentWorkerFailureMode, u64>,
    /// Per-worker and per-cohort summaries.
    pub workers: Vec<AgentWorkerGroupSummary>,
}
```

Add the `impl AgentWorkerAuditSummary` block:

```rust
impl AgentWorkerAuditSummary {
    /// Build a body-free aggregate from audit records.
    #[must_use]
    pub fn from_records(records: &[AgentWorkerAuditRecord]) -> Self {
        let mut total_runs = 0_u64;
        let mut completed_runs = 0_u64;
        let mut failed_runs = 0_u64;
        let mut generated_candidates = 0_u64;
        let mut accepted_candidates = 0_u64;
        let mut turns = 0_u64;
        let mut tool_calls = 0_u64;
        let mut cost_units = 0_u64;
        let mut failure_modes: BTreeMap<AgentWorkerFailureMode, u64> = BTreeMap::new();
        let mut groups: BTreeMap<(AgentWorkerKind, String, Option<String>), AgentWorkerGroupSummary> =
            BTreeMap::new();

        for record in records {
            total_runs = total_runs.saturating_add(1);
            if record.status == AgentWorkerStatus::Completed {
                completed_runs = completed_runs.saturating_add(1);
            }
            if record.status.is_failure() {
                failed_runs = failed_runs.saturating_add(1);
            }
            generated_candidates =
                generated_candidates.saturating_add(record.generated_candidates);
            accepted_candidates = accepted_candidates.saturating_add(record.accepted_candidates);
            turns = turns.saturating_add(u64::from(record.budget_consumed.turns));
            tool_calls = tool_calls.saturating_add(u64::from(record.budget_consumed.tool_calls));
            cost_units = cost_units.saturating_add(record.budget_consumed.cost_units);

            if let Some(mode) = record.failure_mode {
                let count = failure_modes.entry(mode).or_insert(0);
                *count = count.saturating_add(1);
            }

            let key = (
                record.worker_kind,
                record.worker_name.clone(),
                record.canary_label.clone(),
            );
            let group = groups.entry(key).or_insert_with(|| AgentWorkerGroupSummary {
                worker_kind: record.worker_kind,
                worker_name: record.worker_name.clone(),
                canary_label: record.canary_label.clone(),
                total_runs: 0,
                accepted_candidates: 0,
                generated_candidates: 0,
            });
            group.total_runs = group.total_runs.saturating_add(1);
            group.accepted_candidates =
                group.accepted_candidates.saturating_add(record.accepted_candidates);
            group.generated_candidates =
                group.generated_candidates.saturating_add(record.generated_candidates);
        }

        let acceptance_rate = if generated_candidates == 0 {
            None
        } else {
            Some(accepted_candidates as f64 / generated_candidates as f64)
        };

        Self {
            total_runs,
            completed_runs,
            failed_runs,
            generated_candidates,
            accepted_candidates,
            acceptance_rate,
            turns,
            tool_calls,
            cost_units,
            failure_modes,
            workers: groups.into_values().collect(),
        }
    }

    /// True when no worker audit records were observed.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.total_runs == 0
    }
}
```

- [ ] **Step 4: Export the audit module**

Modify `crates/cairn-core/src/domain/mod.rs`:

```rust
pub mod agent_audit;
```

Add exports with the other `pub use` lines:

```rust
pub use agent_audit::{
    AgentWorkerAuditRecord, AgentWorkerAuditSummary, AgentWorkerFailureMode,
    AgentWorkerGroupSummary, AgentWorkerKind, AgentWorkerStatus,
};
```

- [ ] **Step 5: Run audit tests to verify GREEN**

Run:

```bash
cargo test -p cairn-core domain::agent_audit --lib
```

Expected: PASS for all `agent_audit` tests.

- [ ] **Step 6: Commit Task 1**

Run:

```bash
git add crates/cairn-core/src/domain/agent_audit.rs crates/cairn-core/src/domain/mod.rs
git commit -m "feat(core): add agent worker audit aggregate"
```

## Task 2: Core Agent Canary Controls

**Files:**
- Create: `crates/cairn-core/src/domain/agent_canary.rs`
- Modify: `crates/cairn-core/src/domain/mod.rs`
- Test: `crates/cairn-core/src/domain/agent_canary.rs`

- [ ] **Step 1: Write failing canary tests**

Create `crates/cairn-core/src/domain/agent_canary.rs` with tests first:

```rust
use crate::domain::AgentWorkerAuditSummary;

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(total: u64, failed: u64, generated: u64, accepted: u64, cost: u64) -> AgentWorkerAuditSummary {
        AgentWorkerAuditSummary {
            total_runs: total,
            completed_runs: total.saturating_sub(failed),
            failed_runs: failed,
            generated_candidates: generated,
            accepted_candidates: accepted,
            acceptance_rate: if generated == 0 {
                None
            } else {
                Some(accepted as f64 / generated as f64)
            },
            turns: 0,
            tool_calls: 0,
            cost_units: cost,
            failure_modes: Default::default(),
            workers: Vec::new(),
        }
    }

    #[test]
    fn paused_rollout_denies_dispatch() {
        let policy = AgentCanaryPolicy {
            state: AgentCanaryState::Paused,
            rollout_percent: 5,
            min_runs: 10,
            min_acceptance_rate: None,
            max_failure_rate: None,
            max_cost_units_per_accepted_candidate: None,
            pause_requested: false,
            rollback_requested: false,
        };

        let decision = policy.dispatch_decision(0).expect("valid policy");

        assert_eq!(decision.kind, AgentCanaryDecisionKind::DispatchDeniedPaused);
    }

    #[test]
    fn canary_rollout_denies_outside_cohort() {
        let policy = AgentCanaryPolicy {
            state: AgentCanaryState::Canary,
            rollout_percent: 5,
            min_runs: 10,
            min_acceptance_rate: None,
            max_failure_rate: None,
            max_cost_units_per_accepted_candidate: None,
            pause_requested: false,
            rollback_requested: false,
        };

        let decision = policy.dispatch_decision(50).expect("valid policy");

        assert_eq!(decision.kind, AgentCanaryDecisionKind::DispatchDeniedOutsideCanary);
    }

    #[test]
    fn canary_remains_when_insufficient_runs() {
        let policy = AgentCanaryPolicy {
            state: AgentCanaryState::Canary,
            rollout_percent: 5,
            min_runs: 10,
            min_acceptance_rate: Some(0.50),
            max_failure_rate: Some(0.20),
            max_cost_units_per_accepted_candidate: None,
            pause_requested: false,
            rollback_requested: false,
        };

        let decision = policy.evaluate_summary(&summary(3, 0, 4, 4, 20)).expect("valid policy");

        assert_eq!(decision.kind, AgentCanaryDecisionKind::RemainCanaryInsufficientData);
    }

    #[test]
    fn canary_rolls_back_when_failure_rate_exceeds_threshold() {
        let policy = AgentCanaryPolicy {
            state: AgentCanaryState::Canary,
            rollout_percent: 5,
            min_runs: 4,
            min_acceptance_rate: Some(0.50),
            max_failure_rate: Some(0.20),
            max_cost_units_per_accepted_candidate: None,
            pause_requested: false,
            rollback_requested: false,
        };

        let decision = policy.evaluate_summary(&summary(5, 2, 4, 4, 20)).expect("valid policy");

        assert_eq!(decision.kind, AgentCanaryDecisionKind::RollbackThresholdFailed);
        assert_eq!(decision.next_state, AgentCanaryState::RolledBack);
    }

    #[test]
    fn canary_promotes_when_thresholds_pass() {
        let policy = AgentCanaryPolicy {
            state: AgentCanaryState::Canary,
            rollout_percent: 5,
            min_runs: 4,
            min_acceptance_rate: Some(0.50),
            max_failure_rate: Some(0.20),
            max_cost_units_per_accepted_candidate: Some(20),
            pause_requested: false,
            rollback_requested: false,
        };

        let decision = policy.evaluate_summary(&summary(5, 0, 10, 8, 80)).expect("valid policy");

        assert_eq!(decision.kind, AgentCanaryDecisionKind::PromoteToEnabled);
        assert_eq!(decision.next_state, AgentCanaryState::Enabled);
    }

    #[test]
    fn explicit_rollback_wins_over_promotion() {
        let policy = AgentCanaryPolicy {
            state: AgentCanaryState::Canary,
            rollout_percent: 5,
            min_runs: 1,
            min_acceptance_rate: None,
            max_failure_rate: None,
            max_cost_units_per_accepted_candidate: None,
            pause_requested: false,
            rollback_requested: true,
        };

        let decision = policy.evaluate_summary(&summary(5, 0, 10, 10, 10)).expect("valid policy");

        assert_eq!(decision.kind, AgentCanaryDecisionKind::RollbackRequested);
        assert_eq!(decision.next_state, AgentCanaryState::RolledBack);
    }
}
```

- [ ] **Step 2: Run canary tests to verify RED**

Run:

```bash
cargo test -p cairn-core domain::agent_canary --lib
```

Expected: FAIL with unresolved items such as `AgentCanaryPolicy`, `AgentCanaryState`, and `AgentCanaryDecisionKind`.

- [ ] **Step 3: Implement canary state and decisions**

Replace the top of `crates/cairn-core/src/domain/agent_canary.rs` above the test module with:

```rust
//! Pure canary controls for agent-mode workers.

use serde::{Deserialize, Serialize};

use crate::domain::AgentWorkerAuditSummary;

/// Operator-visible rollout state for agent-mode workers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCanaryState {
    /// Do not dispatch agent-mode workers.
    Paused,
    /// Dispatch only for a bounded canary cohort.
    Canary,
    /// Dispatch for all configured eligible traffic.
    Enabled,
    /// Do not dispatch until explicitly reset.
    RolledBack,
}

impl AgentCanaryState {
    /// Stable snake-case label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Paused => "paused",
            Self::Canary => "canary",
            Self::Enabled => "enabled",
            Self::RolledBack => "rolled_back",
        }
    }
}

/// Pure canary policy evaluated before dispatch and after audit aggregation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCanaryPolicy {
    /// Current rollout state.
    pub state: AgentCanaryState,
    /// Percentage of traffic allowed in canary state, 0 through 100.
    pub rollout_percent: u8,
    /// Minimum observed runs before judging canary metrics.
    pub min_runs: u64,
    /// Minimum accepted/generated rate required for promotion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_acceptance_rate: Option<f64>,
    /// Maximum failed/total rate allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_failure_rate: Option<f64>,
    /// Maximum cost units per accepted candidate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_units_per_accepted_candidate: Option<u64>,
    /// Explicit operator pause.
    pub pause_requested: bool,
    /// Explicit operator rollback.
    pub rollback_requested: bool,
}

/// Stable decision category for reports and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCanaryDecisionKind {
    /// Dispatch can proceed.
    DispatchAllowed,
    /// Dispatch denied because agent mode is paused.
    DispatchDeniedPaused,
    /// Dispatch denied because agent mode is rolled back.
    DispatchDeniedRolledBack,
    /// Dispatch denied because this request is outside the canary cohort.
    DispatchDeniedOutsideCanary,
    /// Canary stays in observation mode.
    RemainCanaryInsufficientData,
    /// Canary passed configured thresholds.
    PromoteToEnabled,
    /// Canary failed a configured threshold.
    RollbackThresholdFailed,
    /// Operator requested rollback.
    RollbackRequested,
}

/// Pure canary decision with the next rollout state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCanaryDecision {
    /// Stable decision kind.
    pub kind: AgentCanaryDecisionKind,
    /// State that should be used after applying the decision.
    pub next_state: AgentCanaryState,
    /// Compact body-free reason.
    pub reason: String,
}

/// Validation failures for impossible canary policies.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum AgentCanaryError {
    /// `min_runs` must be nonzero.
    #[error("agent canary min_runs must be nonzero")]
    ZeroMinRuns,
    /// A rate field was outside 0.0 through 1.0.
    #[error("agent canary rate {field} must be between 0.0 and 1.0")]
    RateOutOfRange {
        /// Field name.
        field: &'static str,
    },
}
```

Add the policy implementation:

```rust
impl AgentCanaryPolicy {
    /// Validate policy invariants.
    ///
    /// # Errors
    /// Returns [`AgentCanaryError`] when a threshold is impossible.
    pub fn validate(&self) -> Result<(), AgentCanaryError> {
        if self.min_runs == 0 {
            return Err(AgentCanaryError::ZeroMinRuns);
        }
        validate_rate("min_acceptance_rate", self.min_acceptance_rate)?;
        validate_rate("max_failure_rate", self.max_failure_rate)?;
        Ok(())
    }

    /// Decide whether one request in `cohort_percentile` may dispatch.
    ///
    /// # Errors
    /// Returns [`AgentCanaryError`] when the policy is invalid.
    pub fn dispatch_decision(
        &self,
        cohort_percentile: u8,
    ) -> Result<AgentCanaryDecision, AgentCanaryError> {
        self.validate()?;
        if self.rollback_requested {
            return Ok(decision(
                AgentCanaryDecisionKind::RollbackRequested,
                AgentCanaryState::RolledBack,
                "operator requested rollback",
            ));
        }
        if self.pause_requested || self.state == AgentCanaryState::Paused {
            return Ok(decision(
                AgentCanaryDecisionKind::DispatchDeniedPaused,
                AgentCanaryState::Paused,
                "agent mode is paused",
            ));
        }
        if self.state == AgentCanaryState::RolledBack {
            return Ok(decision(
                AgentCanaryDecisionKind::DispatchDeniedRolledBack,
                AgentCanaryState::RolledBack,
                "agent mode is rolled back",
            ));
        }
        if self.state == AgentCanaryState::Canary
            && cohort_percentile >= self.rollout_percent
        {
            return Ok(decision(
                AgentCanaryDecisionKind::DispatchDeniedOutsideCanary,
                AgentCanaryState::Canary,
                "request is outside the canary cohort",
            ));
        }
        Ok(decision(
            AgentCanaryDecisionKind::DispatchAllowed,
            self.state,
            "dispatch allowed",
        ))
    }

    /// Evaluate aggregate canary metrics after worker audit records are summarized.
    ///
    /// # Errors
    /// Returns [`AgentCanaryError`] when the policy is invalid.
    pub fn evaluate_summary(
        &self,
        summary: &AgentWorkerAuditSummary,
    ) -> Result<AgentCanaryDecision, AgentCanaryError> {
        self.validate()?;
        if self.rollback_requested {
            return Ok(decision(
                AgentCanaryDecisionKind::RollbackRequested,
                AgentCanaryState::RolledBack,
                "operator requested rollback",
            ));
        }
        if self.pause_requested || self.state == AgentCanaryState::Paused {
            return Ok(decision(
                AgentCanaryDecisionKind::DispatchDeniedPaused,
                AgentCanaryState::Paused,
                "agent mode is paused",
            ));
        }
        if self.state == AgentCanaryState::RolledBack {
            return Ok(decision(
                AgentCanaryDecisionKind::DispatchDeniedRolledBack,
                AgentCanaryState::RolledBack,
                "agent mode is rolled back",
            ));
        }
        if self.state != AgentCanaryState::Canary {
            return Ok(decision(
                AgentCanaryDecisionKind::DispatchAllowed,
                self.state,
                "non-canary state has no aggregate transition",
            ));
        }
        if summary.total_runs < self.min_runs {
            return Ok(decision(
                AgentCanaryDecisionKind::RemainCanaryInsufficientData,
                AgentCanaryState::Canary,
                "not enough audit records to judge canary",
            ));
        }
        if let Some(max_failure_rate) = self.max_failure_rate {
            let failure_rate = summary.failed_runs as f64 / summary.total_runs as f64;
            if failure_rate > max_failure_rate {
                return Ok(decision(
                    AgentCanaryDecisionKind::RollbackThresholdFailed,
                    AgentCanaryState::RolledBack,
                    "failure rate exceeded canary threshold",
                ));
            }
        }
        if let Some(min_acceptance_rate) = self.min_acceptance_rate {
            if summary.acceptance_rate.unwrap_or(0.0) < min_acceptance_rate {
                return Ok(decision(
                    AgentCanaryDecisionKind::RollbackThresholdFailed,
                    AgentCanaryState::RolledBack,
                    "acceptance rate fell below canary threshold",
                ));
            }
        }
        if let Some(max_cost) = self.max_cost_units_per_accepted_candidate {
            if summary.accepted_candidates == 0
                || summary.cost_units / summary.accepted_candidates > max_cost
            {
                return Ok(decision(
                    AgentCanaryDecisionKind::RollbackThresholdFailed,
                    AgentCanaryState::RolledBack,
                    "cost per accepted candidate exceeded canary threshold",
                ));
            }
        }
        Ok(decision(
            AgentCanaryDecisionKind::PromoteToEnabled,
            AgentCanaryState::Enabled,
            "canary thresholds passed",
        ))
    }
}

fn validate_rate(field: &'static str, value: Option<f64>) -> Result<(), AgentCanaryError> {
    if value.is_some_and(|rate| !(0.0..=1.0).contains(&rate)) {
        return Err(AgentCanaryError::RateOutOfRange { field });
    }
    Ok(())
}

fn decision(
    kind: AgentCanaryDecisionKind,
    next_state: AgentCanaryState,
    reason: &str,
) -> AgentCanaryDecision {
    AgentCanaryDecision {
        kind,
        next_state,
        reason: reason.to_owned(),
    }
}
```

- [ ] **Step 4: Export the canary module**

Modify `crates/cairn-core/src/domain/mod.rs`:

```rust
pub mod agent_canary;
```

Add exports:

```rust
pub use agent_canary::{
    AgentCanaryDecision, AgentCanaryDecisionKind, AgentCanaryError, AgentCanaryPolicy,
    AgentCanaryState,
};
```

- [ ] **Step 5: Run canary tests to verify GREEN**

Run:

```bash
cargo test -p cairn-core domain::agent_canary --lib
```

Expected: PASS for all `agent_canary` tests.

- [ ] **Step 6: Commit Task 2**

Run:

```bash
git add crates/cairn-core/src/domain/agent_canary.rs crates/cairn-core/src/domain/mod.rs
git commit -m "feat(core): add agent canary controls"
```

## Task 3: Lint JSON And Markdown Report Projection

**Files:**
- Modify: `crates/cairn-idl/schema/verbs/lint.json`
- Modify after codegen: `crates/cairn-core/src/generated/verbs/lint.rs`
- Modify after codegen: `crates/cairn-core/src/generated/schemas/verbs/lint.json`
- Modify after codegen: `crates/cairn-mcp/src/generated/schemas/verbs/lint.json`
- Modify after codegen: generated skill files if codegen refreshes them
- Modify: `crates/cairn-core/src/verbs/lint/mod.rs`
- Modify: `crates/cairn-core/src/verbs/lint/report.rs`
- Test: `crates/cairn-core/src/verbs/lint/mod.rs`
- Test: `crates/cairn-core/src/verbs/lint/report.rs`

- [ ] **Step 1: Write failing lint projection tests**

In `crates/cairn-core/src/verbs/lint/mod.rs`, add this test inside the existing `#[cfg(test)] mod tests`:

```rust
#[test]
fn agent_worker_audit_report_projects_summary() {
    use crate::domain::{
        AgentCanaryState, AgentWorkerAuditSummary, AgentWorkerFailureMode,
        AgentWorkerGroupSummary, AgentWorkerKind,
    };

    let mut failure_modes = std::collections::BTreeMap::new();
    failure_modes.insert(AgentWorkerFailureMode::BudgetExceeded, 2);
    let summary = AgentWorkerAuditSummary {
        total_runs: 4,
        completed_runs: 2,
        failed_runs: 2,
        generated_candidates: 10,
        accepted_candidates: 5,
        acceptance_rate: Some(0.5),
        turns: 8,
        tool_calls: 12,
        cost_units: 200,
        failure_modes,
        workers: vec![AgentWorkerGroupSummary {
            worker_kind: AgentWorkerKind::Extractor,
            worker_name: "agent_extractor".to_owned(),
            canary_label: Some("canary-05".to_owned()),
            total_runs: 4,
            accepted_candidates: 5,
            generated_candidates: 10,
        }],
    };

    let report = agent_worker_audit_report(&summary, Some(AgentCanaryState::Canary));

    assert!(report.observed_records);
    assert_eq!(report.rollout_state.as_deref(), Some("canary"));
    assert_eq!(report.total_runs, 4);
    assert_eq!(report.accepted_candidates, 5);
    assert_eq!(report.acceptance_rate, Some(0.5));
    assert_eq!(report.failure_modes["budget_exceeded"], serde_json::json!(2));
    assert_eq!(report.workers.len(), 1);
}
```

In `crates/cairn-core/src/verbs/lint/report.rs`, add this test:

```rust
#[test]
fn agent_worker_audit_section_renders_without_body_text() {
    let data = LintData {
        findings: vec![],
        summary: empty_summary(),
        report_path: None,
        agent_worker_audit: Some(crate::generated::verbs::lint::AgentWorkerAuditReport {
            observed_records: true,
            rollout_state: Some("canary".to_owned()),
            total_runs: 4,
            completed_runs: 2,
            failed_runs: 2,
            generated_candidates: 10,
            accepted_candidates: 5,
            acceptance_rate: Some(0.5),
            turns: 8,
            tool_calls: 12,
            cost_units: 200,
            failure_modes: serde_json::json!({"budget_exceeded": 2}),
            workers: Vec::new(),
        }),
    };

    let rendered = render(&data);

    assert!(rendered.contains("## Agent worker audit"));
    assert!(rendered.contains("- state: canary"));
    assert!(rendered.contains("- accepted candidates: 5 / 10"));
    assert!(rendered.contains("- failures: budget_exceeded=2"));
    assert!(!rendered.contains("prompt"));
    assert!(!rendered.contains("candidate body"));
}
```

- [ ] **Step 2: Run lint tests to verify RED**

Run:

```bash
cargo test -p cairn-core agent_worker_audit --lib
```

Expected: FAIL with missing generated type or field errors for `AgentWorkerAuditReport` and `LintData.agent_worker_audit`.

- [ ] **Step 3: Update lint IDL schema**

Modify `crates/cairn-idl/schema/verbs/lint.json`.

Add these definitions under `$defs` after `Target`:

```json
"AgentWorkerAuditWorker": {
  "type": "object",
  "additionalProperties": false,
  "required": ["worker_kind", "worker_name", "total_runs", "generated_candidates", "accepted_candidates"],
  "properties": {
    "worker_kind": {
      "type": "string",
      "enum": ["extractor", "dream"]
    },
    "worker_name": {
      "type": "string",
      "minLength": 1
    },
    "canary_label": {
      "type": "string",
      "minLength": 1
    },
    "total_runs": {
      "type": "integer",
      "minimum": 0
    },
    "generated_candidates": {
      "type": "integer",
      "minimum": 0
    },
    "accepted_candidates": {
      "type": "integer",
      "minimum": 0
    }
  }
},
"AgentWorkerAuditReport": {
  "type": "object",
  "additionalProperties": false,
  "required": [
    "observed_records",
    "total_runs",
    "completed_runs",
    "failed_runs",
    "generated_candidates",
    "accepted_candidates",
    "turns",
    "tool_calls",
    "cost_units",
    "failure_modes",
    "workers"
  ],
  "properties": {
    "observed_records": {
      "type": "boolean"
    },
    "rollout_state": {
      "type": "string",
      "enum": ["paused", "canary", "enabled", "rolled_back"]
    },
    "total_runs": {
      "type": "integer",
      "minimum": 0
    },
    "completed_runs": {
      "type": "integer",
      "minimum": 0
    },
    "failed_runs": {
      "type": "integer",
      "minimum": 0
    },
    "generated_candidates": {
      "type": "integer",
      "minimum": 0
    },
    "accepted_candidates": {
      "type": "integer",
      "minimum": 0
    },
    "acceptance_rate": {
      "type": "number",
      "minimum": 0,
      "maximum": 1
    },
    "turns": {
      "type": "integer",
      "minimum": 0
    },
    "tool_calls": {
      "type": "integer",
      "minimum": 0
    },
    "cost_units": {
      "type": "integer",
      "minimum": 0
    },
    "failure_modes": {
      "type": "object",
      "additionalProperties": {
        "type": "integer",
        "minimum": 0
      }
    },
    "workers": {
      "type": "array",
      "items": {
        "$ref": "#/$defs/AgentWorkerAuditWorker"
      }
    }
  }
}
```

Add this optional field to `$defs.Data.properties`:

```json
"agent_worker_audit": {
  "$ref": "#/$defs/AgentWorkerAuditReport"
}
```

- [ ] **Step 4: Regenerate IDL artifacts**

Run:

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked
```

Expected: command exits 0 and rewrites generated artifacts. Inspect generated `crates/cairn-core/src/generated/verbs/lint.rs`; it should include `AgentWorkerAuditReport`, `AgentWorkerAuditWorker`, and `LintData { agent_worker_audit: Option<AgentWorkerAuditReport>, ... }`.

- [ ] **Step 5: Implement lint projection helper and default no-data report**

In `crates/cairn-core/src/verbs/lint/mod.rs`, add imports:

```rust
use crate::domain::{AgentCanaryState, AgentWorkerAuditSummary};
```

In `run_checks`, build the no-data report:

```rust
let agent_worker_audit = agent_worker_audit_report(
    &AgentWorkerAuditSummary::from_records(&[]),
    None,
);
LintData {
    findings,
    summary,
    report_path: None,
    agent_worker_audit: Some(agent_worker_audit),
}
```

Add this public helper near `kind_key`:

```rust
/// Project a core agent-worker summary into the generated lint DTO.
#[must_use]
pub fn agent_worker_audit_report(
    summary: &AgentWorkerAuditSummary,
    rollout_state: Option<AgentCanaryState>,
) -> crate::generated::verbs::lint::AgentWorkerAuditReport {
    let mut failure_modes = serde_json::Map::new();
    for (mode, count) in &summary.failure_modes {
        failure_modes.insert(mode.as_str().to_owned(), serde_json::json!(count));
    }
    crate::generated::verbs::lint::AgentWorkerAuditReport {
        observed_records: !summary.is_empty(),
        rollout_state: rollout_state.map(|state| state.as_str().to_owned()),
        total_runs: summary.total_runs,
        completed_runs: summary.completed_runs,
        failed_runs: summary.failed_runs,
        generated_candidates: summary.generated_candidates,
        accepted_candidates: summary.accepted_candidates,
        acceptance_rate: summary.acceptance_rate,
        turns: summary.turns,
        tool_calls: summary.tool_calls,
        cost_units: summary.cost_units,
        failure_modes: serde_json::Value::Object(failure_modes),
        workers: summary
            .workers
            .iter()
            .map(|worker| crate::generated::verbs::lint::AgentWorkerAuditWorker {
                worker_kind: match worker.worker_kind {
                    crate::domain::AgentWorkerKind::Extractor => "extractor".to_owned(),
                    crate::domain::AgentWorkerKind::Dream => "dream".to_owned(),
                },
                worker_name: worker.worker_name.clone(),
                canary_label: worker.canary_label.clone(),
                total_runs: worker.total_runs,
                generated_candidates: worker.generated_candidates,
                accepted_candidates: worker.accepted_candidates,
            })
            .collect(),
    }
}
```

Update all direct `LintData { ... }` initializers in `crates/cairn-core/src/verbs/lint/report.rs` and `crates/cairn-cli/src/verbs/lint.rs` to include:

```rust
agent_worker_audit: None,
```

- [ ] **Step 6: Render markdown agent audit section**

In `crates/cairn-core/src/verbs/lint/report.rs`, after the severity summary and before the coverage section, add:

```rust
if let Some(agent) = &data.agent_worker_audit {
    out.push_str("## Agent worker audit\n\n");
    if !agent.observed_records {
        out.push_str("- no agent-worker audit records observed\n\n");
    } else {
        let state = agent.rollout_state.as_deref().unwrap_or("unconfigured");
        let _ = writeln!(out, "- state: {state}");
        let _ = writeln!(out, "- runs: {}", agent.total_runs);
        let _ = writeln!(
            out,
            "- accepted candidates: {} / {}",
            agent.accepted_candidates, agent.generated_candidates
        );
        let _ = writeln!(out, "- cost units: {}", agent.cost_units);
        let _ = writeln!(out, "- tool calls: {}", agent.tool_calls);
        let failures = render_failure_modes(&agent.failure_modes);
        let _ = writeln!(out, "- failures: {failures}");
        out.push('\n');
    }
}
```

Add helper:

```rust
fn render_failure_modes(value: &serde_json::Value) -> String {
    let Some(map) = value.as_object() else {
        return "none".to_owned();
    };
    if map.is_empty() {
        return "none".to_owned();
    }
    map.iter()
        .filter_map(|(key, value)| value.as_u64().map(|count| format!("{key}={count}")))
        .collect::<Vec<_>>()
        .join(", ")
}
```

- [ ] **Step 7: Run lint tests to verify GREEN**

Run:

```bash
cargo test -p cairn-core agent_worker_audit --lib
cargo test -p cairn-core verbs::lint::report --lib
```

Expected: PASS. Snapshot tests may create `.snap.new`; inspect them and accept intentional markdown changes with:

```bash
cargo insta accept -p cairn-core
```

- [ ] **Step 8: Verify generated artifacts are clean**

Run:

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Expected: PASS with `cairn-codegen: clean`.

- [ ] **Step 9: Commit Task 3**

Run:

```bash
git add crates/cairn-idl/schema/verbs/lint.json crates/cairn-core/src/generated crates/cairn-mcp/src/generated skills/cairn crates/cairn-core/src/verbs/lint/mod.rs crates/cairn-core/src/verbs/lint/report.rs crates/cairn-cli/src/verbs/lint.rs
git commit -m "feat(lint): report agent worker audit metrics"
```

## Task 4: Evaluation Report Projection

**Files:**
- Modify: `crates/cairn-workflows/src/evaluation/handler.rs`
- Test: `crates/cairn-workflows/src/evaluation/handler.rs`

- [ ] **Step 1: Write failing evaluation tests**

In `crates/cairn-workflows/src/evaluation/handler.rs`, add this test in the existing `#[cfg(test)] mod tests`:

```rust
#[tokio::test]
async fn evaluation_report_includes_agent_worker_audit_summary() {
    use cairn_core::contract::agent_provider::AgentBudgetConsumed;
    use cairn_core::domain::{
        AgentWorkerAuditRecord, AgentWorkerFailureMode, AgentWorkerKind, AgentWorkerStatus,
        ScopeTuple,
    };

    let store: Arc<dyn MemoryStore> = Arc::new(NoopMemoryStore::default());
    let sink = metrics();
    let audit_records = vec![AgentWorkerAuditRecord {
        operation_id: "op-agent-1".to_owned(),
        worker_kind: AgentWorkerKind::Dream,
        worker_name: "agent_dream".to_owned(),
        agent_identity: "agt:cairn-dream:v1".to_owned(),
        scope: Some(ScopeTuple {
            tenant: Some("tenant-a".to_owned()),
            agent: Some("agt:cairn-dream:v1".to_owned()),
            ..ScopeTuple::default()
        }),
        status: AgentWorkerStatus::Aborted,
        generated_candidates: 2,
        accepted_candidates: 1,
        budget_consumed: AgentBudgetConsumed {
            turns: 1,
            tool_calls: 2,
            cost_units: 99,
        },
        failure_mode: Some(AgentWorkerFailureMode::ProviderUnavailable),
        canary_label: Some("canary-05".to_owned()),
    }];
    let h = EvaluationHandler::new(
        store,
        sink.clone() as Arc<dyn MetricsSink>,
        default_checks(),
        EvaluationConfig {
            enabled: true,
            write_report_record: false,
            ..EvaluationConfig::default()
        },
    )
    .with_agent_worker_audit(audit_records);
    let payload = EvaluationPayload {
        ts_ms: 1_700_000_000_000,
        check_ids: vec![],
        bound_scope: None,
    };

    let report = h.run_once(&payload).await.expect("run_once");

    assert_eq!(report.agent_worker_audit.total_runs, 1);
    assert_eq!(report.agent_worker_audit.accepted_candidates, 1);
    assert_eq!(report.agent_worker_audit.cost_units, 99);
    assert_eq!(
        report
            .agent_worker_audit
            .failure_modes
            .get(&AgentWorkerFailureMode::ProviderUnavailable),
        Some(&1)
    );
}
```

- [ ] **Step 2: Run evaluation test to verify RED**

Run:

```bash
cargo test -p cairn-workflows evaluation_report_includes_agent_worker_audit_summary
```

Expected: FAIL because `EvaluationHandler::with_agent_worker_audit` and `EvaluationReport.agent_worker_audit` do not exist.

- [ ] **Step 3: Add handler audit storage and report field**

In `crates/cairn-workflows/src/evaluation/handler.rs`, update imports:

```rust
use cairn_core::domain::{
    AgentWorkerAuditRecord, AgentWorkerAuditSummary, ScopeTuple,
    metrics::MetricEvent,
    taxonomy::{MemoryClass, MemoryKind},
};
```

Add to `EvaluationReport`:

```rust
/// Body-free aggregate for agent-mode worker audit records observed by this sweep.
pub agent_worker_audit: AgentWorkerAuditSummary,
```

Add to `EvaluationHandler`:

```rust
agent_worker_audit: Vec<AgentWorkerAuditRecord>,
```

In `EvaluationHandler::new`, initialize:

```rust
agent_worker_audit: Vec::new(),
```

Add builder:

```rust
/// Attach body-free agent-worker audit records for this handler instance.
#[must_use]
pub fn with_agent_worker_audit(mut self, records: Vec<AgentWorkerAuditRecord>) -> Self {
    self.agent_worker_audit = records;
    self
}
```

In `run_once`, compute after sorting findings:

```rust
let agent_worker_audit = AgentWorkerAuditSummary::from_records(&self.agent_worker_audit);
```

Pass `&agent_worker_audit` to `report_target_key` and `upsert_report_record`, and include it in the returned report:

```rust
Ok(EvaluationReport {
    checks_run,
    passed,
    failed,
    report_target_id: Some(report_target_id),
    agent_worker_audit,
})
```

- [ ] **Step 4: Make evaluation target keys include audit state**

Change the signature:

```rust
fn report_target_key(
    payload: &EvaluationPayload,
    findings: &[(String, CheckOutcome)],
    agent_worker_audit: &AgentWorkerAuditSummary,
) -> String
```

Before computing `outcome_hash`, append the audit summary JSON into `outcome_basis`:

```rust
let audit_basis = serde_json::to_string(agent_worker_audit)
    .unwrap_or_else(|_| "agent_worker_audit_unserializable".to_owned());
let outcome_basis = format!("{outcome_basis}|agent_worker_audit={audit_basis}");
let outcome_hash = crate::synthetic::sha256_hex(outcome_basis.as_bytes());
```

Update the call site:

```rust
let target_key = Self::report_target_key(payload, &findings, &agent_worker_audit);
```

- [ ] **Step 5: Render evaluation report audit section**

Change `upsert_report_record` signature:

```rust
async fn upsert_report_record(
    &self,
    payload: &EvaluationPayload,
    findings: &[(String, CheckOutcome)],
    target_key: &str,
    agent_worker_audit: &AgentWorkerAuditSummary,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>>
```

After rendering check findings, add:

```rust
body.push_str("\n## Agent worker audit\n\n");
if agent_worker_audit.is_empty() {
    body.push_str("- no agent-worker audit records observed\n");
} else {
    body.push_str("- runs: ");
    body.push_str(&agent_worker_audit.total_runs.to_string());
    body.push('\n');
    body.push_str("- accepted candidates: ");
    body.push_str(&agent_worker_audit.accepted_candidates.to_string());
    body.push_str(" / ");
    body.push_str(&agent_worker_audit.generated_candidates.to_string());
    body.push('\n');
    body.push_str("- cost units: ");
    body.push_str(&agent_worker_audit.cost_units.to_string());
    body.push('\n');
    body.push_str("- tool calls: ");
    body.push_str(&agent_worker_audit.tool_calls.to_string());
    body.push('\n');
}
```

Add audit summary to `extras`:

```rust
"agent_worker_audit": agent_worker_audit,
```

inside the existing `"evaluation"` JSON object.

Update `upsert_report_record` call:

```rust
self.upsert_report_record(payload, &findings, &target_key, &agent_worker_audit)
    .await?
```

- [ ] **Step 6: Update existing evaluation report construction assertions**

Existing tests that assert `EvaluationReport` fields can continue to use direct field access. No literal `EvaluationReport { ... }` initializers exist outside the handler return path, so no other compile updates are expected.

- [ ] **Step 7: Run evaluation tests to verify GREEN**

Run:

```bash
cargo test -p cairn-workflows evaluation_report_includes_agent_worker_audit_summary
cargo test -p cairn-workflows evaluation --lib
```

Expected: PASS.

- [ ] **Step 8: Commit Task 4**

Run:

```bash
git add crates/cairn-workflows/src/evaluation/handler.rs
git commit -m "feat(workflows): include agent audit in evaluation reports"
```

## Task 5: Focused Verification And Drift Checks

**Files:**
- No new files.
- Verify all files changed by Tasks 1 through 4.

- [ ] **Step 1: Run focused core tests**

Run:

```bash
cargo test -p cairn-core domain::agent_audit --lib
cargo test -p cairn-core domain::agent_canary --lib
cargo test -p cairn-core agent_worker_audit --lib
cargo test -p cairn-core verbs::lint::report --lib
```

Expected: all commands PASS.

- [ ] **Step 2: Run workflow tests**

Run:

```bash
cargo test -p cairn-workflows evaluation --lib
```

Expected: PASS.

- [ ] **Step 3: Verify generated IDL artifacts**

Run:

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Expected: PASS with `cairn-codegen: clean`.

- [ ] **Step 4: Verify core boundary**

Run:

```bash
./scripts/check-core-boundary.sh
```

Expected: PASS.

- [ ] **Step 5: Run formatting**

Run:

```bash
cargo fmt --all --check
```

Expected: PASS. If formatting fails, run `cargo fmt --all`, inspect the diff, then rerun `cargo fmt --all --check`.

- [ ] **Step 6: Run compile check for touched crates**

Run:

```bash
cargo check -p cairn-core -p cairn-workflows -p cairn-cli -p cairn-mcp --all-targets --locked
```

Expected: PASS.

- [ ] **Step 7: Commit verification-only generated or snapshot changes**

If Step 5 formatting or intentional snapshot acceptance changed files, commit them:

```bash
git add crates/cairn-core crates/cairn-workflows crates/cairn-cli crates/cairn-mcp crates/cairn-idl skills/cairn
git commit -m "test: update agent audit report snapshots"
```

Skip this commit when there are no files to commit.

## Self-Review Checklist

- Spec coverage:
  - Audit record identity, scope, cost, tool call, candidate, acceptance, and failure fields are covered by Task 1.
  - Canary pause, cohort dispatch, metric rollback, explicit rollback, and promotion are covered by Task 2.
  - Lint JSON and markdown report projection are covered by Task 3.
  - Evaluation report projection is covered by Task 4.
  - Verification commands are covered by Task 5.
- Placeholder scan:
  - The plan contains no unresolved marker text or omitted code blocks.
- Type consistency:
  - `AgentWorkerAuditSummary` is the single aggregate type shared by lint and evaluation.
  - `AgentCanaryState::as_str()` supplies the generated lint `rollout_state` string.
  - `AgentWorkerFailureMode::as_str()` supplies stable JSON keys for report maps.
