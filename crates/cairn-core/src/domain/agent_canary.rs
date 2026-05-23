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
    /// Do not dispatch until an operator resets the rollout.
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
    /// Stable operator-facing cohort label, such as `canary-05`.
    pub cohort_label: String,
    /// Percentage of traffic allowed in canary state, from 0 through 100.
    pub rollout_percent: u8,
    /// Minimum observed runs before judging canary metrics.
    pub min_runs: u64,
    /// Minimum accepted/generated rate required for promotion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_acceptance_rate: Option<f64>,
    /// Maximum failed/total rate allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_failure_rate: Option<f64>,
    /// Maximum aggregate provider-defined cost units allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_units: Option<u64>,
    /// Maximum aggregate agent tool calls allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_calls: Option<u64>,
    /// Maximum aggregate agent turns allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u64>,
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

/// Stable body-free reason code for a canary decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCanaryReasonCode {
    /// Dispatch is allowed by the current state.
    DispatchAllowed,
    /// Operator requested pause.
    OperatorPaused,
    /// Rollout state is paused.
    RolloutPaused,
    /// Operator requested rollback.
    OperatorRollbackRequested,
    /// Rollout state is rolled back.
    RolloutRolledBack,
    /// Request cohort is outside the current canary sample.
    OutsideCanaryCohort,
    /// Not enough audit runs have accumulated to judge canary health.
    InsufficientAuditRuns,
    /// Failure rate exceeded the configured threshold.
    FailureRateExceeded,
    /// Acceptance rate fell below the configured threshold.
    AcceptanceRateTooLow,
    /// Aggregate cost units exceeded the configured cap.
    CostUnitsExceeded,
    /// Aggregate tool calls exceeded the configured cap.
    ToolCallsExceeded,
    /// Aggregate turns exceeded the configured cap.
    TurnsExceeded,
    /// Cost per accepted candidate exceeded the configured cap.
    CostPerAcceptedCandidateExceeded,
    /// Canary metrics passed configured thresholds.
    ThresholdsPassed,
    /// Aggregate evaluation observed a non-canary state.
    NonCanaryState,
}

impl AgentCanaryReasonCode {
    /// Stable snake-case label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DispatchAllowed => "dispatch_allowed",
            Self::OperatorPaused => "operator_paused",
            Self::RolloutPaused => "rollout_paused",
            Self::OperatorRollbackRequested => "operator_rollback_requested",
            Self::RolloutRolledBack => "rollout_rolled_back",
            Self::OutsideCanaryCohort => "outside_canary_cohort",
            Self::InsufficientAuditRuns => "insufficient_audit_runs",
            Self::FailureRateExceeded => "failure_rate_exceeded",
            Self::AcceptanceRateTooLow => "acceptance_rate_too_low",
            Self::CostUnitsExceeded => "cost_units_exceeded",
            Self::ToolCallsExceeded => "tool_calls_exceeded",
            Self::TurnsExceeded => "turns_exceeded",
            Self::CostPerAcceptedCandidateExceeded => "cost_per_accepted_candidate_exceeded",
            Self::ThresholdsPassed => "thresholds_passed",
            Self::NonCanaryState => "non_canary_state",
        }
    }
}

/// Body-free audit counters attached to aggregate canary decisions.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCanaryDecisionCounters {
    /// Total observed worker runs.
    pub total_runs: u64,
    /// Runs counted as failed by canary gates.
    pub failed_runs: u64,
    /// Generated candidates across observed worker runs.
    pub generated_candidates: u64,
    /// Accepted candidates across observed worker runs.
    pub accepted_candidates: u64,
    /// Agent turns consumed.
    pub turns: u64,
    /// Agent tool calls consumed.
    pub tool_calls: u64,
    /// Provider-defined cost units consumed.
    pub cost_units: u64,
    /// `failed_runs / total_runs`, absent without observed runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_rate: Option<f64>,
    /// `accepted_candidates / generated_candidates`, absent without generated candidates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_rate: Option<f64>,
    /// Ceil-rounded cost units per accepted candidate, absent without accepted candidates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_units_per_accepted_candidate: Option<u64>,
}

