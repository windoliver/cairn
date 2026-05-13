//! Optional production voice runtime for microphone capture and local ASR.
//!
//! This module is compiled only with the `voice-runtime` feature. The public
//! constructors validate configuration but do not open a microphone or load an
//! ASR model until callers explicitly construct/call them behind
//! `voice.enabled`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cairn_core::domain::{CaptureEventId, CaptureRefs, Rfc3339Timestamp};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SizedSample};
use sherpa_onnx::{OfflineRecognizer, OfflineRecognizerConfig, OfflineSenseVoiceModelConfig};

use crate::voice::{
    VoiceAudioChunk, VoiceAudioSource, VoiceDeviceMetadata, VoiceTranscriber, VoiceTranscript,
};

type SharedSamples = Arc<Mutex<Vec<f32>>>;
type SharedStreamError = Arc<Mutex<Option<String>>>;

/// Runtime configuration for a default-input-device cpal capture source.
#[derive(Debug, Clone, PartialEq)]
pub struct CpalVoiceSourceConfig {
    /// Capture event id assigned by the caller.
    pub event_id: CaptureEventId,
    /// Wall-clock timestamp when the chunk should be recorded as captured.
    pub captured_at: Rfc3339Timestamp,
    /// Wall-clock timestamp when the utterance window started.
    pub started_at: Rfc3339Timestamp,
    /// Amount of time to record from the default input device.
    pub capture_duration: Duration,
    /// Optional session, turn, and tool references.
    pub refs: Option<CaptureRefs>,
}

impl CpalVoiceSourceConfig {
    /// Build a default four-second capture configuration.
    #[must_use]
    pub fn new(
        event_id: CaptureEventId,
        started_at: Rfc3339Timestamp,
        captured_at: Rfc3339Timestamp,
    ) -> Self {
        Self {
            event_id,
            captured_at,
            started_at,
            capture_duration: Duration::from_secs(4),
            refs: None,
        }
    }

    /// Validate the cpal source configuration without touching audio hardware.
    pub fn validate(&self) -> Result<(), String> {
        if self.capture_duration.is_zero() {
            return Err("capture_duration must be greater than zero".to_owned());
        }

        Ok(())
    }
}

/// cpal-backed voice source that records one chunk from the default input.
#[derive(Debug, Clone)]
pub struct CpalVoiceSource {
    config: CpalVoiceSourceConfig,
}

impl CpalVoiceSource {
    /// Validate and create a cpal source without opening the microphone.
    pub fn new(config: CpalVoiceSourceConfig) -> Result<Self, String> {
        config.validate()?;

        Ok(Self { config })
    }
}

impl VoiceAudioSource for CpalVoiceSource {
    fn next_chunk(&mut self) -> Result<Option<VoiceAudioChunk>, String> {
        capture_cpal_chunk(&self.config).map(Some)
    }
}

/// Runtime configuration for a sherpa-onnx `SenseVoice` offline transcriber.
#[derive(Debug, Clone, PartialEq)]
pub struct SherpaOnnxTranscriberConfig {
    /// Path to the `SenseVoice` ONNX model file.
    pub model: PathBuf,
    /// Path to the model tokens file.
    pub tokens: PathBuf,
    /// `SenseVoice` language code. Use `auto` for automatic language detection.
    pub language: String,
    /// Whether sherpa should apply inverse text normalization.
    pub use_itn: bool,
    /// Number of CPU threads for the local recognizer.
    pub num_threads: i32,
    /// Optional sherpa provider, such as `cpu`.
    pub provider: Option<String>,
    /// Speaker label to attach to emitted transcripts.
    pub speaker_id: String,
    /// Confidence value to attach when the offline backend does not expose one.
    pub default_confidence: f32,
}

impl SherpaOnnxTranscriberConfig {
    /// Build a default `SenseVoice` transcriber configuration.
    #[must_use]
    pub fn sense_voice(model: impl Into<PathBuf>, tokens: impl Into<PathBuf>) -> Self {
        Self {
            model: model.into(),
            tokens: tokens.into(),
            language: "auto".to_owned(),
            use_itn: true,
            num_threads: 1,
            provider: Some("cpu".to_owned()),
            speaker_id: "unknown_speaker_01".to_owned(),
            default_confidence: 1.0,
        }
    }

