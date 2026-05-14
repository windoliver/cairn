#![allow(missing_docs)]
#![cfg(feature = "voice-runtime")]

use std::time::Duration;

use cairn_core::domain::{CaptureEventId, Rfc3339Timestamp};
use cairn_sensors_local::voice_runtime::{CpalVoiceSourceConfig, SherpaOnnxTranscriberConfig};
use tempfile::tempdir;

fn id(raw: &str) -> CaptureEventId {
    CaptureEventId::parse(raw).expect("valid test ULID")
}

fn ts(raw: &str) -> Rfc3339Timestamp {
    Rfc3339Timestamp::parse(raw).expect("valid test timestamp")
}

#[test]
fn cpal_runtime_config_rejects_zero_capture_duration() {
    let config = CpalVoiceSourceConfig {
        event_id: id("01ARZ3NDEKTSV4RRFFQ69G5FC0"),
        captured_at: ts("2026-05-11T12:00:04Z"),
        started_at: ts("2026-05-11T12:00:00Z"),
        capture_duration: Duration::ZERO,
        refs: None,
    };

    let error = config.validate().expect_err("zero duration is invalid");

    assert!(error.contains("capture_duration"));
}

#[test]
fn sherpa_runtime_config_validates_model_files_without_loading_runtime() {
    let dir = tempdir().expect("tempdir");
    let model = dir.path().join("model.int8.onnx");
    let tokens = dir.path().join("tokens.txt");
    std::fs::write(&model, b"fake onnx").expect("model fixture");
    std::fs::write(&tokens, b"fake tokens").expect("tokens fixture");

    let config = SherpaOnnxTranscriberConfig::sense_voice(model, tokens);

    config.validate().expect("existing files validate");
}

#[test]
fn sherpa_runtime_config_rejects_missing_model_files_before_loading_runtime() {
    let dir = tempdir().expect("tempdir");
    let model = dir.path().join("missing-model.onnx");
    let tokens = dir.path().join("missing-tokens.txt");

    let config = SherpaOnnxTranscriberConfig::sense_voice(model, tokens);
    let error = config.validate().expect_err("missing files are invalid");

    assert!(error.contains("model"));
}
