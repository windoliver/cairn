//! Terminal sensor emission.

use cairn_core::domain::{
    CaptureEventId, CapturePayload, CaptureRefs, Rfc3339Timestamp, SourceFamily, TerminalContext,
};

use crate::event::build_auto_event;
use crate::policy::{PolicyAction, sanitize_text_payload};
use crate::{DropReason, EmitOutcome, LocalSensorConfig, SensorKind};

const SENSOR_LABEL: &str = "local:terminal:default:v1";

/// Raw terminal observation ready for policy sanitization and capture event construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalObservation {
    /// Capture event id assigned by the caller.
    pub event_id: CaptureEventId,
    /// Observation capture timestamp.
    pub captured_at: Rfc3339Timestamp,
    /// Argv-style command line.
    pub command: String,
    /// Process exit code, if the command had completed at capture time.
    pub exit_code: Option<i32>,
    /// Sensor-classified execution context for fresh terminal writes.
    pub context: Option<TerminalContext>,
    /// Terminal output bytes decoded as text by the local sensor.
    pub output: String,
    /// Optional session, turn, and tool references.
    pub refs: Option<CaptureRefs>,
}

/// Emit one terminal observation as a validated capture event.
#[must_use]
pub fn emit(config: &LocalSensorConfig, observation: TerminalObservation) -> EmitOutcome {
    if !config.terminal.enabled {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Terminal,
            reason: DropReason::Disabled,
        };
    }

    let raw_len = observation.command.len() + observation.output.len();
    if !config.terminal.budget.allows(1, raw_len) {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Terminal,
            reason: DropReason::BudgetExceeded,
        };
    }

    let Some(context) = observation.context else {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Terminal,
            reason: DropReason::MalformedObservation("terminal context is required".to_owned()),
        };
    };

    let command = match sanitize_text_payload(&observation.command) {
        PolicyAction::Sanitized(command) => command,
        PolicyAction::Rejected(reason) => {
            return EmitOutcome::Dropped {
                sensor: SensorKind::Terminal,
                reason: DropReason::PolicyRejected(reason),
            };
        }
    };
    let output = match sanitize_text_payload(&observation.output) {
        PolicyAction::Sanitized(output) => output,
        PolicyAction::Rejected(reason) => {
            return EmitOutcome::Dropped {
                sensor: SensorKind::Terminal,
                reason: DropReason::PolicyRejected(reason),
            };
        }
    };
    let sanitized_payload = format!("{command}\n{output}");

    match build_auto_event(
        observation.event_id,
        observation.captured_at,
        SENSOR_LABEL,
        CapturePayload::Terminal {
            command,
            exit_code: observation.exit_code,
            context: Some(context),
        },
        SourceFamily::Terminal,
        observation.refs,
        sanitized_payload.as_bytes(),
    ) {
        Ok(event) => EmitOutcome::Emitted(event),
        Err(err) => EmitOutcome::Dropped {
            sensor: SensorKind::Terminal,
            reason: DropReason::MalformedObservation(err.to_string()),
        },
    }
}