    /// Validate the sherpa configuration without loading the model runtime.
    pub fn validate(&self) -> Result<(), String> {
        validate_regular_file(&self.model, "model")?;
        validate_regular_file(&self.tokens, "tokens")?;
        path_string(&self.model, "model")?;
        path_string(&self.tokens, "tokens")?;

        if self.language.trim().is_empty() {
            return Err("language must not be empty".to_owned());
        }
        if self.num_threads <= 0 {
            return Err("num_threads must be greater than zero".to_owned());
        }
        if self.speaker_id.trim().is_empty() {
            return Err("speaker_id must not be empty".to_owned());
        }
        if !(0.0..=1.0).contains(&self.default_confidence) {
            return Err("default_confidence must be in [0.0, 1.0]".to_owned());
        }

        Ok(())
    }

    fn recognizer_config(&self) -> Result<OfflineRecognizerConfig, String> {
        self.validate()?;

        let mut config = OfflineRecognizerConfig::default();
        config.model_config.sense_voice = OfflineSenseVoiceModelConfig {
            model: Some(path_string(&self.model, "model")?),
            language: Some(self.language.clone()),
            use_itn: self.use_itn,
        };
        config.model_config.tokens = Some(path_string(&self.tokens, "tokens")?);
        config.model_config.num_threads = self.num_threads;
        config.model_config.provider.clone_from(&self.provider);

        Ok(config)
    }
}

/// sherpa-onnx offline ASR transcriber.
pub struct SherpaOnnxTranscriber {
    recognizer: OfflineRecognizer,
    speaker_id: String,
    default_confidence: f32,
}

impl SherpaOnnxTranscriber {
    /// Validate config and load the sherpa-onnx recognizer.
    pub fn from_config(config: SherpaOnnxTranscriberConfig) -> Result<Self, String> {
        let recognizer_config = config.recognizer_config()?;
        let recognizer = OfflineRecognizer::create(&recognizer_config)
            .ok_or_else(|| "failed to create sherpa-onnx offline recognizer".to_owned())?;

        Ok(Self {
            recognizer,
            speaker_id: config.speaker_id,
            default_confidence: config.default_confidence,
        })
    }
}

impl VoiceTranscriber for SherpaOnnxTranscriber {
    fn transcribe(&self, chunk: &VoiceAudioChunk) -> Result<VoiceTranscript, String> {
        let samples = mono_samples(&chunk.samples, chunk.device.channels)?;
        if samples.is_empty() {
            return Err("voice audio chunk must contain samples".to_owned());
        }

        let sample_rate = i32::try_from(chunk.device.sample_rate_hz)
            .map_err(|_| "voice sample_rate_hz is too large for sherpa-onnx".to_owned())?;
        let stream = self.recognizer.create_stream();
        stream.accept_waveform(sample_rate, &samples);
        self.recognizer.decode(&stream);

        let result = stream
            .get_result()
            .ok_or_else(|| "sherpa-onnx did not return a transcript result".to_owned())?;
        let text = result.text.trim().to_owned();
        if text.is_empty() {
            return Err("sherpa-onnx returned an empty transcript".to_owned());
        }

        Ok(VoiceTranscript {
            speaker_id: self.speaker_id.clone(),
            text,
            confidence: self.default_confidence,
        })
    }
}

fn capture_cpal_chunk(config: &CpalVoiceSourceConfig) -> Result<VoiceAudioChunk, String> {
    config.validate()?;

    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| "no default input device is available".to_owned())?;
    let device_name = device.description().map_or_else(
        |_| "unknown input device".to_owned(),
        |description| description.name().to_owned(),
    );
    let supported_config = device
        .default_input_config()
        .map_err(|err| format!("failed to read default input config: {err}"))?;

    let sample_format = supported_config.sample_format();
    let sample_rate_hz = supported_config.sample_rate();
    let channels = supported_config.channels();
    let stream_config: cpal::StreamConfig = supported_config.into();
    let samples = SharedSamples::default();
    let stream_error = SharedStreamError::default();

    let stream = match sample_format {
        SampleFormat::F32 => build_input_stream::<f32, _>(
            &device,
            &stream_config,
            &samples,
            &stream_error,
            |sample| sample,
        ),
        SampleFormat::I16 => build_input_stream::<i16, _>(
            &device,
            &stream_config,
            &samples,
            &stream_error,
            |sample| f32::from(sample) / f32::from(i16::MAX),
        ),
        SampleFormat::U16 => build_input_stream::<u16, _>(
            &device,
            &stream_config,
            &samples,
            &stream_error,
            |sample| (f32::from(sample) - 32_768.0) / 32_768.0,
        ),
        other => Err(format!(
            "unsupported default input sample format {other:?}; supported formats are f32, i16, and u16"
        )),
    }?;

    stream
        .play()
        .map_err(|err| format!("failed to start default input stream: {err}"))?;
    std::thread::sleep(config.capture_duration);
    drop(stream);

    if let Some(err) = take_stream_error(&stream_error)? {
        return Err(format!("default input stream failed: {err}"));
    }

    let captured_samples = samples
        .lock()
        .map_err(|_| "voice sample buffer lock was poisoned".to_owned())?
        .clone();
    if captured_samples.is_empty() {
        return Err("default input stream produced no samples".to_owned());
    }

    Ok(VoiceAudioChunk {
        event_id: config.event_id.clone(),
        captured_at: config.captured_at.clone(),
        started_at: config.started_at.clone(),
        duration_ms: millis_u64(config.capture_duration)?,
        samples: captured_samples,
        device: VoiceDeviceMetadata {
            name: device_name,
            host: host.id().to_string(),
            sample_rate_hz,
            channels,
        },
        refs: config.refs.clone(),
    })
}

