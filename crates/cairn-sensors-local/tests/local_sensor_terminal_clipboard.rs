#![allow(missing_docs)]

use cairn_core::domain::{
    CaptureEvent, CaptureEventId, CapturePayload, Rfc3339Timestamp, SourceFamily, TerminalContext,
};
use cairn_sensors_local::clipboard::ClipboardObservation;
use cairn_sensors_local::terminal::TerminalObservation;
use cairn_sensors_local::{
    CaptureBudget, DropReason, EmitOutcome, LocalSensorConfig, SensorKind, SensorSettings,
    clipboard, terminal,
};
use sha2::{Digest as _, Sha256};

fn id(raw: &str) -> CaptureEventId {
    CaptureEventId::parse(raw).expect("valid test ULID")
}

fn ts() -> Rfc3339Timestamp {
    Rfc3339Timestamp::parse("2026-05-11T12:00:00Z").expect("valid test timestamp")
}

fn enabled_config() -> LocalSensorConfig {
    LocalSensorConfig {
        terminal: SensorSettings::enabled(),
        clipboard: SensorSettings::enabled(),
        ..LocalSensorConfig::default()
    }
}

fn emitted(outcome: EmitOutcome) -> CaptureEvent {
    match outcome {
        EmitOutcome::Emitted(event) => event,
        EmitOutcome::Dropped { sensor, reason } => {
            panic!("expected emitted event, got drop from {sensor:?}: {reason:?}")
        }
    }
}

fn hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[test]
fn terminal_sensor_redacts_before_hashing_and_emits_valid_event() {
    let observation = TerminalObservation {
        event_id: id("01ARZ3NDEKTSV4RRFFQ69G5FAY"),
        captured_at: ts(),
        command: "run TOKEN=secret".to_owned(),
        exit_code: Some(0),
        context: Some(TerminalContext::InteractiveTty),
        output: "PASSWORD=hunter2".to_owned(),
        refs: None,
    };

    let event = emitted(terminal::emit(&enabled_config(), observation));

    assert_eq!(event.sensor_id.as_str(), "snr:local:terminal:default:v1");
    assert_eq!(event.source_family, SourceFamily::Terminal);
    assert_eq!(
        event.payload_hash.as_str(),
        hash(b"run TOKEN=[REDACTED]\nPASSWORD=[REDACTED]")
    );
    match &event.payload {
        CapturePayload::Terminal {
            command,
            exit_code,
            context,
        } => {
            assert_eq!(command, "run TOKEN=[REDACTED]");
            assert_eq!(*exit_code, Some(0));
            assert_eq!(*context, Some(TerminalContext::InteractiveTty));
        }
        other => panic!("unexpected payload: {other:?}"),
    }
    event.validate_for_capture().expect("valid event");
}

#[test]
fn terminal_sensor_drops_missing_context() {
    let observation = TerminalObservation {
        event_id: id("01ARZ3NDEKTSV4RRFFQ69G5FAZ"),
        captured_at: ts(),
        command: "cargo test".to_owned(),
        exit_code: None,
        context: None,
        output: String::new(),
        refs: None,
    };

    let outcome = terminal::emit(&enabled_config(), observation);

    assert!(matches!(
        outcome,
        EmitOutcome::Dropped {
            sensor: SensorKind::Terminal,
            reason: DropReason::MalformedObservation(_)
        }
    ));
}

#[test]
fn terminal_sensor_drops_private_key_output() {
    let observation = TerminalObservation {
        event_id: id("01ARZ3NDEKTSV4RRFFQ69G5FB0"),
        captured_at: ts(),
        command: "cat key.pem".to_owned(),
        exit_code: Some(0),
        context: Some(TerminalContext::InteractiveTty),
        output: "-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----".to_owned(),
        refs: None,
    };

    let outcome = terminal::emit(&enabled_config(), observation);

    assert_eq!(
        outcome,
        EmitOutcome::Dropped {
            sensor: SensorKind::Terminal,
            reason: DropReason::PolicyRejected("private key block".to_owned())
        }
    );
}

#[test]
fn terminal_budget_drops_before_event_creation() {
    let mut config = enabled_config();
    config.terminal.budget = CaptureBudget {
        max_items: Some(1),
        max_bytes: Some(4),
    };
    let observation = TerminalObservation {
        event_id: id("01ARZ3NDEKTSV4RRFFQ69G5FB1"),
        captured_at: ts(),
        command: "12345".to_owned(),
        exit_code: None,
        context: Some(TerminalContext::NonInteractiveOrStructured),
        output: String::new(),
        refs: None,
    };

    let outcome = terminal::emit(&config, observation);

    assert_eq!(
        outcome,
        EmitOutcome::Dropped {
            sensor: SensorKind::Terminal,
            reason: DropReason::BudgetExceeded
        }
    );
}

#[test]
fn clipboard_text_redacts_before_hashing_and_emits_valid_event() {
    let observation = ClipboardObservation {
        event_id: id("01ARZ3NDEKTSV4RRFFQ69G5FB2"),
        captured_at: ts(),
        mime_type: "text/plain".to_owned(),
        bytes: b"API_KEY=secret".to_vec(),
        metadata_only: false,
        refs: None,
    };

    let event = emitted(clipboard::emit(&enabled_config(), observation));

    assert_eq!(event.sensor_id.as_str(), "snr:local:clipboard:default:v1");
    assert_eq!(event.source_family, SourceFamily::Clipboard);
    assert_eq!(event.payload_hash.as_str(), hash(b"API_KEY=[REDACTED]"));
    match &event.payload {
        CapturePayload::Clipboard {
            mime_type,
            byte_len,
        } => {
            assert_eq!(mime_type, "text/plain");
            assert_eq!(*byte_len, 18);
        }
        other => panic!("unexpected payload: {other:?}"),
    }
    event.validate_for_capture().expect("valid event");
}

#[test]
fn clipboard_metadata_only_non_text_reports_original_byte_len_and_hashes_empty_payload() {
    let observation = ClipboardObservation {
        event_id: id("01ARZ3NDEKTSV4RRFFQ69G5FB3"),
        captured_at: ts(),
        mime_type: "image/png".to_owned(),
        bytes: vec![1, 2, 3, 4, 5],
        metadata_only: true,
        refs: None,
    };

    let event = emitted(clipboard::emit(&enabled_config(), observation));

    assert_eq!(event.sensor_id.as_str(), "snr:local:clipboard:default:v1");
    assert_eq!(event.source_family, SourceFamily::Clipboard);
    assert_eq!(event.payload_hash.as_str(), hash(b""));
    match &event.payload {
        CapturePayload::Clipboard {
            mime_type,
            byte_len,
        } => {
            assert_eq!(mime_type, "image/png");
            assert_eq!(*byte_len, 5);
        }
        other => panic!("unexpected payload: {other:?}"),
    }
    event.validate_for_capture().expect("valid event");
}

#[test]
fn clipboard_drops_unsupported_mime_without_metadata_only() {
    let observation = ClipboardObservation {
        event_id: id("01ARZ3NDEKTSV4RRFFQ69G5FB4"),
        captured_at: ts(),
        mime_type: "application/octet-stream".to_owned(),
        bytes: vec![1, 2, 3],
        metadata_only: false,
        refs: None,
    };

    let outcome = clipboard::emit(&enabled_config(), observation);

    assert_eq!(
        outcome,
        EmitOutcome::Dropped {
            sensor: SensorKind::Clipboard,
            reason: DropReason::PolicyRejected("unsupported clipboard MIME type".to_owned())
        }
    );
}
