//! `DreamWorkflow` configuration (brief §10.1, §10.2).
//!
//! P1 exposes all three dream tiers as explicit configuration:
//! Light Sleep, REM Sleep, and Deep Dreaming. Each tier carries the
//! cadence, input window, output kind, worker mode, and budget needed
//! by the workflow host while keeping this crate pure data.

use serde::{Deserialize, Serialize};

/// Dream tier from brief §10.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DreamTier {
    /// Light Sleep: cheap session/current-day maintenance.
    #[default]
    LightSleep,
    /// REM Sleep: hourly or high-salience mid-depth consolidation.
    RemSleep,
    /// Deep Dreaming: nightly or cron full-vault sweep.
    DeepDreaming,
}

impl DreamTier {
    /// Stable lowercase discriminator used in job ids, queue keys, and
    /// workflow metadata.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LightSleep => "light_sleep",
            Self::RemSleep => "rem_sleep",
            Self::DeepDreaming => "deep_dreaming",
        }
    }
}

impl std::fmt::Display for DreamTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Pluggable worker mode from brief §10.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DreamWorkerMode {
    /// `LLMDreamWorker`: one bounded LLM call over the selected window.
    Llm,
    /// `HybridDreamWorker`: deterministic prune, then bounded LLM call.
    Hybrid,
    /// `AgentDreamWorker`: bounded agent runtime with read-only tools.
    Agent,
}

/// Tier cadence descriptor from brief §10.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DreamCadence {
    /// Every Stop hook and every configured N turns.
    StopHookAndTurns,
    /// Hourly or immediately after high-salience writes.
    HourlyOrHighSalience,
    /// Nightly or externally triggered cron.
    NightlyOrCron,
}

/// Tier input window descriptor from brief §10.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DreamInputWindow {
    /// Current session and the last 24 hours.
    CurrentSessionAndLast24h,
    /// Last seven days.
    Last7Days,
    /// Entire vault.
    EntireVault,
}

/// Tier output descriptor from brief §10.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DreamOutputKind {
    /// Index updates and conflict markers.
    IndexUpdatesAndConflictMarkers,
    /// Consolidated records and reflection kicks.
    ConsolidatedRecordsAndReflectionKicks,
    /// Promotions, skills, synthesis pages, and lint report updates.
    PromotionsSkillsSynthesisAndLint,
}

/// Budget and metadata for one dream tier.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DreamTierConfig {
    /// Slot this config belongs to.
    #[serde(default)]
    pub tier: DreamTier,
    /// When the tier is eligible to run.
    pub cadence: DreamCadence,
    /// Records the tier may read.
    pub input_window: DreamInputWindow,
    /// Durable output shape the tier may produce.
    pub output_kind: DreamOutputKind,
    /// Worker dispatch mode.
    pub worker: DreamWorkerMode,
    /// Number of recent eligible records the worker reads per run.
    pub window_size_records: u32,

    /// Approximate hard cap on the LLM completion in tokens. Mirrors
    /// `ConsolidationConfig::token_budget` semantics.
    pub completion_token_budget: u32,

    /// Maximum wall-clock budget for one tier run.
    pub max_wall_ms: u32,

    /// Maximum tool calls allowed. P1 `llm` and `hybrid` workers do
    /// not use tool calls, so defaults are zero. P2 agent mode owns
    /// nonzero values.
    pub max_tool_calls: u32,

    /// LLM sampling temperature for distillation calls. Range `[0.0,
    /// 2.0]`. Brief §10.2 expects deterministic distillation by
    /// default; 0.0 keeps the dream record byte-stable for the same
    /// input window.
    pub llm_temperature: f32,
}

impl DreamTierConfig {
    /// Default Light Sleep config.
    #[must_use]
    pub const fn light_sleep_default() -> Self {
        Self {
            tier: DreamTier::LightSleep,
            cadence: DreamCadence::StopHookAndTurns,
            input_window: DreamInputWindow::CurrentSessionAndLast24h,
            output_kind: DreamOutputKind::IndexUpdatesAndConflictMarkers,
            worker: DreamWorkerMode::Llm,
            window_size_records: 16,
            completion_token_budget: 1024,
            max_wall_ms: 60_000,
            max_tool_calls: 0,
            llm_temperature: 0.0,
        }
    }

    /// Default REM Sleep config.
    #[must_use]
    pub const fn rem_sleep_default() -> Self {
        Self {
            tier: DreamTier::RemSleep,
            cadence: DreamCadence::HourlyOrHighSalience,
            input_window: DreamInputWindow::Last7Days,
            output_kind: DreamOutputKind::ConsolidatedRecordsAndReflectionKicks,
            worker: DreamWorkerMode::Hybrid,
            window_size_records: 128,
            completion_token_budget: 4096,
            max_wall_ms: 180_000,
            max_tool_calls: 0,
            llm_temperature: 0.0,
        }
    }

