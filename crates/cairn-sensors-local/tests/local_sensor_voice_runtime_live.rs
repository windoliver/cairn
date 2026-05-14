#![allow(missing_docs)]
#![cfg(feature = "voice-runtime")]

use std::env;
use std::time::Duration;

use cairn_core::domain::{CaptureEventId, Rfc3339Timestamp};
use cairn_sensors_local::voice::VoiceAudioSource;
use cairn_sensors_local::voice_runtime::{CpalVoiceSource, CpalVoiceSourceConfig};

fn id(raw: &str) -> CaptureEventId {
    CaptureEventId::parse(raw).expect("valid test ULID")
}

fn ts(raw: &str) -> Rfc3339Timestamp {
    Rfc3339Timestamp::parse(raw).expect("valid test timestamp")
}

#[test]
#[ignore = "opens the default input microphone; set CAIRN_VOICE_RUN_LIVE_MIC=1"]
fn cpal_runtime_captures_default_input_when_explicitly_requested() {
    assert_eq!(
        env::var("CAIRN_VOICE_RUN_LIVE_MIC").as_deref(),
        Ok("1"),
        "set CAIRN_VOICE_RUN_LIVE_MIC=1 to open the microphone"
    );

    let mut source = CpalVoiceSource::new(CpalVoiceSourceConfig {
        event_id: id("01ARZ3NDEKTSV4RRFFQ69G5FC0"),
        captured_at: ts("2026-05-11T12:00:01Z"),
        started_at: ts("2026-05-11T12:00:00Z"),
        capture_duration: Duration::from_millis(250),
        refs: None,
    })
    .expect("cpal source config validates");

    let chunk = source
        .next_chunk()
        .expect("default input capture succeeds")
        .expect("default input capture returns one chunk");

    assert!(!chunk.samples.is_empty());
    assert!(chunk.device.sample_rate_hz > 0);
    assert!(chunk.device.channels > 0);
    assert_eq!(chunk.duration_ms, 250);
}
