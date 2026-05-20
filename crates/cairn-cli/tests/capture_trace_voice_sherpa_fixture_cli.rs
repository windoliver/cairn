#![allow(missing_docs)]
#![cfg(feature = "voice-runtime")]

use std::env;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, bail};
use cairn_core::domain::{CaptureEventId, CaptureRefs, Rfc3339Timestamp, SessionId};
use cairn_sensors_local::voice::{
    VoiceAudioChunk, VoiceAudioSource, VoiceDeviceMetadata, VoiceTranscriber,
};
use cairn_sensors_local::voice_runtime::{SherpaOnnxTranscriber, SherpaOnnxTranscriberConfig};
use cairn_sensors_local::{EmitOutcome, LocalSensorConfig, SensorSettings, voice};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

const EVENT_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FDV";
const SESSION_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FEV";
const TURN_ID: &str = "turn-voice-sherpa-e2e";

#[derive(Debug, Clone)]
struct PcmFixture {
    sample_rate_hz: u32,
    channels: u16,
    samples: Vec<f32>,
}

fn cli(config_home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
    cmd.env_remove("CAIRN_VAULT");
    cmd.env_remove("CAIRN_REGISTRY");
    cmd.env("XDG_CONFIG_HOME", config_home);
    cmd
}

fn fresh_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp vault");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: dir.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    dir
}

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

fn emitted(outcome: EmitOutcome) -> cairn_core::domain::CaptureEvent {
    match outcome {
        EmitOutcome::Emitted(event) => event,
        EmitOutcome::Dropped { sensor, reason } => {
            panic!("expected emitted event, got drop from {sensor:?}: {reason:?}")
        }
    }
}

fn duration_ms(fixture: &PcmFixture) -> u64 {
    let frames = fixture.samples.len() / usize::from(fixture.channels);
    u64::try_from(frames)
        .expect("frame count fits u64")
        .saturating_mul(1_000)
        / u64::from(fixture.sample_rate_hz)
}

fn voice_chunk(fixture: &PcmFixture) -> VoiceAudioChunk {
    VoiceAudioChunk {
        event_id: id(EVENT_ID),
        captured_at: ts("2026-05-11T12:00:04Z"),
        started_at: ts("2026-05-11T12:00:00Z"),
        duration_ms: duration_ms(fixture),
        samples: fixture.samples.clone(),
        device: VoiceDeviceMetadata {
            name: "fixture wav".to_owned(),
            host: "sherpa-onnx-wave".to_owned(),
            sample_rate_hz: fixture.sample_rate_hz,
            channels: fixture.channels,
        },
        refs: Some(CaptureRefs {
            session_id: Some(SESSION_ID.to_owned()),
            turn_id: Some(TURN_ID.to_owned()),
            tool_id: None,
        }),
    }
}

fn voice_payload_bytes(chunk: &VoiceAudioChunk, transcript_text: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "device": {
            "channels": chunk.device.channels,
            "host": chunk.device.host,
            "name": chunk.device.name,
            "sample_rate_hz": chunk.device.sample_rate_hz,
        },
        "timing": {
            "captured_at": chunk.captured_at.as_str(),
            "duration_ms": chunk.duration_ms,
            "started_at": chunk.started_at.as_str(),
        },
        "transcript": {
            "speaker_id": "unknown_speaker_01",
            "text": transcript_text,
        },
    }))
    .expect("serialize voice source payload")
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

struct FixtureWaveSource {
    chunk: Option<VoiceAudioChunk>,
}

impl VoiceAudioSource for FixtureWaveSource {
    fn next_chunk(&mut self) -> Result<Option<VoiceAudioChunk>, String> {
        Ok(self.chunk.take())
    }
}

fn read_pcm16_wav(path: &Path) -> anyhow::Result<PcmFixture> {
    let bytes = std::fs::read(path).with_context(|| format!("read WAV {}", path.display()))?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        bail!("fixture is not a RIFF/WAVE file: {}", path.display());
    }

    let mut cursor = 12_usize;
    let mut sample_rate_hz = None;
    let mut channels = None;
    let mut bits_per_sample = None;
    let mut audio_format = None;
    let mut data = None;

    while cursor.saturating_add(8) <= bytes.len() {
        let id = &bytes[cursor..cursor + 4];
        let size = usize::try_from(u32::from_le_bytes([
            bytes[cursor + 4],
            bytes[cursor + 5],
            bytes[cursor + 6],
            bytes[cursor + 7],
        ]))
        .context("WAV chunk size fits usize")?;
        cursor += 8;
        let end = cursor
            .checked_add(size)
            .filter(|end| *end <= bytes.len())
            .with_context(|| format!("WAV chunk overruns file: {}", path.display()))?;

        match id {
            b"fmt " if size >= 16 => {
                audio_format = Some(u16::from_le_bytes([bytes[cursor], bytes[cursor + 1]]));
                channels = Some(u16::from_le_bytes([bytes[cursor + 2], bytes[cursor + 3]]));
                sample_rate_hz = Some(u32::from_le_bytes([
                    bytes[cursor + 4],
                    bytes[cursor + 5],
                    bytes[cursor + 6],
                    bytes[cursor + 7],
                ]));
                bits_per_sample =
                    Some(u16::from_le_bytes([bytes[cursor + 14], bytes[cursor + 15]]));
            }
            b"data" => data = Some(bytes[cursor..end].to_vec()),
            _ => {}
        }

        cursor = end + (size % 2);
    }

    if audio_format != Some(1) {
        bail!("fixture WAV must be PCM");
    }
    if bits_per_sample != Some(16) {
        bail!("fixture WAV must be 16-bit PCM");
    }
    let channels = channels.context("fixture WAV missing channel count")?;
    if channels == 0 {
        bail!("fixture WAV channel count must be greater than zero");
    }
    let sample_rate_hz = sample_rate_hz.context("fixture WAV missing sample rate")?;
    if sample_rate_hz == 0 {
        bail!("fixture WAV sample rate must be greater than zero");
    }
    let data = data.context("fixture WAV missing data chunk")?;
    if data.len() % 2 != 0 {
        bail!("fixture WAV data has a partial i16 sample");
    }

    let samples = data
        .chunks_exact(2)
        .map(|bytes| {
            let sample = i16::from_le_bytes([bytes[0], bytes[1]]);
            f32::from(sample) / f32::from(i16::MAX)
        })
        .collect();

    Ok(PcmFixture {
        sample_rate_hz,
        channels,
        samples,
    })
}

