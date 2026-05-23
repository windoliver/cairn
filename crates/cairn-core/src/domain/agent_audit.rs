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
        let mut groups: BTreeMap<
            (AgentWorkerKind, String, Option<String>),
            AgentWorkerGroupSummary,
        > = BTreeMap::new();

        for record in records {
            total_runs = total_runs.saturating_add(1);
            if record.status == AgentWorkerStatus::Completed {
                completed_runs = completed_runs.saturating_add(1);
            }
            if record.status.is_failure() {
                failed_runs = failed_runs.saturating_add(1);
            }
            generated_candidates = generated_candidates.saturating_add(record.generated_candidates);
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
            let group = groups
                .entry(key)
                .or_insert_with(|| AgentWorkerGroupSummary {
                    worker_kind: record.worker_kind,
                    worker_name: record.worker_name.clone(),
                    canary_label: record.canary_label.clone(),
                    total_runs: 0,
                    accepted_candidates: 0,
                    generated_candidates: 0,
                });
            group.total_runs = group.total_runs.saturating_add(1);
            group.accepted_candidates = group
                .accepted_candidates
                .saturating_add(record.accepted_candidates);
            group.generated_candidates = group
                .generated_candidates
                .saturating_add(record.generated_candidates);
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::contract::agent_provider::CairnVerb;

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
            summary
                .failure_modes
                .get(&AgentWorkerFailureMode::BudgetExceeded),
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
            record
                .scope
                .as_ref()
                .and_then(|scope| scope.agent.as_deref()),
            Some("agt:cairn-extractor:v1")
        );
        assert_eq!(
            record.failure_mode,
            Some(AgentWorkerFailureMode::ProviderUnavailable)
        );
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
