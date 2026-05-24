//! Scheduler handler for evolution proposal decisions.

use std::path::PathBuf;

use cairn_core::contract::job_store::{FailureClass, JobKind, JobPayload};
use cairn_core::pipeline::evolution::{
    EvolutionGateKind, EvolutionGateReport, EvolutionRun, EvolutionTransitionError,
};

use crate::scheduler::{HandlerOutcome, JobHandler};

use super::materialize::{
    EvolutionMaterializeError, MaterializedEvolutionDecision, materialize_run,
};
use super::payload::EvolutionPayload;

/// The `JobKind` discriminator stored in `workflow_jobs.kind`.
pub const EVOLUTION_KIND: &str = "evolution.evolve";

/// File-backed evolution state-machine handler.
pub struct EvolutionHandler {
    vault_root: PathBuf,
}

/// Error from one decoded evolution handler run.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EvolutionRunError {
    /// Core state transition rejected the payload.
    #[error(transparent)]
    Transition(#[from] EvolutionTransitionError),
    /// Persistence failed.
    #[error(transparent)]
    Materialize(#[from] EvolutionMaterializeError),
}

impl EvolutionRunError {
    fn is_permanent(&self) -> bool {
        matches!(self, Self::Transition(_))
            || matches!(
                self,
                Self::Materialize(
                    EvolutionMaterializeError::InvalidProposalId { .. }
                        | EvolutionMaterializeError::NonTerminal { .. }
                        | EvolutionMaterializeError::Json(_)
                )
            )
    }
}

impl EvolutionHandler {
    /// Construct a handler rooted at a vault directory.
    #[must_use]
    pub const fn new(vault_root: PathBuf) -> Self {
        Self { vault_root }
    }

    /// Run one decoded evolution proposal.
    ///
    /// # Errors
    /// Returns when the payload violates the evolution state machine or the
    /// terminal audit files cannot be written.
    pub fn run_once(
        &self,
        payload: EvolutionPayload,
    ) -> Result<MaterializedEvolutionDecision, EvolutionRunError> {
        let mut run = EvolutionRun::new(
            payload.proposal_id,
            payload.previous_artifact,
            payload.candidate_artifact,
            payload.rollback_plan,
        )?;
        run.extend_gates(EvolutionGateReport {
            gates: payload.gates,
        });

        let (missing, failed) = run.promotion_blockers();
        let pre_canary_blocked = missing
            .iter()
            .chain(failed.iter())
            .any(|gate| *gate != EvolutionGateKind::Canary);
        if pre_canary_blocked {
            run.reject(&payload.decision_ref)?;
            return Ok(materialize_run(&self.vault_root, &run)?);
        }

        if let Some(canary_ref) = payload.canary_ref.as_deref() {
            run.start_canary(canary_ref)?;
        }
        if let Some(failure_ref) = payload.canary_failure_ref.as_deref() {
            run.fail_canary(failure_ref)?;
            return Ok(materialize_run(&self.vault_root, &run)?);
        }

        let (missing, failed) = run.promotion_blockers();
        if !missing.is_empty() || !failed.is_empty() {
            run.reject(&payload.decision_ref)?;
            return Ok(materialize_run(&self.vault_root, &run)?);
        }

        let reviewer = payload.reviewer.as_deref().unwrap_or("autonomous");
        run.promote(reviewer, &payload.decision_ref)?;
        Ok(materialize_run(&self.vault_root, &run)?)
    }
}

#[async_trait::async_trait]
impl JobHandler for EvolutionHandler {
    fn kind(&self) -> JobKind {
        JobKind::new(EVOLUTION_KIND)
    }

    async fn handle(&self, payload: &JobPayload) -> HandlerOutcome {
        let payload = match EvolutionPayload::from_bytes(payload) {
            Ok(payload) => payload,
            Err(e) => {
                return HandlerOutcome::Permanent {
                    class: FailureClass::Validation,
                    reason: format!("invalid evolution payload: {e}"),
                };
            }
        };

        match self.run_once(payload) {
            Ok(_) => HandlerOutcome::Done,
            Err(e) if e.is_permanent() => HandlerOutcome::Permanent {
                class: FailureClass::Validation,
                reason: e.to_string(),
            },
            Err(e) => HandlerOutcome::transient_retry(e.to_string()),
        }
    }
}
