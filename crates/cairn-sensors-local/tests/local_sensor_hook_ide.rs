#![allow(missing_docs)]

use cairn_core::domain::{
    CaptureEvent, CaptureEventId, CapturePayload, CaptureRefs, Rfc3339Timestamp, SourceFamily,
};
use cairn_sensors_local::hook::{HookHarness, HookObservation};
use cairn_sensors_local::ide::IdeObservation;
use cairn_sensors_local::{
    CaptureBudget, DropReason, EmitOutcome, LocalSensorConfig, SensorKind, SensorSettings, hook,
    ide,
};

fn id(raw: &str) -> CaptureEventId {
    CaptureEventId::parse(raw).expect("valid test ULID")
}

fn ts() -> Rfc3339Timestamp {
    Rfc3339Timestamp::parse("2026-05-11T12:00:00Z").expect("valid test timestamp")
}

fn emitted(outcome: EmitOutcome) -> CaptureEvent {
    match outcome {
        EmitOutcome::Emitted(event) => event,
        EmitOutcome::Dropped { sensor, reason } => {
            panic!("expected emitted event, got drop from {sensor:?}: {reason:?}")
        }
    }
}

#[test]
fn enabled_hook_sensor_emits_valid_capture_event() {
    let observation = HookObservation {
        event_id: id("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
        captured_at: ts(),
        harness: HookHarness::ClaudeCode,
        hook_name: "UserPromptSubmit".to_owned(),
        tool_name: None,
        refs: Some(CaptureRefs {
            session_id: Some("session-1".to_owned()),
            turn_id: Some("turn-1".to_owned()),
            tool_id: None,
        }),
        raw_payload: br#"{"prompt":"remember this"}"#.to_vec(),
    };

    let event = emitted(hook::emit(&LocalSensorConfig::default(), observation));

    assert_eq!(event.sensor_id.as_str(), "snr:local:hook:cc-session:v1");
    assert_eq!(event.source_family, SourceFamily::Hook);
    match &event.payload {
        CapturePayload::Hook {
            hook_name,
            tool_name,
        } => {
            assert_eq!(hook_name, "UserPromptSubmit");
            assert_eq!(tool_name, &None);
        }
        other => panic!("unexpected payload: {other:?}"),
    }
    event.validate_for_capture().expect("valid event");
}

#[test]
fn hook_harnesses_emit_distinct_sensor_labels() {
    let cases = [
        (
            HookHarness::Codex,
            "01ARZ3NDEKTSV4RRFFQ69G5FB5",
            "snr:local:hook:codex-session:v1",
        ),
        (
            HookHarness::Gemini,
            "01ARZ3NDEKTSV4RRFFQ69G5FB6",
            "snr:local:hook:gemini-session:v1",
        ),
    ];

    for (harness, event_id, sensor_id) in cases {
        let observation = HookObservation {
            event_id: id(event_id),
            captured_at: ts(),
            harness,
            hook_name: "PostToolUse".to_owned(),
            tool_name: Some("shell".to_owned()),
            refs: None,
            raw_payload: br#"{"tool_name":"shell","status":"ok"}"#.to_vec(),
        };

        let event = emitted(hook::emit(&LocalSensorConfig::default(), observation));

        assert_eq!(event.sensor_id.as_str(), sensor_id);
        event.validate_for_capture().expect("valid event");
    }
}

#[test]
fn disabled_hook_sensor_drops_without_event() {
    let config = LocalSensorConfig {
        hooks: SensorSettings::disabled(),
        ..Default::default()
    };
    let observation = HookObservation {
        event_id: id("01ARZ3NDEKTSV4RRFFQ69G5FAW"),
        captured_at: ts(),
        harness: HookHarness::Codex,
        hook_name: "SessionStart".to_owned(),
        tool_name: None,
        refs: None,
        raw_payload: br#"{"session_id":"session-1"}"#.to_vec(),
    };

    let outcome = hook::emit(&config, observation);

    assert_eq!(
        outcome,
        EmitOutcome::Dropped {
            sensor: SensorKind::Hook,
            reason: DropReason::Disabled
        }
    );
}

#[test]
fn hook_budget_drops_before_event_creation() {
    let config = LocalSensorConfig {
        hooks: SensorSettings {
            enabled: true,
            budget: CaptureBudget {
                max_items: Some(1),
                max_bytes: Some(4),
            },
        },
        ..Default::default()
    };
    let observation = HookObservation {
        event_id: id("01ARZ3NDEKTSV4RRFFQ69G5FB7"),
        captured_at: ts(),
        harness: HookHarness::ClaudeCode,
        hook_name: String::new(),
        tool_name: None,
        refs: None,
        raw_payload: b"12345".to_vec(),
    };

    let outcome = hook::emit(&config, observation);

    assert_eq!(
        outcome,
        EmitOutcome::Dropped {
            sensor: SensorKind::Hook,
            reason: DropReason::BudgetExceeded
        }
    );
}

#[test]
fn enabled_ide_sensor_emits_valid_capture_event() {
    let observation = IdeObservation {
        event_id: id("01ARZ3NDEKTSV4RRFFQ69G5FAX"),
        captured_at: ts(),
        file_path: "crates/cairn-core/src/domain/capture.rs".to_owned(),
        event_kind: "diagnostic".to_owned(),
        refs: None,
        raw_payload: br#"{"diagnostics":1}"#.to_vec(),
    };

    let event = emitted(ide::emit(&LocalSensorConfig::default(), observation));

    assert_eq!(event.sensor_id.as_str(), "snr:local:ide:default:v1");
    assert_eq!(event.source_family, SourceFamily::Ide);
    match &event.payload {
        CapturePayload::Ide {
            file_path,
            event_kind,
        } => {
            assert_eq!(file_path, "crates/cairn-core/src/domain/capture.rs");
            assert_eq!(event_kind, "diagnostic");
        }
        other => panic!("unexpected payload: {other:?}"),
    }
    event.validate_for_capture().expect("valid event");
}

#[test]
fn disabled_ide_sensor_drops_without_event() {
    let config = LocalSensorConfig {
        ide: SensorSettings::disabled(),
        ..Default::default()
    };
    let observation = IdeObservation {
        event_id: id("01ARZ3NDEKTSV4RRFFQ69G5FB8"),
        captured_at: ts(),
        file_path: "crates/cairn-core/src/domain/capture.rs".to_owned(),
        event_kind: "diagnostic".to_owned(),
        refs: None,
        raw_payload: br#"{"diagnostics":1}"#.to_vec(),
    };

    let outcome = ide::emit(&config, observation);

    assert_eq!(
        outcome,
        EmitOutcome::Dropped {
            sensor: SensorKind::Ide,
            reason: DropReason::Disabled
        }
    );
}

#[test]
fn ide_budget_drops_before_event_creation() {
    let config = LocalSensorConfig {
        ide: SensorSettings {
            enabled: true,
            budget: CaptureBudget {
                max_items: Some(1),
                max_bytes: Some(4),
            },
        },
        ..Default::default()
    };
    let observation = IdeObservation {
        event_id: id("01ARZ3NDEKTSV4RRFFQ69G5FB9"),
        captured_at: ts(),
        file_path: String::new(),
        event_kind: "diagnostic".to_owned(),
        refs: None,
        raw_payload: b"12345".to_vec(),
    };

    let outcome = ide::emit(&config, observation);

    assert_eq!(
        outcome,
        EmitOutcome::Dropped {
            sensor: SensorKind::Ide,
            reason: DropReason::BudgetExceeded
        }
    );
}
