//! Harness hook sensor emission.

use cairn_core::domain::{
    CaptureEventId, CapturePayload, CaptureRefs, Rfc3339Timestamp, SourceFamily,
};

use crate::event::build_auto_event;
use crate::{DropReason, EmitOutcome, LocalSensorConfig, SensorKind};

/// Supported local hook harnesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookHarness {
    /// Claude Code hook harness.
    ClaudeCode,
    /// Codex hook harness.
    Codex,
    /// Gemini hook harness.
    Gemini,
}

impl HookHarness {
    const fn sensor_label(self) -> &'static str {
        match self {
            Self::ClaudeCode => "local:hook:cc-session:v1",
            Self::Codex => "local:hook:codex-session:v1",
            Self::Gemini => "local:hook:gemini-session:v1",
        }
    }
}

/// Sanitized hook observation ready for capture event construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookObservation {
    /// Capture event id assigned by the caller.
    pub event_id: CaptureEventId,
    /// Observation capture timestamp.
    pub captured_at: Rfc3339Timestamp,
    /// Hook harness that produced the observation.
    pub harness: HookHarness,
    /// Harness hook name.
    pub hook_name: String,
    /// Optional tool name for tool hook observations.
    pub tool_name: Option<String>,
    /// Optional session, turn, and tool references.
    pub refs: Option<CaptureRefs>,
    /// Sanitized raw payload bytes stored behind the capture payload ref.
    pub raw_payload: Vec<u8>,
}

/// Emit one hook observation as a validated capture event.
#[must_use]
pub fn emit(config: &LocalSensorConfig, observation: HookObservation) -> EmitOutcome {
    if !config.hooks.enabled {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Hook,
            reason: DropReason::Disabled,
        };
    }

    if !config.hooks.budget.allows(1, observation.raw_payload.len()) {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Hook,
            reason: DropReason::BudgetExceeded,
        };
    }

    let HookObservation {
        event_id,
        captured_at,
        harness,
        hook_name,
        tool_name,
        refs,
        raw_payload,
    } = observation;

    match build_auto_event(
        event_id,
        captured_at,
        harness.sensor_label(),
        CapturePayload::Hook {
            hook_name,
            tool_name,
        },
        SourceFamily::Hook,
        refs,
        &raw_payload,
    ) {
        Ok(event) => EmitOutcome::Emitted(event),
        Err(err) => EmitOutcome::Dropped {
            sensor: SensorKind::Hook,
            reason: DropReason::MalformedObservation(err.to_string()),
        },
    }
}
