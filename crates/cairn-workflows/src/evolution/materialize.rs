//! File-backed audit materialization for `EvolutionWorkflow`.

use std::fs;
use std::path::{Component, Path, PathBuf};

use cairn_core::pipeline::evolution::{EvolutionArtifactRef, EvolutionRun, EvolutionState};
use serde::Serialize;

/// Materialized terminal decision for one proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterializedEvolutionDecision {
    /// Candidate promoted.
    Promoted,
    /// Candidate rolled back after canary failure.
    RolledBack,
    /// Candidate rejected before promotion.
    Rejected,
}

impl MaterializedEvolutionDecision {
    /// Map an evolution state to its terminal materialized decision.
    #[must_use]
    pub const fn from_state(state: EvolutionState) -> Option<Self> {
        match state {
            EvolutionState::Promoted => Some(Self::Promoted),
            EvolutionState::RolledBack => Some(Self::RolledBack),
            EvolutionState::Rejected => Some(Self::Rejected),
            EvolutionState::Proposed | EvolutionState::Evaluating | EvolutionState::Canarying => {
                None
            }
        }
    }
}

/// Persistence error for `.cairn/evolution/evolve`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EvolutionMaterializeError {
    /// Proposal id cannot be safely used as a path segment.
    #[error("invalid proposal id `{value}`")]
    InvalidProposalId {
        /// Rejected value.
        value: String,
    },
    /// JSON serialization failed.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Filesystem operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Tried to persist a non-terminal state.
    #[error("evolution state {state:?} is not terminal")]
    NonTerminal {
        /// State that was not terminal.
        state: EvolutionState,
    },
}

/// Persist the terminal state, gate report, rollback plan, and lineage.
///
/// # Errors
/// Returns when the proposal id is unsafe, the state is non-terminal, or JSON
/// or filesystem writes fail.
pub fn materialize_run(
    vault_root: &Path,
    run: &EvolutionRun,
) -> Result<MaterializedEvolutionDecision, EvolutionMaterializeError> {
    let decision = MaterializedEvolutionDecision::from_state(run.state())
        .ok_or(EvolutionMaterializeError::NonTerminal { state: run.state() })?;
    validate_path_token(run.proposal_id())?;

    let root = vault_root
        .join(".cairn/evolution/evolve")
        .join(run.proposal_id());
    fs::create_dir_all(&root)?;

    write_pretty(root.join("state.json"), &EvolutionStateSnapshot::from(run))?;
    write_pretty(root.join("gate-report.json"), run.gate_report())?;
    write_pretty(root.join("rollback-plan.json"), run.rollback_plan())?;
    write_pretty(root.join("lineage.json"), &run.lineage())?;

    Ok(decision)
}

#[derive(Serialize)]
struct EvolutionStateSnapshot<'a> {
    proposal_id: &'a str,
    state: EvolutionState,
    active_artifact: &'a EvolutionArtifactRef,
    promoted_artifact: Option<&'a EvolutionArtifactRef>,
    decision_evidence: &'a [String],
}

impl<'a> From<&'a EvolutionRun> for EvolutionStateSnapshot<'a> {
    fn from(run: &'a EvolutionRun) -> Self {
        Self {
            proposal_id: run.proposal_id(),
            state: run.state(),
            active_artifact: run.active_artifact(),
            promoted_artifact: run.promoted_artifact(),
            decision_evidence: run.decision_evidence(),
        }
    }
}

fn write_pretty<T: Serialize>(path: PathBuf, value: &T) -> Result<(), EvolutionMaterializeError> {
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

fn validate_path_token(value: &str) -> Result<(), EvolutionMaterializeError> {
    let path = Path::new(value);
    let components = path.components().collect::<Vec<_>>();
    if value.trim().is_empty()
        || path.is_absolute()
        || components.len() != 1
        || !components
            .iter()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(EvolutionMaterializeError::InvalidProposalId {
            value: value.to_owned(),
        });
    }
    Ok(())
}
