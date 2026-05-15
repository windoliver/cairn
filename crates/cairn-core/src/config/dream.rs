//! `DreamWorkflow` configuration (issue #91, brief §10.1, §10.2).
//!
//! Minimum P0 surface: a single `LLMDreamWorker` tier. Cron cadence and
//! the REM / Deep tiers are deferred. Operators may override values
//! per-vault via `.cairn/config.yaml` or per-folder via `_policy.yaml`.

use serde::{Deserialize, Serialize};

/// Typed configuration for the minimum-path `DreamWorkflow`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DreamConfig {
    /// Master switch. When `false`, no `DreamWorkflow` jobs are
    /// enqueued and `status` does not advertise
    /// `cairn.workflows.v1.dream`.
    #[serde(default = "defaults::enabled")]
    pub enabled: bool,

    /// Number of recent records the LLM worker reads per dream call.
    /// Caps prompt size and per-call latency. Brief §10.1 calls this
    /// the "window" — equivalent to the consolidation window.
    #[serde(default = "defaults::window_size_records")]
    pub window_size_records: u32,

    /// Approximate hard cap on the LLM completion in tokens. Mirrors
    /// `ConsolidationConfig::token_budget` semantics.
    #[serde(default = "defaults::completion_token_budget")]
    pub completion_token_budget: u32,

    /// LLM sampling temperature for distillation calls. Range `[0.0,
    /// 2.0]`. Brief §10.2 expects deterministic distillation by
    /// default; 0.0 keeps the dream record byte-stable for the same
    /// input window.
    #[serde(default = "defaults::llm_temperature")]
    pub llm_temperature: f32,
}

impl Default for DreamConfig {
    fn default() -> Self {
        Self {
            enabled: defaults::enabled(),
            window_size_records: defaults::window_size_records(),
            completion_token_budget: defaults::completion_token_budget(),
            llm_temperature: defaults::llm_temperature(),
        }
    }
}

mod defaults {
    pub const fn enabled() -> bool {
        // P0 holds the master switch off until an `LLMProvider` is
        // configured; flipping `enabled = true` on a vault without a
        // provider is rejected by `validate()` so status never
        // advertises a workflow that would `Permanent`-fail every
        // handle call.
        false
    }
    pub const fn window_size_records() -> u32 {
        16
    }
    pub const fn completion_token_budget() -> u32 {
        1024
    }
    pub const fn llm_temperature() -> f32 {
        0.0
    }
}

/// Validation errors raised by [`DreamConfig::validate`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DreamConfigError {
    /// `window_size_records` was zero.
    #[error("dream.window_size_records must be \u{2265} 1")]
    ZeroWindow,
    /// `completion_token_budget` below the workable floor (64 tokens).
    #[error("dream.completion_token_budget {actual} < required floor {floor}")]
    BudgetTooLow {
        /// Provided value.
        actual: u32,
        /// Required minimum.
        floor: u32,
    },
    /// `llm_temperature` outside the supported `[0.0, 2.0]` band.
    #[error("dream.llm_temperature {actual} outside [0.0, {max}]")]
    TemperatureOutOfRange {
        /// Provided value.
        actual: f32,
        /// Maximum accepted value.
        max: f32,
    },
}

impl DreamConfig {
    /// Lowest-acceptable `completion_token_budget`.
    pub const COMPLETION_BUDGET_FLOOR: u32 = 64;

    /// Highest-acceptable `llm_temperature`.
    pub const TEMPERATURE_MAX: f32 = 2.0;

    /// Validate semantic invariants the serde layer cannot express.
    ///
    /// # Errors
    /// Returns the first violated invariant.
    pub fn validate(&self) -> Result<(), DreamConfigError> {
        if self.window_size_records == 0 {
            return Err(DreamConfigError::ZeroWindow);
        }
        if self.completion_token_budget < Self::COMPLETION_BUDGET_FLOOR {
            return Err(DreamConfigError::BudgetTooLow {
                actual: self.completion_token_budget,
                floor: Self::COMPLETION_BUDGET_FLOOR,
            });
        }
        if !(0.0..=Self::TEMPERATURE_MAX).contains(&self.llm_temperature)
            || self.llm_temperature.is_nan()
        {
            return Err(DreamConfigError::TemperatureOutOfRange {
                actual: self.llm_temperature,
                max: Self::TEMPERATURE_MAX,
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
        let cfg = DreamConfig::default();
        assert!(!cfg.enabled, "dream P0 default is OFF (no llm provider)");
        assert_eq!(cfg.window_size_records, 16);
        assert_eq!(cfg.completion_token_budget, 1024);
        assert!(cfg.llm_temperature.abs() < f32::EPSILON);
    }

    #[test]
    fn rejects_zero_window() {
        let cfg = DreamConfig {
            window_size_records: 0,
            ..DreamConfig::default()
        };
        assert!(matches!(cfg.validate(), Err(DreamConfigError::ZeroWindow)));
    }

    #[test]
    fn rejects_budget_below_floor() {
        let cfg = DreamConfig {
            completion_token_budget: 63,
            ..DreamConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(DreamConfigError::BudgetTooLow { .. })
        ));
    }

    #[test]
    fn rejects_temperature_out_of_range() {
        let cfg = DreamConfig {
            llm_temperature: 2.5,
            ..DreamConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(DreamConfigError::TemperatureOutOfRange { .. })
        ));
    }

    #[test]
    fn rejects_negative_temperature() {
        let cfg = DreamConfig {
            llm_temperature: -0.1,
            ..DreamConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(DreamConfigError::TemperatureOutOfRange { .. })
        ));
    }

    #[test]
    fn rejects_nan_temperature() {
        let cfg = DreamConfig {
            llm_temperature: f32::NAN,
            ..DreamConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(DreamConfigError::TemperatureOutOfRange { .. })
        ));
    }
}