#[tokio::test]
#[ignore = "requires CAIRN_VOICE_FIXTURE_MODEL, CAIRN_VOICE_FIXTURE_TOKENS, and CAIRN_VOICE_FIXTURE_WAV"]
async fn capture_trace_cli_imports_real_sherpa_fixture_end_to_end() {
    let model =
        PathBuf::from(env::var("CAIRN_VOICE_FIXTURE_MODEL").expect("CAIRN_VOICE_FIXTURE_MODEL"));
    let tokens =
        PathBuf::from(env::var("CAIRN_VOICE_FIXTURE_TOKENS").expect("CAIRN_VOICE_FIXTURE_TOKENS"));
    let wav = PathBuf::from(env::var("CAIRN_VOICE_FIXTURE_WAV").expect("CAIRN_VOICE_FIXTURE_WAV"));
    let expected = env::var("CAIRN_VOICE_FIXTURE_EXPECTED_CONTAINS")
        .unwrap_or_else(|_| "tribal chieftain".to_owned());

    let fixture = read_pcm16_wav(&wav).expect("fixture WAV loads");
    let chunk = voice_chunk(&fixture);
    let mut transcriber_config = SherpaOnnxTranscriberConfig::sense_voice(model, tokens);
    transcriber_config.language = "en".to_owned();
    transcriber_config.use_itn = false;
    let transcriber =
        SherpaOnnxTranscriber::from_config(transcriber_config).expect("sherpa recognizer loads");
    let transcript = transcriber.transcribe(&chunk).expect("fixture transcribes");
    assert!(
        transcript
            .text
            .to_ascii_lowercase()
            .contains(&expected.to_ascii_lowercase()),
        "transcript did not contain {expected:?}: {:?}",
        transcript.text
    );

    let mut source = FixtureWaveSource {
        chunk: Some(chunk.clone()),
    };
    let event = emitted(
        voice::capture_next_chunk(&enabled_config(), &mut source, &transcriber)
            .expect("sherpa fixture should produce one event"),
    );
    let raw_payload = voice_payload_bytes(&chunk, &transcript.text);
    assert_eq!(
        event.payload_hash.as_str(),
        format!("sha256:{}", sha256_hex(&raw_payload)),
        "test source payload must match the sensor-emitted hash"
    );

    let vault = fresh_vault();
    let config_home = tempfile::tempdir().expect("temp config home");
    let source_path = vault.path().join(&event.payload_ref);
    std::fs::create_dir_all(source_path.parent().expect("source parent")).expect("sources dir");
    std::fs::write(&source_path, raw_payload).expect("write voice source payload");

    let jsonl_path = vault.path().join("voice-sherpa-trace.jsonl");
    let mut jsonl = std::fs::File::create(&jsonl_path).expect("create JSONL file");
    writeln!(
        jsonl,
        "{}",
        serde_json::to_string(&event).expect("serialize capture event")
    )
    .expect("write capture event JSONL");

    let out = cli(config_home.path())
        .args([
            "--vault",
            vault.path().to_str().expect("utf-8 vault path"),
            "capture_trace",
            "--from",
            jsonl_path.to_str().expect("utf-8 jsonl path"),
            "--json",
        ])
        .output()
        .expect("cairn capture_trace --json");
    assert!(
        out.status.success(),
        "capture_trace failed\nstderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );

    let envelope: Value = serde_json::from_slice(&out.stdout).expect("response JSON");
    assert_eq!(envelope["status"], "committed", "envelope: {envelope}");
    assert_eq!(
        envelope["data"]["failed_turns"].as_array().map(Vec::len),
        Some(0),
        "voice trace import must not fail any turn: {envelope}"
    );

    let store = cairn_store_sqlite::open(vault.path().join(".cairn/cairn.db"))
        .await
        .expect("open store");
    let session = SessionId::parse(SESSION_ID).expect("valid session id");
    store
        .with_tx(move |tx| {
            let rows = tx.list_trace_events(&session, TURN_ID)?;
            assert_eq!(rows.len(), 1, "expected one voice trace row");
            let row = &rows[0];
            assert_eq!(row.body, transcript.text);
            assert_eq!(
                row.extra_frontmatter["trace_event"].as_str(),
                Some("user_message")
            );
            assert_eq!(
                row.provenance.source_sensor.as_str(),
                "snr:local:voice:default:v1"
            );
            assert_eq!(
                row.extra_frontmatter["trace"]["payload_ref"].as_str(),
                Some(format!("sources/voice/{EVENT_ID}.json").as_str())
            );
            Ok(())
        })
        .await
        .expect("query voice trace rows");
}
