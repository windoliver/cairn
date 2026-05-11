//! IDE sensor emission.

use cairn_core::domain::{
    CaptureEventId, CapturePayload, CaptureRefs, Rfc3339Timestamp, SourceFamily,
};

use crate::event::build_auto_event;
use crate::{DropReason, EmitOutcome, LocalSensorConfig, SensorKind};

const SENSOR_LABEL: &str = "local:ide:default:v1";

/// Sanitized IDE observation ready for capture event construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdeObservation {
    /// Capture event id assigned by the caller.
    pub event_id: CaptureEventId,
    /// Observation capture timestamp.
    pub captured_at: Rfc3339Timestamp,
    /// Workspace-relative affected file path.
    pub file_path: String,
    /// IDE event subtype.
    pub event_kind: String,
    /// Optional session, turn, and tool references.
    pub refs: Option<CaptureRefs>,
    /// Sanitized raw payload bytes stored behind the capture payload ref.
    pub raw_payload: Vec<u8>,
}

/// Emit one IDE observation as a validated capture event.
#[must_use]
pub fn emit(config: &LocalSensorConfig, observation: IdeObservation) -> EmitOutcome {
    if !config.ide.enabled {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Ide,
            reason: DropReason::Disabled,
        };
    }

    if !config.ide.budget.allows(1, observation.raw_payload.len()) {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Ide,
            reason: DropReason::BudgetExceeded,
        };
    }

    let IdeObservation {
        event_id,
        captured_at,
        file_path,
        event_kind,
        refs,
        raw_payload,
    } = observation;

    match build_auto_event(
        event_id,
        captured_at,
        SENSOR_LABEL,
        CapturePayload::Ide {
            file_path,
            event_kind,
        },
        SourceFamily::Ide,
        refs,
        &raw_payload,
    ) {
        Ok(event) => EmitOutcome::Emitted(event),
        Err(err) => EmitOutcome::Dropped {
            sensor: SensorKind::Ide,
            reason: DropReason::MalformedObservation(err.to_string()),
        },
    }
}
