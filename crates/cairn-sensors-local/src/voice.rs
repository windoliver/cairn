//! Voice sensor emission.
//!
//! The production microphone and ASR implementations sit behind
//! [`VoiceAudioSource`] and [`VoiceTranscriber`]. Tests use the same
//! boundaries with mocked audio chunks, which keeps microphone access
//! behind explicit source enablement and avoids invoking ASR unless the
//! caller opted into voice capture.

use cairn_core::domain::{
    CaptureEventId, CapturePayload, CaptureRefs, Rfc3339Timestamp, SourceFamily,
};
use serde_json::json;

use crate::event::build_auto_event;
use crate::policy::{PolicyAction, sanitize_text_payload};
use crate::{DropReason, EmitOutcome, LocalSensorConfig, SensorKind};

const SENSOR_LABEL: &str = "local:voice:default:v1";

/// Captured audio device provenance for one voice chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceDeviceMetadata {
    /// Human-readable input device name.
    pub name: String,
    /// Audio host backend name, such as `coreaudio`, `alsa`, or `wasapi`.
    pub host: String,
    /// Input sample rate in hertz.
    pub sample_rate_hz: u32,
    /// Number of input channels in the captured stream.
    pub channels: u16,
}

/// One VAD-gated voice chunk ready for transcription.
#[derive(Debug, Clone, PartialEq)]
pub struct VoiceAudioChunk {
    /// Capture event id assigned by the caller.
    pub event_id: CaptureEventId,
    /// Wall-clock timestamp when the chunk was captured.
    pub captured_at: Rfc3339Timestamp,
    /// Wall-clock timestamp when the utterance started.
    pub started_at: Rfc3339Timestamp,
    /// Number of milliseconds the chunk spans.
    pub duration_ms: u64,
    /// Mono or interleaved floating-point PCM samples normalized to `[-1, 1]`.
    pub samples: Vec<f32>,
    /// Input device metadata.
    pub device: VoiceDeviceMetadata,
    /// Optional session, turn, and tool references.
    pub refs: Option<CaptureRefs>,
}

/// Transcript produced from a [`VoiceAudioChunk`].
#[derive(Debug, Clone, PartialEq)]
pub struct VoiceTranscript {
    /// Speaker label assigned by diarization or enrollment.
    pub speaker_id: String,
    /// Transcribed text.
    pub text: String,
    /// ASR confidence in `[0.0, 1.0]`.
    pub confidence: f32,
}

/// Sanitized voice observation ready for capture event construction.
#[derive(Debug, Clone, PartialEq)]
pub struct VoiceObservation {
    /// Capture event id assigned by the caller.
    pub event_id: CaptureEventId,
    /// Wall-clock timestamp when the chunk was captured.
    pub captured_at: Rfc3339Timestamp,
    /// Wall-clock timestamp when the utterance started.
    pub started_at: Rfc3339Timestamp,
    /// Number of milliseconds the utterance spans.
    pub duration_ms: u64,
    /// Number of PCM samples in the source audio chunk.
    pub sample_count: usize,
    /// Source audio byte count used for source-side budget enforcement.
    pub audio_byte_len: usize,
    /// Input device metadata.
    pub device: VoiceDeviceMetadata,
    /// Transcript emitted by the ASR backend.
    pub transcript: VoiceTranscript,
    /// Optional session, turn, and tool references.
    pub refs: Option<CaptureRefs>,
}

impl VoiceObservation {
    fn from_chunk(chunk: VoiceAudioChunk, transcript: VoiceTranscript) -> Self {
        let audio_byte_len = chunk
            .samples
            .len()
            .saturating_mul(std::mem::size_of::<f32>());
        Self {
            event_id: chunk.event_id,
            captured_at: chunk.captured_at,
            started_at: chunk.started_at,
            duration_ms: chunk.duration_ms,
            sample_count: chunk.samples.len(),
            audio_byte_len,
            device: chunk.device,
            transcript,
            refs: chunk.refs,
        }
    }
}

/// Source of VAD-gated audio chunks.
pub trait VoiceAudioSource {
    /// Return the next available audio chunk, or `None` when no chunk is ready.
    ///
    /// Implementations are expected to start microphone access only when this
    /// method is called. [`capture_next_chunk`] checks `config.voice.enabled`
    /// before invoking it.
    fn next_chunk(&mut self) -> Result<Option<VoiceAudioChunk>, String>;
}

/// ASR backend for one voice chunk.
pub trait VoiceTranscriber {
    /// Transcribe the supplied audio chunk.
    fn transcribe(&self, chunk: &VoiceAudioChunk) -> Result<VoiceTranscript, String>;
}

/// Capture and transcribe one audio chunk when voice capture is enabled.
#[must_use]
pub fn capture_next_chunk<S, T>(
    config: &LocalSensorConfig,
    source: &mut S,
    transcriber: &T,
) -> Option<EmitOutcome>
where
    S: VoiceAudioSource,
    T: VoiceTranscriber,
{
    if !config.voice.enabled {
        return Some(EmitOutcome::Dropped {
            sensor: SensorKind::Voice,
            reason: DropReason::Disabled,
        });
    }

    let chunk = match source.next_chunk() {
        Ok(Some(chunk)) => chunk,
        Ok(None) => return None,
        Err(err) => {
            return Some(EmitOutcome::Dropped {
                sensor: SensorKind::Voice,
                reason: DropReason::MalformedObservation(format!(
                    "voice audio capture failed: {err}"
                )),
            });
        }
    };

    let transcript = match transcriber.transcribe(&chunk) {
        Ok(transcript) => transcript,
        Err(err) => {
            return Some(EmitOutcome::Dropped {
                sensor: SensorKind::Voice,
                reason: DropReason::MalformedObservation(format!(
                    "voice transcription failed: {err}"
                )),
            });
        }
    };

    Some(emit(
        config,
        VoiceObservation::from_chunk(chunk, transcript),
    ))
}