impl AgentCanaryDecisionCounters {
    /// Empty counters for dispatch decisions that do not evaluate an audit summary.
    pub const EMPTY: Self = Self {
        total_runs: 0,
        failed_runs: 0,
        generated_candidates: 0,
        accepted_candidates: 0,
        turns: 0,
        tool_calls: 0,
        cost_units: 0,
        failure_rate: None,
        acceptance_rate: None,
        cost_units_per_accepted_candidate: None,
    };

    /// Build counters from a body-free worker audit summary.
    #[must_use]
    pub fn from_summary(summary: &AgentWorkerAuditSummary) -> Self {
        Self {
            total_runs: summary.total_runs,
            failed_runs: summary.failed_runs,
            generated_candidates: summary.generated_candidates,
            accepted_candidates: summary.accepted_candidates,
            turns: summary.turns,
            tool_calls: summary.tool_calls,
            cost_units: summary.cost_units,
            failure_rate: rate(summary.failed_runs, summary.total_runs),
            acceptance_rate: rate(summary.accepted_candidates, summary.generated_candidates),
            cost_units_per_accepted_candidate: cost_per_accepted_candidate(
                summary.cost_units,
                summary.accepted_candidates,
            ),
        }
    }
}

impl Default for AgentCanaryDecisionCounters {
    fn default() -> Self {
        Self::EMPTY
    }
}

/// Pure canary decision with state, reason code, and report counters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCanaryDecision {
    /// Stable decision kind.
    pub kind: AgentCanaryDecisionKind,
    /// State that should be used after applying the decision.
    pub next_state: AgentCanaryState,
    /// Stable body-free reason code.
    pub reason_code: AgentCanaryReasonCode,
    /// Cohort label copied from the policy for operator reports.
    pub cohort_label: String,
    /// Rollout sample percentage copied from the policy.
    pub rollout_percent: u8,
    /// Body-free audit counters used for the decision.
    pub counters: AgentCanaryDecisionCounters,
}

/// Validation failures for impossible canary policies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AgentCanaryError {
    /// Cohort label must not be empty.
    #[error("agent canary cohort_label must not be empty")]
    EmptyCohortLabel,
    /// `rollout_percent` must be from 0 through 100.
    #[error("agent canary rollout_percent must be between 0 and 100")]
    RolloutPercentOutOfRange,
    /// `cohort_percentile` must be from 0 through 99.
    #[error("agent canary cohort_percentile {cohort_percentile} must be between 0 and 99")]
    CohortPercentileOutOfRange {
        /// Invalid cohort percentile.
        cohort_percentile: u8,
    },
    /// `min_runs` must be nonzero.
    #[error("agent canary min_runs must be nonzero")]
    ZeroMinRuns,
    /// A populated audit summary was not pre-filtered to one canary cohort.
    #[error("agent canary summary must be pre-filtered to the policy cohort")]
    MixedCohortSummary,
    /// A rate field was outside 0.0 through 1.0.
    #[error("agent canary rate {field} must be between 0.0 and 1.0")]
    RateOutOfRange {
        /// Field name.
        field: &'static str,
    },
}

impl AgentCanaryPolicy {
    /// Validate policy invariants.
    ///
    /// # Errors
    /// Returns [`AgentCanaryError`] when the policy is impossible to evaluate.
    pub fn validate(&self) -> Result<(), AgentCanaryError> {
        if self.cohort_label.trim().is_empty() {
            return Err(AgentCanaryError::EmptyCohortLabel);
        }
        if self.rollout_percent > 100 {
            return Err(AgentCanaryError::RolloutPercentOutOfRange);
        }
        if self.min_runs == 0 {
            return Err(AgentCanaryError::ZeroMinRuns);
        }
        validate_rate("min_acceptance_rate", self.min_acceptance_rate)?;
        validate_rate("max_failure_rate", self.max_failure_rate)?;
        Ok(())
    }

