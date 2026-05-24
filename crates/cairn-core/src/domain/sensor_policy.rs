//! Pure local sensor policy vocabulary.
//!
//! This module has no I/O. CLI and store adapters use these names to map
//! config, consent-journal rows, capture source families, and metrics onto the
//! same closed set of local sensors.

use crate::domain::SourceFamily;

/// Local sensor families controlled by `cairn sensor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalSensorName {
    /// Harness lifecycle hook sensor.
    Hook,
    /// IDE event sensor.
    Ide,
    /// Terminal command/output sensor.
    Terminal,
    /// Clipboard snapshot sensor.
    Clipboard,
    /// Voice microphone transcript sensor.
    Voice,
    /// Screen OCR and active-window sensor.
    Screen,
    /// Batch recording ingest pseudo-sensor.
    Recording,
}

impl LocalSensorName {
    /// Every P0 local sensor family in stable display order.
    pub const ALL: [Self; 7] = [
        Self::Hook,
        Self::Ide,
        Self::Terminal,
        Self::Clipboard,
        Self::Voice,
        Self::Screen,
        Self::Recording,
    ];

    /// Stable command/JSON name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hook => "hook",
            Self::Ide => "ide",
            Self::Terminal => "terminal",
            Self::Clipboard => "clipboard",
            Self::Voice => "voice",
            Self::Screen => "screen",
            Self::Recording => "recording",
        }
    }

    /// Parse a command/JSON sensor name.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "hook" | "hooks" => Some(Self::Hook),
            "ide" => Some(Self::Ide),
            "terminal" => Some(Self::Terminal),
            "clipboard" => Some(Self::Clipboard),
            "voice" => Some(Self::Voice),
            "screen" => Some(Self::Screen),
            "recording" => Some(Self::Recording),
            _ => None,
        }
    }

    /// Capture source family emitted by this sensor.
    #[must_use]
    pub const fn source_family(self) -> SourceFamily {
        match self {
            Self::Hook => SourceFamily::Hook,
            Self::Ide => SourceFamily::Ide,
            Self::Terminal => SourceFamily::Terminal,
            Self::Clipboard => SourceFamily::Clipboard,
            Self::Voice => SourceFamily::Voice,
            Self::Screen => SourceFamily::Screen,
            Self::Recording => SourceFamily::RecordingBatch,
        }
    }

    /// Map a capture source family to the corresponding local sensor family.
    #[must_use]
    pub const fn from_source_family(family: SourceFamily) -> Option<Self> {
        match family {
            SourceFamily::Hook => Some(Self::Hook),
            SourceFamily::Ide => Some(Self::Ide),
            SourceFamily::Terminal => Some(Self::Terminal),
            SourceFamily::Clipboard => Some(Self::Clipboard),
            SourceFamily::Voice => Some(Self::Voice),
            SourceFamily::Screen => Some(Self::Screen),
            SourceFamily::RecordingBatch => Some(Self::Recording),
            SourceFamily::Cli
            | SourceFamily::Mcp
            | SourceFamily::Proactive
            | SourceFamily::External => None,
        }
    }

    /// Family-level consent label body, without the `snr:` prefix.
    #[must_use]
    pub const fn family_label(self) -> &'static str {
        match self {
            Self::Hook => "local:hook:default:v1",
            Self::Ide => "local:ide:default:v1",
            Self::Terminal => "local:terminal:default:v1",
            Self::Clipboard => "local:clipboard:default:v1",
            Self::Voice => "local:voice:default:v1",
            Self::Screen => "local:screen:default:v1",
            Self::Recording => "local:recording:default:v1",
        }
    }
}

/// Body-free reason a local sensor gate denied capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorGateReason {
    /// Sensor is disabled in config.
    Disabled,
    /// Consent journal has no active enablement.
    PrivacyDenied,
    /// Configured source-side budget was exhausted.
    BudgetExceeded,
}

impl SensorGateReason {
    /// Stable JSON/policy-trace reason.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::PrivacyDenied => "privacy_denied",
            Self::BudgetExceeded => "budget_exceeded",
        }
    }
}

/// Counts used when evaluating a source-side capture budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetObservation {
    /// Number of observations represented by this capture attempt.
    pub items: u64,
    /// Body or artifact bytes represented by this capture attempt.
    pub bytes: u64,
}