/// Lazily construct the voice source and transcriber, then capture one chunk.
///
/// This keeps microphone/device access and ASR model loading behind the same
/// `config.voice.enabled` gate as [`capture_next_chunk`].
#[must_use]
pub fn capture_next_chunk_lazy<S, T, SourceFactory, TranscriberFactory>(
    config: &LocalSensorConfig,
    source_factory: SourceFactory,
    transcriber_factory: TranscriberFactory,
) -> Option<EmitOutcome>
where
    S: VoiceAudioSource,
    T: VoiceTranscriber,
    SourceFactory: FnOnce() -> Result<S, String>,
    TranscriberFactory: FnOnce() -> Result<T, String>,
{
    if !config.voice.enabled {
        return Some(EmitOutcome::Dropped {
            sensor: SensorKind::Voice,
            reason: DropReason::Disabled,
        });
    }

    let mut source = match source_factory() {
        Ok(source) => source,
        Err(err) => {
            return Some(EmitOutcome::Dropped {
                sensor: SensorKind::Voice,
                reason: DropReason::MalformedObservation(format!(
                    "voice audio source initialization failed: {err}"
                )),
            });
        }
    };

    let transcriber = match transcriber_factory() {
        Ok(transcriber) => transcriber,
        Err(err) => {
            return Some(EmitOutcome::Dropped {
                sensor: SensorKind::Voice,
                reason: DropReason::MalformedObservation(format!(
                    "voice transcriber initialization failed: {err}"
                )),
            });
        }
    };

    capture_next_chunk(config, &mut source, &transcriber)
}

/// Emit one transcribed voice observation as a validated capture event.
#[must_use]
pub fn emit(config: &LocalSensorConfig, observation: VoiceObservation) -> EmitOutcome {
    if !config.voice.enabled {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Voice,
            reason: DropReason::Disabled,
        };
    }

    let raw_len = observation
        .audio_byte_len
        .saturating_add(observation.transcript.text.len())
        .saturating_add(observation.transcript.speaker_id.len())
        .saturating_add(observation.device.name.len())
        .saturating_add(observation.device.host.len());
    if !config.voice.budget.allows(1, raw_len) {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Voice,
            reason: DropReason::BudgetExceeded,
        };
    }

    if let Some(reason) = malformed_reason(&observation) {
        return EmitOutcome::Dropped {
            sensor: SensorKind::Voice,
            reason: DropReason::MalformedObservation(reason),
        };
    }

    let text = match sanitize_text_payload(&observation.transcript.text) {
        PolicyAction::Sanitized(text) => text,
        PolicyAction::Rejected(reason) => {
            return EmitOutcome::Dropped {
                sensor: SensorKind::Voice,
                reason: DropReason::PolicyRejected(reason),
            };
        }
    };

    let sanitized_payload = match raw_payload_bytes(&observation, &text) {
        Ok(bytes) => bytes,
        Err(err) => {
            return EmitOutcome::Dropped {
                sensor: SensorKind::Voice,
                reason: DropReason::MalformedObservation(format!(
                    "voice payload serialization failed: {err}"
                )),
            };
        }
    };
    let payload = CapturePayload::Voice {
        speaker_id: observation.transcript.speaker_id,
        duration_ms: observation.duration_ms,
        confidence: observation.transcript.confidence,
    };

    match build_auto_event(
        observation.event_id,
        observation.captured_at,
        SENSOR_LABEL,
        payload,
        SourceFamily::Voice,
        observation.refs,
        &sanitized_payload,
    ) {
        Ok(event) => EmitOutcome::Emitted(event),
        Err(err) => EmitOutcome::Dropped {
            sensor: SensorKind::Voice,
            reason: DropReason::MalformedObservation(err.to_string()),
        },
    }
}

fn malformed_reason(observation: &VoiceObservation) -> Option<String> {
    if observation.duration_ms == 0 {
        return Some("voice duration_ms must be > 0".to_owned());
    }
    if observation.sample_count == 0 {
        return Some("voice audio chunk must contain samples".to_owned());
    }
    if observation.device.name.trim().is_empty() {
        return Some("voice device name is required".to_owned());
    }
    if observation.device.host.trim().is_empty() {
        return Some("voice device host is required".to_owned());
    }
    if observation.device.sample_rate_hz == 0 {
        return Some("voice sample_rate_hz must be > 0".to_owned());
    }
    if observation.device.channels == 0 {
        return Some("voice channels must be > 0".to_owned());
    }
    if observation.transcript.text.trim().is_empty() {
        return Some("voice transcript text is required".to_owned());
    }
    None
}

fn raw_payload_bytes(
    observation: &VoiceObservation,
    sanitized_text: &str,
) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&json!({
        "device": {
            "channels": observation.device.channels,
            "host": observation.device.host,
            "name": observation.device.name,
            "sample_rate_hz": observation.device.sample_rate_hz,
        },
        "timing": {
            "captured_at": observation.captured_at.as_str(),
            "duration_ms": observation.duration_ms,
            "started_at": observation.started_at.as_str(),
        },
        "transcript": {
            "speaker_id": observation.transcript.speaker_id,
            "text": sanitized_text,
        },
    }))
}
