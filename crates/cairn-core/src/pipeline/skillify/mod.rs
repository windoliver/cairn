//! Skillify pipeline primitives (brief sections 5.0.b and 11.b).
//!
//! Pure data and validation only. Workflow and CLI crates perform I/O.

pub mod artifact;
pub mod candidate;
pub mod gate;
pub mod lint;
pub mod pack;
pub mod spec;
pub mod stage;

pub use artifact::{SkillArtifact, SkillArtifactBundle, SkillArtifactError, SkillArtifactKind};
pub use candidate::{
    SkillifyCandidate, SkillifyCandidateInput, SkillifyCandidateReject, SkillifyOutcome,
    SkillifySource, SkillifyStatus, SkillifyTrigger,
};
pub use gate::{SkillifyGate, SkillifyGateReport, SkillifyGateStatus};
pub use lint::{
    SkillLintIssue, SkillLintIssueKind, SkillLintSkill, SkillLintSnapshot, lint_skill_snapshot,
};
pub use pack::{SkillPackEntry, SkillPackError, SkillPackManifest};
pub use spec::{SkillSpecDraft, SkillSpecError};
pub use stage::{SkillifyPipelineState, SkillifyStage, SkillifyStageError};
