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
        }
    }
}

impl Default for LocalSensorConfig {
    fn default() -> Self {
        Self {
            hooks: SensorSettings::enabled(),
            ide: SensorSettings::enabled(),
            terminal: SensorSettings::disabled(),
            clipboard: SensorSettings::disabled(),
            voice: SensorSettings::disabled(),
        }
    }
}