    /// Default Deep Dreaming config.
    #[must_use]
    pub const fn deep_dreaming_default() -> Self {
        Self {
            tier: DreamTier::DeepDreaming,
            cadence: DreamCadence::NightlyOrCron,
            input_window: DreamInputWindow::EntireVault,
            output_kind: DreamOutputKind::PromotionsSkillsSynthesisAndLint,
            worker: DreamWorkerMode::Hybrid,
            window_size_records: 1024,
            completion_token_budget: 16_384,
            max_wall_ms: 900_000,
            max_tool_calls: 0,
            llm_temperature: 0.0,
        }
    }
}

/// Typed configuration for the tiered `DreamWorkflow`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DreamConfig {
    /// Master switch. When `false`, no `DreamWorkflow` jobs are
    /// enqueued and `status` does not advertise
    /// `cairn.workflows.v1.dream`.
    #[serde(default = "defaults::enabled")]
    pub enabled: bool,

    /// Light Sleep tier.
    #[serde(default = "DreamTierConfig::light_sleep_default")]
    pub light_sleep: DreamTierConfig,

    /// REM Sleep tier.
    #[serde(default = "DreamTierConfig::rem_sleep_default")]
    pub rem_sleep: DreamTierConfig,

    /// Deep Dreaming tier.
    #[serde(default = "DreamTierConfig::deep_dreaming_default")]
    pub deep_dreaming: DreamTierConfig,
}