    /// Decide whether one request in `cohort_percentile` may dispatch.
    ///
    /// `cohort_percentile` is expected to be a deterministic value from 0 through 99
    /// computed by the caller from body-free request identity.
    ///
    /// # Errors
    /// Returns [`AgentCanaryError`] when the policy or cohort percentile is invalid.
    pub fn dispatch_decision(
        &self,
        cohort_percentile: u8,
    ) -> Result<AgentCanaryDecision, AgentCanaryError> {
        self.validate()?;
        if cohort_percentile >= 100 {
            return Err(AgentCanaryError::CohortPercentileOutOfRange { cohort_percentile });
        }
        if self.rollback_requested {
            return Ok(self.decision(
                AgentCanaryDecisionKind::RollbackRequested,
                AgentCanaryState::RolledBack,
                AgentCanaryReasonCode::OperatorRollbackRequested,
                AgentCanaryDecisionCounters::EMPTY,
            ));
        }
        if self.pause_requested {
            return Ok(self.decision(
                AgentCanaryDecisionKind::DispatchDeniedPaused,
                AgentCanaryState::Paused,
                AgentCanaryReasonCode::OperatorPaused,
                AgentCanaryDecisionCounters::EMPTY,
            ));
        }
        if self.state == AgentCanaryState::Paused {
            return Ok(self.decision(
                AgentCanaryDecisionKind::DispatchDeniedPaused,
                AgentCanaryState::Paused,
                AgentCanaryReasonCode::RolloutPaused,
                AgentCanaryDecisionCounters::EMPTY,
            ));
        }
        if self.state == AgentCanaryState::RolledBack {
            return Ok(self.decision(
                AgentCanaryDecisionKind::DispatchDeniedRolledBack,
                AgentCanaryState::RolledBack,
                AgentCanaryReasonCode::RolloutRolledBack,
                AgentCanaryDecisionCounters::EMPTY,
            ));
        }
        if self.state == AgentCanaryState::Canary && cohort_percentile >= self.rollout_percent {
            return Ok(self.decision(
                AgentCanaryDecisionKind::DispatchDeniedOutsideCanary,
                AgentCanaryState::Canary,
                AgentCanaryReasonCode::OutsideCanaryCohort,
                AgentCanaryDecisionCounters::EMPTY,
            ));
        }
        Ok(self.decision(
            AgentCanaryDecisionKind::DispatchAllowed,
            self.state,
            AgentCanaryReasonCode::DispatchAllowed,
            AgentCanaryDecisionCounters::EMPTY,
        ))
    }

    /// Evaluate aggregate canary metrics after worker audit records are summarized.
    ///
    /// Canary-state callers must pass a summary already filtered to
    /// `self.cohort_label`. Empty worker-group metadata is accepted only for
    /// true no-data summaries. When runs are present during canary evaluation,
    /// every worker group must carry the policy cohort label so mixed-cohort
    /// aggregate misuse fails closed. Explicit operator pause/rollback requests
    /// and non-canary states are evaluated before this cohort check.
    ///
    /// # Errors
    /// Returns [`AgentCanaryError`] when the policy is invalid or the summary
    /// contains mixed or unlabeled worker cohorts.
    pub fn evaluate_summary(
        &self,
        summary: &AgentWorkerAuditSummary,
    ) -> Result<AgentCanaryDecision, AgentCanaryError> {
        self.validate()?;
        let counters = AgentCanaryDecisionCounters::from_summary(summary);
        if let Some(decision) = self.gated_summary_decision(counters) {
            return Ok(decision);
        }
        if self.state != AgentCanaryState::Canary {
            return Ok(self.decision(
                AgentCanaryDecisionKind::DispatchAllowed,
                self.state,
                AgentCanaryReasonCode::NonCanaryState,
                counters,
            ));
        }
        self.validate_summary_cohort(summary)?;
        if summary.total_runs < self.min_runs {
            return Ok(self.decision(
                AgentCanaryDecisionKind::RemainCanaryInsufficientData,
                AgentCanaryState::Canary,
                AgentCanaryReasonCode::InsufficientAuditRuns,
                counters,
            ));
        }
        if let Some(reason_code) = self.threshold_failure(summary, counters) {
            return Ok(self.decision(
                AgentCanaryDecisionKind::RollbackThresholdFailed,
                AgentCanaryState::RolledBack,
                reason_code,
                counters,
            ));
        }
        Ok(self.decision(
            AgentCanaryDecisionKind::PromoteToEnabled,
            AgentCanaryState::Enabled,
            AgentCanaryReasonCode::ThresholdsPassed,
            counters,
        ))
    }

