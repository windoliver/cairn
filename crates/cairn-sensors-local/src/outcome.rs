//! Local sensor emission outcomes.

use cairn_core::domain::{CaptureEvent, SourceFamily};

/// Local sensor family handled by this adapter crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SensorKind {
    /// Harness hook sensor.
    Hook,
    /// IDE event sensor.
    Ide,
    /// Terminal command/output sensor.
    Terminal,
    /// Clipboard snapshot sensor.
    Clipboard,
}

impl SensorKind {
    /// Convert a core source family emitted by this crate into its sensor kind.
    #[must_use]
    pub const fn from_source_family(family: SourceFamily) -> Option<Self> {
        match family {
            SourceFamily::Hook => Some(Self::Hook),
            SourceFamily::Ide => Some(Self::Ide),
            SourceFamily::Terminal => Some(Self::Terminal),
            SourceFamily::Clipboard => Some(Self::Clipboard),
            _ => None,
        }
    }
}

/// Reason an observation produced no event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropReason {
    /// Sensor was disabled at source.
    Disabled,
    /// Raw observation exceeded its configured source-side budget.
    BudgetExceeded,
    /// Local privacy policy rejected the observation.
    PolicyRejected(String),
    /// Observation was malformed or failed core capture validation.
    MalformedObservation(String),
}

/// Result of trying to emit one local sensor event.
#[derive(Debug, Clone, PartialEq)]
pub enum EmitOutcome {
    /// Observation became a validated capture event.
    Emitted(CaptureEvent),
    /// Observation was dropped before event emission.
    Dropped {
        /// Sensor that dropped the observation.
        sensor: SensorKind,
        /// Concrete drop reason.
        reason: DropReason,
    },
}

impl EmitOutcome {
    /// Borrow the emitted event when present.
    #[must_use]
    pub const fn event(&self) -> Option<&CaptureEvent> {
        match self {
            Self::Emitted(event) => Some(event),
            Self::Dropped { .. } => None,
        }
    }

    /// Sensor kind associated with this outcome.
    #[must_use]
    pub const fn sensor(&self) -> Option<SensorKind> {
        match self {
            Self::Emitted(event) => SensorKind::from_source_family(event.source_family),
            Self::Dropped { sensor, .. } => Some(*sensor),
        }
    }

    /// Borrow the drop reason when present.
    #[must_use]
    pub const fn drop_reason(&self) -> Option<&DropReason> {
        match self {
            Self::Emitted(_) => None,
            Self::Dropped { reason, .. } => Some(reason),
        }
    }
}
