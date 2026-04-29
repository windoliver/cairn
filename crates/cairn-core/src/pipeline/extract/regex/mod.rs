//! `RegexExtractor` — regex/state-machine implementation of
//! `ExtractorWorker`. See spec §6.

pub mod defaults;
pub(crate) mod dispatch;
pub mod prefilter;
pub mod rule;

pub use prefilter::TriggerPrefilter;
pub use rule::{CompiledRule, CompiledRuleKind, RegexRule, RuleOrigin, RuleSet, ToolFrameFamily};

use async_trait::async_trait;

use super::{ExtractBudget, ExtractError, ExtractInput, ExtractResult, ExtractorWorker};

/// Built-in + user-rule extractor. Implements `ExtractorWorker` over a
/// pre-compiled `RuleSet` and `TriggerPrefilter`.
pub struct RegexExtractor {
    rules: RuleSet,
    prefilter: TriggerPrefilter,
    budget: ExtractBudget,
}

impl RegexExtractor {
    /// Construct with the built-in rule set and the default budget.
    #[must_use]
    pub fn builtin() -> Self {
        Self {
            rules: RuleSet::builtin(),
            prefilter: TriggerPrefilter::new(),
            budget: ExtractBudget::regex_default(),
        }
    }

    /// Construct from any rule set + budget. Used by tests and by #74's
    /// chain dispatcher.
    #[must_use]
    pub fn from_parts(rules: RuleSet, budget: ExtractBudget) -> Self {
        Self {
            rules,
            prefilter: TriggerPrefilter::new(),
            budget,
        }
    }
}

#[async_trait]
impl ExtractorWorker for RegexExtractor {
    fn name(&self) -> &'static str {
        "regex"
    }

    fn budget(&self) -> ExtractBudget {
        self.budget
    }

    async fn extract(&self, input: &ExtractInput<'_>) -> Result<ExtractResult, ExtractError> {
        dispatch::dispatch(&self.rules, &self.prefilter, &self.budget, input).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_constructs() {
        let ex = RegexExtractor::builtin();
        assert_eq!(ex.name(), "regex");
        assert_eq!(ex.budget(), ExtractBudget::regex_default());
    }
}