fn build_input_stream<T, Convert>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    samples: &SharedSamples,
    stream_error: &SharedStreamError,
    convert: Convert,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + Copy + 'static,
    Convert: Fn(T) -> f32 + Send + Sync + Copy + 'static,
{
    let samples_for_callback = Arc::clone(samples);
    let errors_for_callback = Arc::clone(stream_error);

    device
        .build_input_stream::<T, _, _>(
            config,
            move |data, _| push_samples(data, &samples_for_callback, convert),
            move |err| set_stream_error(&errors_for_callback, err.to_string()),
            None,
        )
        .map_err(|err| format!("failed to build default input stream: {err}"))
}

fn push_samples<T, Convert>(data: &[T], samples: &SharedSamples, convert: Convert)
where
    T: Copy,
    Convert: Fn(T) -> f32,
{
    if let Ok(mut guard) = samples.lock() {
        guard.extend(data.iter().copied().map(convert));
    }
}

fn set_stream_error(stream_error: &SharedStreamError, error: String) {
    if let Ok(mut guard) = stream_error.lock() {
        *guard = Some(error);
    }
}

fn take_stream_error(stream_error: &SharedStreamError) -> Result<Option<String>, String> {
    stream_error
        .lock()
        .map_err(|_| "voice stream error lock was poisoned".to_owned())
        .map(|mut guard| guard.take())
}

fn mono_samples(samples: &[f32], channels: u16) -> Result<Vec<f32>, String> {
    if channels == 0 {
        return Err("voice channels must be greater than zero".to_owned());
    }

    if channels == 1 {
        return Ok(samples.to_vec());
    }

    let channel_count = usize::from(channels);
    if !samples.len().is_multiple_of(channel_count) {
        return Err("interleaved voice samples must contain complete frames".to_owned());
    }

    let divisor = f32::from(channels);
    Ok(samples
        .chunks_exact(channel_count)
        .map(|frame| frame.iter().sum::<f32>() / divisor)
        .collect())
}

fn millis_u64(duration: Duration) -> Result<u64, String> {
    u64::try_from(duration.as_millis())
        .map_err(|_| "capture_duration is too large to encode as milliseconds".to_owned())
}

fn validate_regular_file(path: &Path, label: &str) -> Result<(), String> {
    if !path.is_file() {
        return Err(format!("{label} file does not exist: {}", path.display()));
    }

    Ok(())
}

fn path_string(path: &Path, label: &str) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{label} path must be valid UTF-8: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::mono_samples;

    #[test]
    fn mono_samples_passes_through_single_channel_audio() {
        let samples = mono_samples(&[0.5, -0.25], 1).expect("mono samples");

        assert_eq!(samples, vec![0.5, -0.25]);
    }

    #[test]
    fn mono_samples_downmixes_interleaved_stereo_frames() {
        let samples = mono_samples(&[0.0, 1.0, -0.5, 0.25], 2).expect("stereo samples");

        assert_eq!(samples, vec![0.5, -0.125]);
    }

    #[test]
    fn mono_samples_rejects_incomplete_interleaved_frames() {
        let error = mono_samples(&[0.0, 1.0, 0.5], 2).expect_err("incomplete frame");

        assert!(error.contains("complete frames"));
    }
}