    fn validate_summary_cohort(
        &self,
        summary: &AgentWorkerAuditSummary,
    ) -> Result<(), AgentCanaryError> {
        if summary.workers.is_empty() {
            return if summary.total_runs == 0 {
                Ok(())
            } else {
                Err(AgentCanaryError::MixedCohortSummary)
            };
        }

        if summary
            .workers
            .iter()
            .all(|worker| worker.canary_label.as_deref() == Some(self.cohort_label.as_str()))
        {
            return Ok(());
        }

        Err(AgentCanaryError::MixedCohortSummary)
    }

    fn gated_summary_decision(
        &self,
        counters: AgentCanaryDecisionCounters,
    ) -> Option<AgentCanaryDecision> {
        if self.rollback_requested {
            return Some(self.decision(
                AgentCanaryDecisionKind::RollbackRequested,
                AgentCanaryState::RolledBack,
                AgentCanaryReasonCode::OperatorRollbackRequested,
                counters,
            ));
        }
        if self.pause_requested {
            return Some(self.decision(
                AgentCanaryDecisionKind::DispatchDeniedPaused,
                AgentCanaryState::Paused,
                AgentCanaryReasonCode::OperatorPaused,
                counters,
            ));
        }
        match self.state {
            AgentCanaryState::Paused => Some(self.decision(
                AgentCanaryDecisionKind::DispatchDeniedPaused,
                AgentCanaryState::Paused,
                AgentCanaryReasonCode::RolloutPaused,
                counters,
            )),
            AgentCanaryState::RolledBack => Some(self.decision(
                AgentCanaryDecisionKind::DispatchDeniedRolledBack,
                AgentCanaryState::RolledBack,
                AgentCanaryReasonCode::RolloutRolledBack,
                counters,
            )),
            AgentCanaryState::Canary | AgentCanaryState::Enabled => None,
        }
    }

    fn threshold_failure(
        &self,
        summary: &AgentWorkerAuditSummary,
        counters: AgentCanaryDecisionCounters,
    ) -> Option<AgentCanaryReasonCode> {
        if self
            .max_failure_rate
            .is_some_and(|max| counters.failure_rate.is_some_and(|rate| rate > max))
        {
            return Some(AgentCanaryReasonCode::FailureRateExceeded);
        }
        if self
            .min_acceptance_rate
            .is_some_and(|min| counters.acceptance_rate.unwrap_or(0.0) < min)
        {
            return Some(AgentCanaryReasonCode::AcceptanceRateTooLow);
        }
        if self
            .max_cost_units
            .is_some_and(|max| summary.cost_units > max)
        {
            return Some(AgentCanaryReasonCode::CostUnitsExceeded);
        }
        if self
            .max_tool_calls
            .is_some_and(|max| summary.tool_calls > max)
        {
            return Some(AgentCanaryReasonCode::ToolCallsExceeded);
        }
        if self.max_turns.is_some_and(|max| summary.turns > max) {
            return Some(AgentCanaryReasonCode::TurnsExceeded);
        }
        if self
            .max_cost_units_per_accepted_candidate
            .is_some_and(|max| {
                exceeds_cost_per_accepted_candidate(
                    summary.cost_units,
                    summary.accepted_candidates,
                    max,
                )
            })
        {
            return Some(AgentCanaryReasonCode::CostPerAcceptedCandidateExceeded);
        }
        None
    }

    fn decision(
        &self,
        kind: AgentCanaryDecisionKind,
        next_state: AgentCanaryState,
        reason_code: AgentCanaryReasonCode,
        counters: AgentCanaryDecisionCounters,
    ) -> AgentCanaryDecision {
        AgentCanaryDecision {
            kind,
            next_state,
            reason_code,
            cohort_label: self.cohort_label.clone(),
            rollout_percent: self.rollout_percent,
            counters,
        }
    }
}

fn validate_rate(field: &'static str, value: Option<f64>) -> Result<(), AgentCanaryError> {
    if value.is_some_and(|rate| !(0.0..=1.0).contains(&rate)) {
        return Err(AgentCanaryError::RateOutOfRange { field });
    }
    Ok(())
}

fn rate(numerator: u64, denominator: u64) -> Option<f64> {
    if denominator == 0 {
        return None;
    }

    // Operator-facing ratio; exact integer counters remain attached to the decision.
    #[allow(clippy::cast_precision_loss)]
    Some(numerator as f64 / denominator as f64)
}

