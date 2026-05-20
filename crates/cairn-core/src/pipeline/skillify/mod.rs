//! Skillify pipeline primitives (brief sections 5.0.b and 11.b).
//!
//! Pure data and validation only. Workflow and CLI crates perform I/O.

pub mod candidate;

pub use candidate::{
    SkillifyCandidate, SkillifyCandidateInput, SkillifyCandidateReject, SkillifyOutcome,
    SkillifySource, SkillifyStatus, SkillifyTrigger,
};
