//! Local sensor adapter configuration.

/// Per-sensor event budget enforced before payload hashing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaptureBudget {
    /// Maximum number of observations accepted by a single adapter call.
    pub max_items: Option<usize>,
    /// Maximum raw byte count accepted by a single adapter call.
    pub max_bytes: Option<usize>,
}

impl CaptureBudget {
    /// Return whether this budget accepts `items` and `bytes`.
    #[must_use]
    pub fn allows(self, items: usize, bytes: usize) -> bool {
        self.max_items.is_none_or(|limit| items <= limit)
            && self.max_bytes.is_none_or(|limit| bytes <= limit)
    }
}

/// Shared settings for one local sensor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SensorSettings {
    /// Whether this sensor emits events.
    pub enabled: bool,
    /// Source-side budget for one observation.
    pub budget: CaptureBudget,
}

impl SensorSettings {
    /// Enabled settings with no item or byte limit.
    #[must_use]
    pub const fn enabled() -> Self {
        Self {
            enabled: true,
            budget: CaptureBudget {
                max_items: None,
                max_bytes: None,
            },
        }
    }

    /// Disabled settings with no item or byte limit.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            budget: CaptureBudget {
                max_items: None,
                max_bytes: None,
            },
        }
    }
}

/// Configuration for deterministic local sensor adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalSensorConfig {
    /// Hook sensor settings.
    pub hooks: SensorSettings,
    /// IDE sensor settings.
    pub ide: SensorSettings,
    /// Terminal sensor settings.
    pub terminal: SensorSettings,
    /// Clipboard sensor settings.
    pub clipboard: SensorSettings,
    /// Voice sensor settings.
    pub voice: SensorSettings,
    /// Screen OCR and active-window sensor settings.
    pub screen: SensorSettings,
}

impl LocalSensorConfig {
    /// Disable every local sensor.
    #[must_use]
    pub const fn all_disabled() -> Self {
        Self {
            hooks: SensorSettings::disabled(),
            ide: SensorSettings::disabled(),
            terminal: SensorSettings::disabled(),
            clipboard: SensorSettings::disabled(),
            voice: SensorSettings::disabled(),
            screen: SensorSettings::disabled(),
        }
    }

    /// Map the core vault sensor config into local adapter settings.
    #[must_use]
    pub fn from_core(config: &cairn_core::config::SensorsConfig) -> Self {
        Self {
            hooks: settings_from_core(&config.hooks),
            ide: settings_from_core(&config.ide),
            terminal: settings_from_core(&config.terminal),
            clipboard: settings_from_core(&config.clipboard),
            voice: settings_from_core(&config.voice),
            screen: SensorSettings {
                enabled: config.screen.enabled,
                budget: CaptureBudget {
                    max_items: Some(u32_to_usize(config.screen.budget.max_frames_per_minute)),
                    max_bytes: Some(u32_to_usize(config.screen.budget.max_text_bytes_per_event)),
                },
            },
        }
    }
}

fn settings_from_core(config: &cairn_core::config::LocalSensorRuntimeConfig) -> SensorSettings {
    SensorSettings {
        enabled: config.enabled,
        budget: CaptureBudget {
            max_items: config.budget.max_items.map(u64_to_usize),
            max_bytes: config.budget.max_bytes.map(u64_to_usize),
        },
    }
}

fn u64_to_usize(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

fn u32_to_usize(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

impl Default for LocalSensorConfig {
    fn default() -> Self {
        Self {
            hooks: SensorSettings::enabled(),
            ide: SensorSettings::enabled(),
            terminal: SensorSettings::disabled(),
            clipboard: SensorSettings::disabled(),
            voice: SensorSettings::disabled(),
            screen: SensorSettings::disabled(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_core_sensor_config_to_local_adapter_config() {
        let mut config = cairn_core::config::CairnConfig::default();
        config.sensors.clipboard.enabled = true;
        config.sensors.clipboard.budget.max_bytes = Some(128);
        let local = LocalSensorConfig::from_core(&config.sensors);
        assert!(local.clipboard.enabled);
        assert_eq!(local.clipboard.budget.max_bytes, Some(128));
        assert!(!local.screen.enabled);
    }
}
