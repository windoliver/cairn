#![allow(missing_docs)]
#![cfg(feature = "voice-runtime")]

use std::env;
use std::path::PathBuf;

use cairn_core::domain::{CaptureEventId, Rfc3339Timestamp};
use cairn_sensors_local::voice::{VoiceAudioChunk, VoiceDeviceMetadata, VoiceTranscriber};
use cairn_sensors_local::voice_runtime::{SherpaOnnxTranscriber, SherpaOnnxTranscriberConfig};
use sherpa_onnx::Wave;

fn id(raw: &str) -> CaptureEventId {
    CaptureEventId::parse(raw).expect("valid test ULID")
}

fn ts(raw: &str) -> Rfc3339Timestamp {
    Rfc3339Timestamp::parse(raw).expect("valid test timestamp")
}

#[test]
#[ignore = "requires CAIRN_VOICE_FIXTURE_MODEL, CAIRN_VOICE_FIXTURE_TOKENS, and CAIRN_VOICE_FIXTURE_WAV"]
fn sherpa_runtime_transcribes_fixture_audio() {
    let model =
        PathBuf::from(env::var("CAIRN_VOICE_FIXTURE_MODEL").expect("CAIRN_VOICE_FIXTURE_MODEL"));
    let tokens =
        PathBuf::from(env::var("CAIRN_VOICE_FIXTURE_TOKENS").expect("CAIRN_VOICE_FIXTURE_TOKENS"));
    let wav = env::var("CAIRN_VOICE_FIXTURE_WAV").expect("CAIRN_VOICE_FIXTURE_WAV");
    let expected = env::var("CAIRN_VOICE_FIXTURE_EXPECTED_CONTAINS")
        .unwrap_or_else(|_| "tribal chieftain".to_owned());

    let wave = Wave::read(&wav).expect("fixture WAV loads through sherpa-onnx");
    let sample_rate_hz = u32::try_from(wave.sample_rate()).expect("positive sample rate");
    assert!(sample_rate_hz > 0);
    let duration_ms = u64::try_from(wave.samples().len())
        .expect("sample count fits u64")
        .saturating_mul(1_000)
        / u64::from(sample_rate_hz);

    let mut config = SherpaOnnxTranscriberConfig::sense_voice(model, tokens);
    config.language = "en".to_owned();
    config.use_itn = false;
    let transcriber =
        SherpaOnnxTranscriber::from_config(config).expect("sherpa-onnx recognizer loads");
    let transcript = transcriber
        .transcribe(&VoiceAudioChunk {
            event_id: id("01ARZ3NDEKTSV4RRFFQ69G5FC0"),
            captured_at: ts("2026-05-11T12:00:04Z"),
            started_at: ts("2026-05-11T12:00:00Z"),
            duration_ms,
            samples: wave.samples().to_vec(),
            device: VoiceDeviceMetadata {
                name: "fixture wav".to_owned(),
                host: "sherpa-onnx-wave".to_owned(),
                sample_rate_hz,
                channels: 1,
            },
            refs: None,
        })
        .expect("fixture transcribes");

    assert!(
        transcript
            .text
            .to_ascii_lowercase()
            .contains(&expected.to_ascii_lowercase()),
        "transcript did not contain {expected:?}: {:?}",
        transcript.text
    );
}