fn cost_per_accepted_candidate(cost_units: u64, accepted_candidates: u64) -> Option<u64> {
    if accepted_candidates == 0 {
        return None;
    }
    Some(cost_units.div_ceil(accepted_candidates))
}

fn exceeds_cost_per_accepted_candidate(
    cost_units: u64,
    accepted_candidates: u64,
    max_cost_units_per_accepted_candidate: u64,
) -> bool {
    if accepted_candidates == 0 {
        return cost_units > 0;
    }

    u128::from(cost_units)
        > u128::from(max_cost_units_per_accepted_candidate) * u128::from(accepted_candidates)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::domain::{AgentWorkerGroupSummary, AgentWorkerKind};

    fn summary(
        total: u64,
        failed: u64,
        generated: u64,
        accepted: u64,
        cost_units: u64,
    ) -> AgentWorkerAuditSummary {
        AgentWorkerAuditSummary {
            total_runs: total,
            completed_runs: total.saturating_sub(failed),
            failed_runs: failed,
            generated_candidates: generated,
            accepted_candidates: accepted,
            acceptance_rate: rate(accepted, generated),
            turns: 2,
            tool_calls: 3,
            cost_units,
            failure_modes: Default::default(),
            workers: if total == 0 {
                Vec::new()
            } else {
                vec![group(Some("canary-05"), total)]
            },
        }
    }

    fn policy() -> AgentCanaryPolicy {
        AgentCanaryPolicy {
            state: AgentCanaryState::Canary,
            cohort_label: "canary-05".to_owned(),
            rollout_percent: 5,
            min_runs: 4,
            min_acceptance_rate: Some(0.50),
            max_failure_rate: Some(0.20),
            max_cost_units: Some(100),
            max_tool_calls: None,
            max_turns: None,
            max_cost_units_per_accepted_candidate: Some(20),
            pause_requested: false,
            rollback_requested: false,
        }
    }

    fn group(canary_label: Option<&str>, total_runs: u64) -> AgentWorkerGroupSummary {
        AgentWorkerGroupSummary {
            worker_kind: AgentWorkerKind::Extractor,
            worker_name: "agent_extractor".to_owned(),
            canary_label: canary_label.map(str::to_owned),
            total_runs,
            completed_runs: total_runs,
            failed_runs: 0,
            generated_candidates: total_runs,
            accepted_candidates: total_runs,
            acceptance_rate: rate(total_runs, total_runs),
            turns: total_runs,
            tool_calls: total_runs,
            cost_units: total_runs,
            failure_modes: Default::default(),
        }
    }

    #[test]
    fn enabled_summary_bypasses_canary_cohort_validation() {
        let mut summary = summary(5, 0, 10, 8, 80);
        summary.workers = vec![group(Some("canary-05"), 3), group(None, 2)];
        let policy = AgentCanaryPolicy {
            state: AgentCanaryState::Enabled,
            ..policy()
        };

        let decision = policy
            .evaluate_summary(&summary)
            .expect("enabled rollouts accept mixed historical summaries");

        assert_eq!(decision.kind, AgentCanaryDecisionKind::DispatchAllowed);
        assert_eq!(decision.next_state, AgentCanaryState::Enabled);
        assert_eq!(decision.reason_code, AgentCanaryReasonCode::NonCanaryState);
    }

    #[test]
    fn paused_rollout_denies_dispatch_with_stable_reason_code() {
        let policy = AgentCanaryPolicy {
            state: AgentCanaryState::Paused,
            pause_requested: true,
            ..policy()
        };

        let decision = policy.dispatch_decision(0).expect("valid policy");

        assert_eq!(decision.kind, AgentCanaryDecisionKind::DispatchDeniedPaused);
        assert_eq!(decision.next_state, AgentCanaryState::Paused);
        assert_eq!(decision.reason_code, AgentCanaryReasonCode::OperatorPaused);
        assert_eq!(decision.cohort_label, "canary-05");
    }

    #[test]
    fn canary_rollout_denies_outside_cohort() {
        let decision = policy().dispatch_decision(50).expect("valid policy");

        assert_eq!(
            decision.kind,
            AgentCanaryDecisionKind::DispatchDeniedOutsideCanary
        );
        assert_eq!(
            decision.reason_code,
            AgentCanaryReasonCode::OutsideCanaryCohort
        );
        assert_eq!(decision.next_state, AgentCanaryState::Canary);
    }

    #[test]
    fn canary_allows_inside_cohort() {
        let decision = policy().dispatch_decision(4).expect("valid policy");

        assert_eq!(decision.kind, AgentCanaryDecisionKind::DispatchAllowed);
        assert_eq!(decision.reason_code, AgentCanaryReasonCode::DispatchAllowed);
        assert_eq!(decision.next_state, AgentCanaryState::Canary);
    }

    #[test]
    fn canary_remains_when_insufficient_runs() {
        let decision = policy()
            .evaluate_summary(&summary(3, 0, 4, 4, 20))
            .expect("valid policy");

        assert_eq!(
            decision.kind,
            AgentCanaryDecisionKind::RemainCanaryInsufficientData
        );
        assert_eq!(
            decision.reason_code,
            AgentCanaryReasonCode::InsufficientAuditRuns
        );
        assert_eq!(decision.next_state, AgentCanaryState::Canary);
        assert_eq!(decision.counters.total_runs, 3);
    }

    #[test]
    fn canary_rolls_back_when_failure_rate_exceeds_threshold() {
        let decision = policy()
            .evaluate_summary(&summary(5, 2, 4, 4, 20))
            .expect("valid policy");

        assert_eq!(
            decision.kind,
            AgentCanaryDecisionKind::RollbackThresholdFailed
        );
        assert_eq!(
            decision.reason_code,
            AgentCanaryReasonCode::FailureRateExceeded
        );
        assert_eq!(decision.next_state, AgentCanaryState::RolledBack);
        assert_eq!(decision.counters.failed_runs, 2);
        assert_eq!(decision.counters.failure_rate, Some(0.4));
    }

    #[test]
    fn canary_recomputes_acceptance_rate_from_counts() {
        let mut summary = summary(5, 0, 10, 1, 10);
        summary.acceptance_rate = Some(1.0);

        let decision = policy().evaluate_summary(&summary).expect("valid policy");

        assert_eq!(
            decision.kind,
            AgentCanaryDecisionKind::RollbackThresholdFailed
        );
        assert_eq!(
            decision.reason_code,
            AgentCanaryReasonCode::AcceptanceRateTooLow
        );
        assert_eq!(decision.counters.acceptance_rate, Some(0.1));
    }

    #[test]
    fn mixed_cohort_summary_is_rejected_before_thresholds() {
        let mut summary = summary(5, 0, 10, 8, 80);
        summary.workers = vec![group(Some("canary-05"), 3), group(Some("canary-10"), 2)];

        let error = policy()
            .evaluate_summary(&summary)
            .expect_err("mixed cohort summaries must be rejected");

        assert_eq!(error, AgentCanaryError::MixedCohortSummary);
    }

    #[test]
    fn operator_gates_win_over_mixed_cohort_validation() {
        let mut summary = summary(5, 0, 10, 8, 80);
        summary.workers = vec![group(Some("canary-05"), 3), group(Some("canary-10"), 2)];

        let rollback = AgentCanaryPolicy {
            rollback_requested: true,
            ..policy()
        };
        let rollback_decision = rollback
            .evaluate_summary(&summary)
            .expect("operator rollback must bypass cohort validation");
        assert_eq!(
            rollback_decision.kind,
            AgentCanaryDecisionKind::RollbackRequested
        );
        assert_eq!(
            rollback_decision.reason_code,
            AgentCanaryReasonCode::OperatorRollbackRequested
        );

        let pause = AgentCanaryPolicy {
            pause_requested: true,
            ..policy()
        };
        let pause_decision = pause
            .evaluate_summary(&summary)
            .expect("operator pause must bypass cohort validation");
        assert_eq!(
            pause_decision.kind,
            AgentCanaryDecisionKind::DispatchDeniedPaused
        );
        assert_eq!(
            pause_decision.reason_code,
            AgentCanaryReasonCode::OperatorPaused
        );
    }

    #[test]
    fn non_empty_summary_without_worker_metadata_is_rejected() {
        let mut summary = summary(5, 0, 10, 8, 80);
        summary.workers = Vec::new();

        let error = policy()
            .evaluate_summary(&summary)
            .expect_err("non-empty summaries must include cohort metadata");

        assert_eq!(error, AgentCanaryError::MixedCohortSummary);
    }

    #[test]
    fn canary_rolls_back_when_budget_cap_is_exceeded() {
        let policy = AgentCanaryPolicy {
            max_cost_units: Some(30),
            max_cost_units_per_accepted_candidate: None,
            ..policy()
        };

        let decision = policy
            .evaluate_summary(&summary(5, 0, 10, 8, 80))
            .expect("valid policy");

        assert_eq!(
            decision.kind,
            AgentCanaryDecisionKind::RollbackThresholdFailed
        );
        assert_eq!(
            decision.reason_code,
            AgentCanaryReasonCode::CostUnitsExceeded
        );
        assert_eq!(decision.counters.cost_units, 80);
    }

    #[test]
    fn canary_rolls_back_when_cost_per_accepted_candidate_exceeds_threshold() {
        let policy = AgentCanaryPolicy {
            max_cost_units: None,
            max_cost_units_per_accepted_candidate: Some(10),
            ..policy()
        };

        let decision = policy
            .evaluate_summary(&summary(5, 0, 10, 8, 81))
            .expect("valid policy");

        assert_eq!(
            decision.kind,
            AgentCanaryDecisionKind::RollbackThresholdFailed
        );
        assert_eq!(
            decision.reason_code,
            AgentCanaryReasonCode::CostPerAcceptedCandidateExceeded
        );
        assert_eq!(
            decision.counters.cost_units_per_accepted_candidate,
            Some(11)
        );
    }

    #[test]
    fn zero_cost_with_zero_accepted_candidates_does_not_exceed_cost_ratio() {
        let policy = AgentCanaryPolicy {
            min_acceptance_rate: None,
            max_cost_units: None,
            max_cost_units_per_accepted_candidate: Some(10),
            ..policy()
        };

        let decision = policy
            .evaluate_summary(&summary(5, 0, 0, 0, 0))
            .expect("valid policy");

        assert_eq!(decision.kind, AgentCanaryDecisionKind::PromoteToEnabled);
        assert_eq!(decision.counters.cost_units_per_accepted_candidate, None);
    }

    #[test]
    fn positive_cost_with_zero_accepted_candidates_exceeds_cost_ratio() {
        let policy = AgentCanaryPolicy {
            min_acceptance_rate: None,
            max_cost_units: None,
            max_cost_units_per_accepted_candidate: Some(10),
            ..policy()
        };

        let decision = policy
            .evaluate_summary(&summary(5, 0, 0, 0, 1))
            .expect("valid policy");

        assert_eq!(
            decision.kind,
            AgentCanaryDecisionKind::RollbackThresholdFailed
        );
        assert_eq!(
            decision.reason_code,
            AgentCanaryReasonCode::CostPerAcceptedCandidateExceeded
        );
        assert_eq!(decision.counters.cost_units_per_accepted_candidate, None);
    }

    #[test]
    fn canary_promotes_when_thresholds_pass() {
        let decision = policy()
            .evaluate_summary(&summary(5, 0, 10, 8, 80))
            .expect("valid policy");

        assert_eq!(decision.kind, AgentCanaryDecisionKind::PromoteToEnabled);
        assert_eq!(
            decision.reason_code,
            AgentCanaryReasonCode::ThresholdsPassed
        );
        assert_eq!(decision.next_state, AgentCanaryState::Enabled);
        assert_eq!(decision.counters.acceptance_rate, Some(0.8));
    }

    #[test]
    fn explicit_rollback_wins_over_promotion() {
        let policy = AgentCanaryPolicy {
            rollback_requested: true,
            ..policy()
        };

        let decision = policy
            .evaluate_summary(&summary(5, 0, 10, 10, 10))
            .expect("valid policy");

        assert_eq!(decision.kind, AgentCanaryDecisionKind::RollbackRequested);
        assert_eq!(
            decision.reason_code,
            AgentCanaryReasonCode::OperatorRollbackRequested
        );
        assert_eq!(decision.next_state, AgentCanaryState::RolledBack);
        assert_eq!(decision.counters.total_runs, 5);
    }

    fn rate(numerator: u64, denominator: u64) -> Option<f64> {
        if denominator == 0 {
            return None;
        }

        #[allow(clippy::cast_precision_loss)]
        Some(numerator as f64 / denominator as f64)
    }
}
