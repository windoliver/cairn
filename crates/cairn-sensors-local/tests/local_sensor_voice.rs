#![allow(missing_docs)]

use std::cell::Cell;

use cairn_core::domain::{
    CaptureEvent, CaptureEventId, CapturePayload, Rfc3339Timestamp, SourceFamily,
};
use cairn_core::pipeline::dispatch::{BypassReason, DefaultRegistry, DispatchDecision, dispatch};
use cairn_sensors_local::voice::{
    VoiceAudioChunk, VoiceAudioSource, VoiceDeviceMetadata, VoiceTranscriber, VoiceTranscript,
};
use cairn_sensors_local::{
    DropReason, EmitOutcome, LocalSensorConfig, SensorKind, SensorSettings, voice,
};
use sha2::{Digest as _, Sha256};

fn id(raw: &str) -> CaptureEventId {
    CaptureEventId::parse(raw).expect("valid test ULID")
}

fn ts(raw: &str) -> Rfc3339Timestamp {
    Rfc3339Timestamp::parse(raw).expect("valid test timestamp")
}

fn enabled_config() -> LocalSensorConfig {
    LocalSensorConfig {
        voice: SensorSettings::enabled(),
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

fn chunk() -> VoiceAudioChunk {
    VoiceAudioChunk {
        event_id: id("01ARZ3NDEKTSV4RRFFQ69G5FC0"),
        captured_at: ts("2026-05-11T12:00:04Z"),
        started_at: ts("2026-05-11T12:00:00Z"),
        duration_ms: 4_000,
        samples: vec![0.0, 0.2, -0.1, 0.0],
        device: VoiceDeviceMetadata {
            name: "Built-in Microphone".to_owned(),
            host: "coreaudio".to_owned(),
            sample_rate_hz: 16_000,
            channels: 1,
        },
        refs: None,
    }
}

struct OneChunkSource {
    calls: Cell<usize>,
}

impl VoiceAudioSource for OneChunkSource {
    fn next_chunk(&mut self) -> Result<Option<VoiceAudioChunk>, String> {
        self.calls.set(self.calls.get() + 1);
        Ok(Some(chunk()))
    }
}

struct NoChunkSource;

impl VoiceAudioSource for NoChunkSource {
    fn next_chunk(&mut self) -> Result<Option<VoiceAudioChunk>, String> {
        Ok(None)
    }
}

struct StaticTranscriber {
    calls: Cell<usize>,
}

impl VoiceTranscriber for StaticTranscriber {
    fn transcribe(&self, _chunk: &VoiceAudioChunk) -> Result<VoiceTranscript, String> {
        self.calls.set(self.calls.get() + 1);
        Ok(VoiceTranscript {
            speaker_id: "unknown_speaker_01".to_owned(),
            text: "deploy TOKEN=secret today".to_owned(),
            confidence: 0.875,
        })
    }
}

#[test]
fn local_sensor_config_disables_voice_by_default() {
    let config = LocalSensorConfig::default();

    assert!(!config.voice.enabled);
}

#[test]
fn disabled_voice_capture_does_not_start_audio_or_transcription() {
    let mut source = OneChunkSource {
        calls: Cell::new(0),
    };
    let transcriber = StaticTranscriber {
        calls: Cell::new(0),
    };

    let outcome =
        voice::capture_next_chunk(&LocalSensorConfig::default(), &mut source, &transcriber);

    assert_eq!(
        outcome,
        Some(EmitOutcome::Dropped {
            sensor: SensorKind::Voice,
            reason: DropReason::Disabled,
        })
    );
    assert_eq!(source.calls.get(), 0);
    assert_eq!(transcriber.calls.get(), 0);
}

#[test]
fn disabled_lazy_voice_capture_does_not_construct_runtime_dependencies() {
    let source_constructed = Cell::new(false);
    let transcriber_constructed = Cell::new(false);

    let outcome = voice::capture_next_chunk_lazy(
        &LocalSensorConfig::default(),
        || -> Result<OneChunkSource, String> {
            source_constructed.set(true);
            Ok(OneChunkSource {
                calls: Cell::new(0),
            })
        },
        || -> Result<StaticTranscriber, String> {
            transcriber_constructed.set(true);
            Ok(StaticTranscriber {
                calls: Cell::new(0),
            })
        },
    );

    assert_eq!(
        outcome,
        Some(EmitOutcome::Dropped {
            sensor: SensorKind::Voice,
            reason: DropReason::Disabled,
        })
    );
    assert!(!source_constructed.get());
    assert!(!transcriber_constructed.get());
}

#[test]
fn lazy_voice_capture_does_not_construct_transcriber_until_chunk_exists() {
    let transcriber_constructed = Cell::new(false);

    let outcome = voice::capture_next_chunk_lazy(
        &enabled_config(),
        || -> Result<NoChunkSource, String> { Ok(NoChunkSource) },
        || -> Result<StaticTranscriber, String> {
            transcriber_constructed.set(true);
            Ok(StaticTranscriber {
                calls: Cell::new(0),
            })
        },
    );

    assert_eq!(outcome, None);
    assert!(!transcriber_constructed.get());
}

#[test]
fn enabled_voice_capture_transcribes_chunk_into_valid_voice_event() {
    let mut source = OneChunkSource {
        calls: Cell::new(0),
    };
    let transcriber = StaticTranscriber {
        calls: Cell::new(0),
    };

    let outcome = voice::capture_next_chunk(&enabled_config(), &mut source, &transcriber)
        .expect("chunk produces an outcome");
    let event = emitted(outcome);

    assert_eq!(source.calls.get(), 1);
    assert_eq!(transcriber.calls.get(), 1);
    assert_eq!(event.sensor_id.as_str(), "snr:local:voice:default:v1");
    assert_eq!(event.source_family, SourceFamily::Voice);
    assert_eq!(
        event.payload_ref,
        "sources/voice/01ARZ3NDEKTSV4RRFFQ69G5FC0.json"
    );
    assert_eq!(
        event.payload_hash.as_str(),
        hash(
            br#"{"device":{"channels":1,"host":"coreaudio","name":"Built-in Microphone","sample_rate_hz":16000},"timing":{"captured_at":"2026-05-11T12:00:04Z","duration_ms":4000,"started_at":"2026-05-11T12:00:00Z"},"transcript":{"speaker_id":"unknown_speaker_01","text":"deploy TOKEN=[REDACTED] today"}}"#
        )
    );
    match &event.payload {
        CapturePayload::Voice {
            speaker_id,
            duration_ms,
            confidence,
        } => {
            assert_eq!(speaker_id, "unknown_speaker_01");
            assert_eq!(*duration_ms, 4_000);
            assert!((*confidence - 0.875_f32).abs() < f32::EPSILON);
        }
        other => panic!("unexpected payload: {other:?}"),
    }
    event.validate_for_capture().expect("valid event");
    assert_eq!(
        dispatch(&event, &DefaultRegistry),
        DispatchDecision::Bypass(BypassReason::NonTerminalFamily)
    );
}
