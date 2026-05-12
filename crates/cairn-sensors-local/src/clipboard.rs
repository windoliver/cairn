//! Clipboard sensor emission.

use cairn_core::domain::{
    CaptureEventId, CapturePayload, CaptureRefs, Rfc3339Timestamp, SourceFamily,
};

use crate::event::build_auto_event;
use crate::policy::{PolicyAction, sanitize_text_payload};
use crate::{DropReason, EmitOutcome, LocalSensorConfig, SensorKind};

const SENSOR_LABEL: &str = "local:clipboard:default:v1";
const TEXT_PLAIN: &str = "text/plain";

/// Raw clipboard observation ready for policy sanitization and capture event construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardObservation {
    /// Capture event id assigned by the caller.
    pub event_id: CaptureEventId,
    /// Observation capture timestamp.
    pub captured_at: Rfc3339Timestamp,
    /// Clipboard MIME type.
    pub mime_type: String,
    /// Raw clipboard payload bytes.
    pub bytes: Vec<u8>,
    /// Whether non-text clipboard payloads should emit metadata only.
    pub metadata_only: bool,
    /// Optional session, turn, and tool references.
    pub refs: Option<CaptureRefs>,
}

/// Emit one clipboard observation as a validated capture event.
#[must_use]
pub fn emit(config: &LocalSensorConfig, observation: ClipboardObservation) -> EmitOutcome {
    if !config.clipboard.enabled {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Clipboard,
            reason: DropReason::Disabled,
        };
    }

    if !config.clipboard.budget.allows(1, observation.bytes.len()) {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Clipboard,
            reason: DropReason::BudgetExceeded,
        };
    }

    let (sanitized_bytes, payload_byte_len) = if is_text_plain_mime(&observation.mime_type) {
        let Ok(text) = std::str::from_utf8(&observation.bytes) else {
            return EmitOutcome::Dropped {
                sensor: SensorKind::Clipboard,
                reason: DropReason::PolicyRejected("clipboard text is not UTF-8".to_owned()),
            };
        };
        match sanitize_text_payload(text) {
            PolicyAction::Sanitized(text) => {
                let sanitized_bytes = text.into_bytes();
                let byte_len = sanitized_bytes.len();
                (sanitized_bytes, byte_len)
            }
            PolicyAction::Rejected(reason) => {
                return EmitOutcome::Dropped {
                    sensor: SensorKind::Clipboard,
                    reason: DropReason::PolicyRejected(reason),
                };
            }
        }
    } else if observation.metadata_only {
        (Vec::new(), observation.bytes.len())
    } else {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Clipboard,
            reason: DropReason::PolicyRejected("unsupported clipboard MIME type".to_owned()),
        };
    };

    let Ok(byte_len) = u64::try_from(payload_byte_len) else {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Clipboard,
            reason: DropReason::MalformedObservation(
                "clipboard payload length exceeds u64".to_owned(),
            ),
        };
    };

    match build_auto_event(
        observation.event_id,
        observation.captured_at,
        SENSOR_LABEL,
        CapturePayload::Clipboard {
            mime_type: observation.mime_type,
            byte_len,
        },
        SourceFamily::Clipboard,
        observation.refs,
        &sanitized_bytes,
    ) {
        Ok(event) => EmitOutcome::Emitted(event),
        Err(err) => EmitOutcome::Dropped {
            sensor: SensorKind::Clipboard,
            reason: DropReason::MalformedObservation(err.to_string()),
        },
    }
}

fn is_text_plain_mime(mime_type: &str) -> bool {
    mime_type
        .split(';')
        .next()
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case(TEXT_PLAIN))
}