impl Default for DreamConfig {
    fn default() -> Self {
        Self {
            enabled: defaults::enabled(),
            light_sleep: DreamTierConfig::light_sleep_default(),
            rem_sleep: DreamTierConfig::rem_sleep_default(),
            deep_dreaming: DreamTierConfig::deep_dreaming_default(),
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
    /// A tier config was placed in the wrong slot.
    #[error("dream.{slot} has tier {actual}; expected {expected}")]
    TierSlotMismatch {
        /// Config slot.
        slot: &'static str,
        /// Expected tier.
        expected: DreamTier,
        /// Actual tier.
        actual: DreamTier,
    },
    /// Agent worker mode requires at least one tool call in its tier budget.
    #[error("dream.{tier}.max_tool_calls must be >= 1 for agent worker")]
    AgentToolBudgetZero {
        /// Tier whose tool budget is invalid.
        tier: DreamTier,
    },
}

impl DreamConfig {
    /// Lowest-acceptable `completion_token_budget`.
    pub const COMPLETION_BUDGET_FLOOR: u32 = 64;

    /// Highest-acceptable `llm_temperature`.
    pub const TEMPERATURE_MAX: f32 = 2.0;

    /// Return the config for `tier`.
    #[must_use]
    pub const fn tier_config(&self, tier: DreamTier) -> DreamTierConfig {
        match tier {
            DreamTier::LightSleep => self.light_sleep,
            DreamTier::RemSleep => self.rem_sleep,
            DreamTier::DeepDreaming => self.deep_dreaming,
        }
    }

    /// Validate semantic invariants the serde layer cannot express.
    ///
    /// # Errors
    /// Returns the first violated invariant.
    pub fn validate(&self) -> Result<(), DreamConfigError> {
        validate_tier_slot("light_sleep", DreamTier::LightSleep, self.light_sleep)?;
        validate_tier_slot("rem_sleep", DreamTier::RemSleep, self.rem_sleep)?;
        validate_tier_slot("deep_dreaming", DreamTier::DeepDreaming, self.deep_dreaming)?;
        Ok(())
    }
}

fn validate_tier_slot(
    slot: &'static str,
    expected: DreamTier,
    cfg: DreamTierConfig,
) -> Result<(), DreamConfigError> {
    if cfg.tier != expected {
        return Err(DreamConfigError::TierSlotMismatch {
            slot,
            expected,
            actual: cfg.tier,
        });
    }
    if cfg.window_size_records == 0 {
        return Err(DreamConfigError::ZeroWindow);
    }
    if cfg.completion_token_budget < DreamConfig::COMPLETION_BUDGET_FLOOR {
        return Err(DreamConfigError::BudgetTooLow {
            actual: cfg.completion_token_budget,
            floor: DreamConfig::COMPLETION_BUDGET_FLOOR,
        });
    }
    if matches!(cfg.worker, DreamWorkerMode::Agent) && cfg.max_tool_calls == 0 {
        return Err(DreamConfigError::AgentToolBudgetZero { tier: cfg.tier });
    }
    if !(0.0..=DreamConfig::TEMPERATURE_MAX).contains(&cfg.llm_temperature)
        || cfg.llm_temperature.is_nan()
    {
        return Err(DreamConfigError::TemperatureOutOfRange {
            actual: cfg.llm_temperature,
            max: DreamConfig::TEMPERATURE_MAX,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_brief_p1_tiers() {
        let cfg = DreamConfig::default();
        assert!(!cfg.enabled, "dream P0 default is OFF (no llm provider)");
        assert_eq!(cfg.light_sleep.tier, DreamTier::LightSleep);
        assert_eq!(cfg.light_sleep.cadence, DreamCadence::StopHookAndTurns);
        assert_eq!(
            cfg.light_sleep.input_window,
            DreamInputWindow::CurrentSessionAndLast24h
        );
        assert_eq!(
            cfg.light_sleep.output_kind,
            DreamOutputKind::IndexUpdatesAndConflictMarkers
        );
        assert_eq!(cfg.light_sleep.worker, DreamWorkerMode::Llm);
        assert_eq!(cfg.light_sleep.window_size_records, 16);
        assert_eq!(cfg.light_sleep.completion_token_budget, 1024);

        assert_eq!(cfg.rem_sleep.tier, DreamTier::RemSleep);
        assert_eq!(cfg.rem_sleep.cadence, DreamCadence::HourlyOrHighSalience);
        assert_eq!(cfg.rem_sleep.input_window, DreamInputWindow::Last7Days);
        assert_eq!(
            cfg.rem_sleep.output_kind,
            DreamOutputKind::ConsolidatedRecordsAndReflectionKicks
        );
        assert_eq!(cfg.rem_sleep.worker, DreamWorkerMode::Hybrid);

        assert_eq!(cfg.deep_dreaming.tier, DreamTier::DeepDreaming);
        assert_eq!(cfg.deep_dreaming.cadence, DreamCadence::NightlyOrCron);
        assert_eq!(
            cfg.deep_dreaming.input_window,
            DreamInputWindow::EntireVault
        );
        assert_eq!(
            cfg.deep_dreaming.output_kind,
            DreamOutputKind::PromotionsSkillsSynthesisAndLint
        );
        assert_eq!(cfg.deep_dreaming.worker, DreamWorkerMode::Hybrid);
    }

    #[test]
    fn agent_worker_round_trips() {
        let json = serde_json::to_string(&DreamWorkerMode::Agent).expect("serialize");
        assert_eq!(json, "\"agent\"");
        let back: DreamWorkerMode = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, DreamWorkerMode::Agent);
    }

    #[test]
    fn agent_worker_requires_nonzero_tool_budget() {
        let mut cfg = DreamConfig::default();
        cfg.enabled = true;
        cfg.deep_dreaming.worker = DreamWorkerMode::Agent;
        cfg.deep_dreaming.max_tool_calls = 0;

        let err = cfg.validate().expect_err("agent mode must budget tools");
        assert!(matches!(err, DreamConfigError::AgentToolBudgetZero { tier }
            if tier == DreamTier::DeepDreaming));
    }

    #[test]
    fn rejects_zero_window() {
        let cfg = DreamConfig {
            light_sleep: DreamTierConfig {
                window_size_records: 0,
                ..DreamTierConfig::light_sleep_default()
            },
            ..DreamConfig::default()
        };
        assert!(matches!(cfg.validate(), Err(DreamConfigError::ZeroWindow)));
    }

    #[test]
    fn rejects_budget_below_floor() {
        let cfg = DreamConfig {
            rem_sleep: DreamTierConfig {
                completion_token_budget: 63,
                ..DreamTierConfig::rem_sleep_default()
            },
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
            deep_dreaming: DreamTierConfig {
                llm_temperature: 2.5,
                ..DreamTierConfig::deep_dreaming_default()
            },
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
            light_sleep: DreamTierConfig {
                llm_temperature: -0.1,
                ..DreamTierConfig::light_sleep_default()
            },
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
            light_sleep: DreamTierConfig {
                llm_temperature: f32::NAN,
                ..DreamTierConfig::light_sleep_default()
            },
            ..DreamConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(DreamConfigError::TemperatureOutOfRange { .. })
        ));
    }

    #[test]
    fn rejects_mismatched_tier_slot() {
        let cfg = DreamConfig {
            rem_sleep: DreamTierConfig {
                tier: DreamTier::DeepDreaming,
                ..DreamTierConfig::rem_sleep_default()
            },
            ..DreamConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(DreamConfigError::TierSlotMismatch { .. })
        ));
    }

    #[test]
    fn resolves_tier_config() {
        let cfg = DreamConfig::default();
        assert_eq!(
            cfg.tier_config(DreamTier::LightSleep).cadence,
            DreamCadence::StopHookAndTurns
        );
        assert_eq!(
            cfg.tier_config(DreamTier::RemSleep).input_window,
            DreamInputWindow::Last7Days
        );
        assert_eq!(
            cfg.tier_config(DreamTier::DeepDreaming).output_kind,
            DreamOutputKind::PromotionsSkillsSynthesisAndLint
        );
    }
}
