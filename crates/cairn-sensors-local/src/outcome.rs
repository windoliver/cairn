//! Local sensor emission outcomes.

use cairn_core::domain::{CaptureEvent, SourceFamily, metrics::MetricEvent};

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
    /// Voice microphone transcript sensor.
    Voice,
    /// Screen OCR and active-window sensor.
    Screen,
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
            SourceFamily::Voice => Some(Self::Voice),
            SourceFamily::Screen => Some(Self::Screen),
            _ => None,
        }
    }

    /// Stable label used in telemetry dimensions.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hook => "hook",
            Self::Ide => "ide",
            Self::Terminal => "terminal",
            Self::Clipboard => "clipboard",
            Self::Voice => "voice",
            Self::Screen => "screen",
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

impl DropReason {
    /// Body-free error class suitable for metrics and traces.
    #[must_use]
    pub const fn as_metric_error(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::BudgetExceeded => "budget_exceeded",
            Self::PolicyRejected(_) => "policy_rejected",
            Self::MalformedObservation(_) => "malformed_observation",
        }
    }
}

/// Result of trying to emit one local sensor event.
// Keep the emitted event by value: this short-lived adapter return type is
// matched directly by integration tests and callers using the public API.
#[allow(clippy::large_enum_variant)]
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

    /// Build a body-free metric event for this sensor outcome.
    #[must_use]
    pub fn metric_event(
        &self,
        ts_ms: i64,
        latency_ms: u64,
        bytes: u64,
        budget_bytes: Option<u64>,
    ) -> MetricEvent {
        let budget_used_ratio = budget_bytes.and_then(|budget| budget_ratio(bytes, budget));
        let sensor = self
            .sensor()
            .map_or("unknown", SensorKind::as_str)
            .to_owned();
        let (status, error) = match self {
            Self::Emitted(_) => ("emitted", None),
            Self::Dropped { reason, .. } => ("dropped", Some(reason.as_metric_error().to_owned())),
        };

        MetricEvent::SensorEmission {
            ts_ms,
            sensor,
            status: status.to_owned(),
            latency_ms,
            bytes,
            budget_bytes,
            budget_used_ratio,
            error,
            degradation_state: Some("none".to_owned()),
        }
    }
}

fn budget_ratio(bytes: u64, budget: u64) -> Option<f64> {
    if budget == 0 {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    Some(bytes as f64 / budget as f64)
}

#[cfg(test)]
mod tests {
    use cairn_core::domain::metrics::MetricEvent;

    use super::{DropReason, EmitOutcome, SensorKind};

    #[test]
    fn dropped_outcome_builds_body_free_sensor_metric() {
        let outcome = EmitOutcome::Dropped {
            sensor: SensorKind::Clipboard,
            reason: DropReason::PolicyRejected("private key block".to_owned()),
        };

        let metric = outcome.metric_event(10, 7, 64, Some(1_024));
        let MetricEvent::SensorEmission {
            sensor,
            status,
            latency_ms,
            bytes,
            budget_bytes,
            budget_used_ratio,
            error,
            ..
        } = metric
        else {
            panic!("expected SensorEmission");
        };

        assert_eq!(sensor, "clipboard");
        assert_eq!(status, "dropped");
        assert_eq!(latency_ms, 7);
        assert_eq!(bytes, 64);
        assert_eq!(budget_bytes, Some(1_024));
        assert_eq!(budget_used_ratio, Some(0.0625));
        assert_eq!(error.as_deref(), Some("policy_rejected"));
    }
}
