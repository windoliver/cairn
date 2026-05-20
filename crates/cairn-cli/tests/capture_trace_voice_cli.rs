#![allow(missing_docs)]

use std::io::Write as _;
use std::path::Path;
use std::process::Command;

use cairn_core::domain::{CaptureEvent, CaptureEventId, CaptureRefs, Rfc3339Timestamp, SessionId};
use cairn_sensors_local::voice::{
    VoiceAudioChunk, VoiceAudioSource, VoiceDeviceMetadata, VoiceTranscriber, VoiceTranscript,
};
use cairn_sensors_local::{EmitOutcome, LocalSensorConfig, SensorSettings, voice};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

const EVENT_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FCV";
const SESSION_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const TURN_ID: &str = "turn-voice-e2e";
const TRANSCRIPT_TEXT: &str = "review the launch checklist before the Friday demo";

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

fn emitted(outcome: EmitOutcome) -> CaptureEvent {
    match outcome {
        EmitOutcome::Emitted(event) => event,
        EmitOutcome::Dropped { sensor, reason } => {
            panic!("expected emitted event, got drop from {sensor:?}: {reason:?}")
        }
    }
}

fn voice_payload_bytes() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "device": {
            "channels": 1,
            "host": "coreaudio",
            "name": "Built-in Microphone",
            "sample_rate_hz": 16_000,
        },
        "timing": {
            "captured_at": "2026-05-11T12:00:04Z",
            "duration_ms": 4_000,
            "started_at": "2026-05-11T12:00:00Z",
        },
        "transcript": {
            "speaker_id": "unknown_speaker_voice_e2e",
            "text": TRANSCRIPT_TEXT,
        },
    }))
    .expect("serialize voice source payload")
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

struct OneChunkSource;

impl VoiceAudioSource for OneChunkSource {
    fn next_chunk(&mut self) -> Result<Option<VoiceAudioChunk>, String> {
        Ok(Some(VoiceAudioChunk {
            event_id: id(EVENT_ID),
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
            refs: Some(CaptureRefs {
                session_id: Some(SESSION_ID.to_owned()),
                turn_id: Some(TURN_ID.to_owned()),
                tool_id: None,
            }),
        }))
    }
}

struct StaticTranscriber;

impl VoiceTranscriber for StaticTranscriber {
    fn transcribe(&self, _chunk: &VoiceAudioChunk) -> Result<VoiceTranscript, String> {
        Ok(VoiceTranscript {
            speaker_id: "unknown_speaker_voice_e2e".to_owned(),
            text: TRANSCRIPT_TEXT.to_owned(),
            confidence: 0.93,
        })
    }
}

fn write_capture_trace_fixture(vault: &Path) -> std::path::PathBuf {
    let mut source = OneChunkSource;
    let transcriber = StaticTranscriber;
    let event = emitted(
        voice::capture_next_chunk(&enabled_config(), &mut source, &transcriber)
            .expect("mocked audio chunk should produce an outcome"),
    );

    let raw_payload = voice_payload_bytes();
    assert_eq!(
        event.payload_hash.as_str(),
        format!("sha256:{}", sha256_hex(&raw_payload)),
        "test source payload must match the sensor-emitted hash"
    );

    let source_path = vault.join(&event.payload_ref);
    std::fs::create_dir_all(source_path.parent().expect("source parent")).expect("sources dir");
    std::fs::write(&source_path, raw_payload).expect("write voice source payload");

    let jsonl_path = vault.join("voice-trace.jsonl");
    let mut jsonl = std::fs::File::create(&jsonl_path).expect("create JSONL file");
    writeln!(
        jsonl,
        "{}",
        serde_json::to_string(&event).expect("serialize capture event")
    )
    .expect("write capture event JSONL");
    jsonl_path
}

fn enable_voice_sensor(vault: &Path, config_home: &Path) {
    let out = cli(config_home)
        .current_dir(vault)
        .args([
            "sensor",
            "enable",
            "voice",
            "--reason",
            "operator_on",
            "--json",
        ])
        .output()
        .expect("cairn sensor enable voice");
    assert!(
        out.status.success(),
        "sensor enable voice failed\nstderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
}

#[tokio::test]
async fn capture_trace_cli_imports_voice_transcript_end_to_end() {
    let vault = fresh_vault();
    let config_home = tempfile::tempdir().expect("temp config home");
    enable_voice_sensor(vault.path(), config_home.path());
    let jsonl_path = write_capture_trace_fixture(vault.path());

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
    assert_eq!(envelope["verb"], "capture_trace");
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
            assert_eq!(row.body, TRANSCRIPT_TEXT);
            assert_eq!(
                row.extra_frontmatter["trace_event"].as_str(),
                Some("user_message")
            );
            assert_eq!(
                row.provenance.source_sensor.as_str(),
                "snr:local:voice:default:v1"
            );
            let expected_ref = format!("sources/voice/{EVENT_ID}.json");
            assert_eq!(
                row.extra_frontmatter["trace"]["payload_ref"].as_str(),
                Some(expected_ref.as_str())
            );
            Ok(())
        })
        .await
        .expect("query voice trace rows");
}
