//! Rolling-summary `ConsolidationWorkflow` configuration (brief §5.3, §10.0).
//!
//! All knobs are P0 defaults — they may be overridden per-vault via
//! `.cairn/config.yaml` or per-folder via `_policy.yaml`.

use serde::{Deserialize, Serialize};

/// Typed configuration for the rolling-summary `ConsolidationWorkflow`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsolidationConfig {
    /// Master switch. When `false` the trigger never enqueues and the
    /// status capability advertises `consolidation_deferred`.
    #[serde(default = "defaults::enabled")]
    pub enabled: bool,

    /// Number of consecutive turns covered by one summary record.
    #[serde(default = "defaults::window_size_turns")]
    pub window_size_turns: u32,

    /// Minimum turns since the previous summary before another job is
    /// eligible. Keeps the trigger from firing every turn on a chatty
    /// session.
    #[serde(default = "defaults::min_turns_for_trigger")]
    pub min_turns_for_trigger: u32,

    /// Approximate hard cap on summary body length, in tokens. The
    /// consolidator truncates / re-summarizes any window whose draft
    /// exceeds this.
    #[serde(default = "defaults::token_budget")]
    pub token_budget: u32,

    /// Drop turns from the window whose computed salience falls below
    /// this floor. Range `[0.0, 1.0]`.
    #[serde(default = "defaults::salience_floor")]
    pub salience_floor: f32,
}

impl Default for ConsolidationConfig {
    fn default() -> Self {
        Self {
            enabled: defaults::enabled(),
            window_size_turns: defaults::window_size_turns(),
            min_turns_for_trigger: defaults::min_turns_for_trigger(),
            token_budget: defaults::token_budget(),
            salience_floor: defaults::salience_floor(),
        }
    }
}

mod defaults {
    pub const fn enabled() -> bool {
        true
    }
    pub const fn window_size_turns() -> u32 {
        8
    }
    pub const fn min_turns_for_trigger() -> u32 {
        4
    }
    pub const fn token_budget() -> u32 {
        512
    }
    pub const fn salience_floor() -> f32 {
        0.4
    }
}

/// Validation errors raised by [`ConsolidationConfig::validate`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConsolidationConfigError {
    /// `window_size_turns` was zero.
    #[error("consolidation.window_size_turns must be \u{2265} 1")]
    ZeroWindow,
    /// `token_budget` below the workable floor (32 tokens).
    #[error("consolidation.token_budget {actual} < required floor {floor}")]
    BudgetTooLow {
        /// Provided value.
        actual: u32,
        /// Required minimum.
        floor: u32,
    },
    /// `salience_floor` outside the currently-supported range.
    #[error(
        "consolidation.salience_floor {actual} outside [0, {max}] — real \
         salience scoring is pending; list_trace_turns emits a constant \
         0.5 baseline today, so any floor above that would gate every \
         turn out of every window"
    )]
    SalienceOutOfRange {
        /// Provided value.
        actual: f32,
        /// Max accepted value while real salience scoring is pending.
        max: f32,
    },
}

impl ConsolidationConfig {
    /// Lowest-acceptable `token_budget`. Below this the summary cannot
    /// carry meaningful source-id linkage.
    pub const TOKEN_BUDGET_FLOOR: u32 = 32;

    /// Highest-acceptable `salience_floor` while real per-turn salience
    /// scoring is still pending. `SqliteMemoryStore::list_trace_turns`
    /// stamps every header with a constant `0.5` baseline today, so any
    /// floor above this would gate every turn out of every window and
    /// trigger the round-7 dedupe-poisoning failure mode (round-8
    /// adversarial review #3). Bumped to `1.0` when a real salience
    /// signal lands.
    pub const SALIENCE_FLOOR_MAX: f32 = 0.5;

    /// Validate semantic invariants the serde layer can't express.
    ///
    /// # Errors
    /// Returns the first violated invariant.
    pub fn validate(&self) -> Result<(), ConsolidationConfigError> {
        if self.window_size_turns == 0 {
            return Err(ConsolidationConfigError::ZeroWindow);
        }
        if self.token_budget < Self::TOKEN_BUDGET_FLOOR {
            return Err(ConsolidationConfigError::BudgetTooLow {
                actual: self.token_budget,
                floor: Self::TOKEN_BUDGET_FLOOR,
            });
        }
        if !(0.0..=Self::SALIENCE_FLOOR_MAX).contains(&self.salience_floor) {
            return Err(ConsolidationConfigError::SalienceOutOfRange {
                actual: self.salience_floor,
                max: Self::SALIENCE_FLOOR_MAX,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_brief_p0() {
        let cfg = ConsolidationConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.window_size_turns, 8);
        assert_eq!(cfg.token_budget, 512);
        assert!((cfg.salience_floor - 0.4).abs() < f32::EPSILON);
        assert_eq!(cfg.min_turns_for_trigger, 4);
    }

    #[test]
    fn rejects_zero_window() {
        let cfg = ConsolidationConfig {
            window_size_turns: 0,
            ..ConsolidationConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(ConsolidationConfigError::ZeroWindow)
        ));
    }

    #[test]
    fn rejects_budget_below_floor() {
        let cfg = ConsolidationConfig {
            token_budget: 31,
            ..ConsolidationConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(ConsolidationConfigError::BudgetTooLow { .. })
        ));
    }

    #[test]
    fn salience_floor_out_of_range_rejected() {
        let cfg = ConsolidationConfig {
            salience_floor: 1.5,
            ..ConsolidationConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(ConsolidationConfigError::SalienceOutOfRange { .. })
        ));
    }
}
