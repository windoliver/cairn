//! `assemble_hot` verb: hot-memory recipe assembly with cache-breakpoint
//! segment markers. Brief §5, §7, §8.0.f. Issue #288.

pub mod assembler;
pub mod raw;
pub mod segments;

pub use assembler::{assemble_hot, AssembleHotError};
pub use segments::{
    build_segments, default_stability, validate, validate_base, validate_segments,
    validate_with_recipe, AssembleHotValidationError, MAX_SEGMENTS,
};

use crate::config::HotMemoryRecipeStep as ConfigStep;
use crate::generated::verbs::assemble_hot::HotRecipeStep as IdlStep;

/// Convert a config-side recipe step into the IDL-side wire enum.
/// Required because the assembler walks `HotMemoryConfig.recipe`
/// (config-side) and feeds steps into `build_segments` (IDL-side).
impl From<ConfigStep> for IdlStep {
    fn from(s: ConfigStep) -> Self {
        match s {
            ConfigStep::Purpose => IdlStep::Purpose,
            ConfigStep::Index => IdlStep::Index,
            ConfigStep::PinnedFeedback => IdlStep::PinnedFeedback,
            ConfigStep::TopSalienceProject => IdlStep::TopSalienceProject,
            ConfigStep::ActivePlaybook => IdlStep::ActivePlaybook,
            ConfigStep::RecentUserSignal => IdlStep::RecentUserSignal,
        }
    }
}

#[cfg(test)]
mod from_config_tests {
    use super::*;

    #[test]
    fn from_config_recipe_step_round_trips() {
        let pairs = [
            (ConfigStep::Purpose, IdlStep::Purpose),
            (ConfigStep::Index, IdlStep::Index),
            (ConfigStep::PinnedFeedback, IdlStep::PinnedFeedback),
            (ConfigStep::TopSalienceProject, IdlStep::TopSalienceProject),
            (ConfigStep::ActivePlaybook, IdlStep::ActivePlaybook),
            (ConfigStep::RecentUserSignal, IdlStep::RecentUserSignal),
        ];
        for (cfg, idl) in pairs {
            assert_eq!(IdlStep::from(cfg), idl);
        }
    }
}
