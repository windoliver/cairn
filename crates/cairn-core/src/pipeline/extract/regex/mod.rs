//! `RegexExtractor` — regex/state-machine implementation of
//! `ExtractorWorker`. See spec §6.

pub mod defaults;
pub(crate) mod dispatch;
pub mod prefilter;
pub mod rule;

pub use prefilter::TriggerPrefilter;
pub use rule::{CompiledRule, CompiledRuleKind, RegexRule, RuleOrigin, RuleSet, ToolFrameFamily};
