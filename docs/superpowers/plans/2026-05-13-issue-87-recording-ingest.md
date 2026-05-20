# Issue 87 Recording Ingest Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement full P0 `cairn ingest --recording <path>` batch ingestion for audio transcription, video frame OCR, ordered `RecordingBatch` capture events, normal pipeline persistence, and record-level forget leakage coverage.

**Architecture:** Add `--recording` to the generated ingest contract, then implement recording ingestion as a CLI/local-sensor adapter that emits normal `CaptureEvent`s instead of writing records directly. The adapter stages derived JSON payloads, moves them under `sources/recordings/`, and reuses extracted capture persistence logic from `capture_trace`.

**Tech Stack:** Rust 2024, `clap` generated from IDL, `serde`/`serde_json`, `tokio`, `tempfile`, `sha2`, `ulid`, existing `cairn-sensors-local` voice abstractions, existing `cairn-store-sqlite`, `cargo nextest`, `cairn-codegen`.

---

## File Structure

- Modify `crates/cairn-idl/schema/verbs/ingest.json`: add `recording` input, CLI flag, one-of variant, and `recording_summary` response data.
- Regenerate generated files with `cargo run -p cairn-idl --bin cairn-codegen --locked`: updates `crates/cairn-cli/src/generated/verbs.rs`, `crates/cairn-core/src/generated/verbs/ingest.rs`, `crates/cairn-core/src/generated/schemas/verbs/ingest.json`, `crates/cairn-mcp/src/generated/schemas/verbs/ingest.json`, `crates/cairn-mcp/src/generated/schemas/verbs/ingest.input.json`, `crates/cairn-idl/tests/snapshots/codegen_snapshot__snapshot_sdk_ingest.snap`, and `crates/cairn-idl/tests/snapshots/codegen_snapshot__snapshot_mcp_ingest_schema.snap`.
- Modify `crates/cairn-cli/src/verbs/ingest.rs`: dispatch `--recording`, include recording in source counting, initialize the recording handler.
- Create `crates/cairn-cli/src/verbs/ingest/recording.rs`: recording CLI adapter, planner, staging, event construction, summary emission, and tests for pure helper functions.
- Modify `crates/cairn-cli/src/verbs/capture_trace.rs`: extract an event-vector import helper and add `RecordingBatch` text extraction from derived payload JSON.
- Create `crates/cairn-cli/tests/recording_ingest.rs`: CLI integration tests for unsupported/corrupt rejection, fixture ingestion, no media copy, ordering, and forget leakage.
- Create fixture files under `fixtures/v0/recordings/`: small deterministic text fixture files used by mock recording mode in tests, not binary media.

## Test Strategy

Default tests must not require real `ffmpeg`, microphone access, sherpa models, tesseract, or OS OCR. Add a test-only deterministic recording fixture mode controlled by `CAIRN_RECORDING_FIXTURE_JSON`. Production code still supports real local tool execution, but tests exercise the adapter contract with deterministic segment inputs.

The fixture JSON should model the output of media preparation:

```json
{
  "media_path": "fixtures/v0/recordings/demo.mp4",
  "media_sha256": "sha256:0000000000000000000000000000000000000000000000000000000000000087",
  "duration_ms": 5200,
  "file_size": 1234,
  "audio": [
    {
      "start_ms": 0,
      "duration_ms": 1800,
      "speaker_id": "unknown_speaker_01",
      "confidence": 0.91,
      "text": "alpha recording launch note"
    },
    {
      "start_ms": 3200,
      "duration_ms": 900,
      "speaker_id": "unknown_speaker_02",
      "confidence": 0.86,
      "text": "beta follow up action"
    }
  ],
  "frames": [
    {
      "timestamp_ms": 2000,
      "duration_ms": 1000,
      "confidence": 0.82,
      "text": "screen shows gamma config"
    },
    {
      "timestamp_ms": 3000,
      "duration_ms": 1000,
      "confidence": 0.80,
      "text": "screen shows gamma config"
    }
  ]
}
```

The duplicate frame verifies adjacent OCR dedupe.

## Task 1: IDL And CLI Surface

**Files:**
- Modify: `crates/cairn-idl/schema/verbs/ingest.json`
- Generated: `crates/cairn-cli/src/generated/verbs.rs`
- Generated: `crates/cairn-core/src/generated/verbs/ingest.rs`
- Generated: `crates/cairn-core/src/generated/schemas/verbs/ingest.json`
- Test: `crates/cairn-cli/tests/cli.rs`

- [ ] **Step 1: Write the failing CLI help test**

Add this test to `crates/cairn-cli/tests/cli.rs` near the other help tests:

```rust
#[test]
fn ingest_help_lists_recording_flag() {
    let out = cli()
        .args(["ingest", "--help"])
        .output()
        .expect("ingest --help");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(
        stdout.contains("--recording"),
        "ingest help missing --recording: {stdout}",
    );
}
```

- [ ] **Step 2: Run the failing test**

Run: `cargo nextest run -p cairn-cli --test cli ingest_help_lists_recording_flag --locked`

Expected: FAIL because `--recording` is not in the generated clap command.

- [ ] **Step 3: Update the ingest IDL**

Patch `crates/cairn-idl/schema/verbs/ingest.json`:

```json
{ "name": "recording", "long": "recording", "value_source": "path" }
```

Add it to `x-cairn-cli.flags` after `jsonl`.

Add this property under `$defs.Args.properties`:

```json
"recording": {
  "type": "string",
  "minLength": 1,
  "description": "Path to an audio or video recording for offline batch transcription and frame OCR."
}
```

Add `{ "required": ["recording"] }` to `$defs.Args.oneOf`.

Add this response summary definition under `$defs`:

```json
"IngestDataRecordingSummary": {
  "type": "object",
  "additionalProperties": false,
  "description": "Recording batch ingestion counts. Present only when source is --recording.",
  "properties": {
    "segments": { "type": "integer", "minimum": 0 },
    "audio_segments": { "type": "integer", "minimum": 0 },
    "frame_ocr_segments": { "type": "integer", "minimum": 0 },
    "skipped_frames": { "type": "integer", "minimum": 0 },
    "records_written": { "type": "integer", "minimum": 0 },
    "media_hash": { "type": "string", "pattern": "^sha256:[0-9a-f]{64}$" },
    "elapsed_ms": { "type": "integer", "minimum": 0 }
  }
}
```

Add this property under `$defs.Data.properties`:

```json
"recording_summary": {
  "$ref": "#/$defs/IngestDataRecordingSummary"
}
```

- [ ] **Step 4: Regenerate generated surfaces**

Run: `cargo run -p cairn-idl --bin cairn-codegen --locked`

Expected: generated ingest args include `recording: Option<String>`, validation says exactly one of `[body, file, folder, jsonl, recording, url]`, `IngestData` includes `recording_summary`, and generated clap has `.arg(clap::Arg::new("recording").long("recording")...)`.

- [ ] **Step 5: Run the help test again**

Run: `cargo nextest run -p cairn-cli --test cli ingest_help_lists_recording_flag --locked`

Expected: PASS.

- [ ] **Step 6: Verify codegen check**

Run: `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check`

Expected: PASS with `cairn-codegen: clean`.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-idl/schema/verbs/ingest.json crates/cairn-cli/src/generated crates/cairn-core/src/generated crates/cairn-mcp/src/generated skills/cairn crates/cairn-idl/tests/snapshots crates/cairn-cli/tests/cli.rs
git commit -m "feat(idl): add recording ingest surface"
```

## Task 2: Recording Fixture Parser And Segment Model

**Files:**
- Create: `crates/cairn-cli/src/verbs/ingest/recording.rs`
- Modify: `crates/cairn-cli/src/verbs/ingest.rs`
- Test: unit tests inside `crates/cairn-cli/src/verbs/ingest/recording.rs`

- [ ] **Step 1: Add the module declaration**

In `crates/cairn-cli/src/verbs/ingest.rs`, add:

```rust
mod recording;
```

next to the other `mod` declarations.

- [ ] **Step 2: Write failing tests for fixture parsing and frame dedupe**

Create `crates/cairn-cli/src/verbs/ingest/recording.rs` with this initial content:

```rust
//! Recording batch ingestion for `cairn ingest --recording`.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
enum SegmentKind {
    AudioTranscript {
        speaker_id: String,
        confidence: f32,
    },
    FrameOcr {
        confidence: f32,
    },
}

#[derive(Debug, Clone, PartialEq)]
struct RecordingSegment {
    start_ms: u64,
    duration_ms: u64,
    text: String,
    kind: SegmentKind,
}

#[derive(Debug, Clone, PartialEq)]
struct RecordingPlan {
    media_path: PathBuf,
    media_hash: String,
    duration_ms: u64,
    file_size: u64,
    skipped_frames: u64,
    segments: Vec<RecordingSegment>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "media_path": "fixtures/v0/recordings/demo.mp4",
      "media_sha256": "sha256:0000000000000000000000000000000000000000000000000000000000000087",
      "duration_ms": 5200,
      "file_size": 1234,
      "audio": [
        {"start_ms": 0, "duration_ms": 1800, "speaker_id": "unknown_speaker_01", "confidence": 0.91, "text": "alpha recording launch note"},
        {"start_ms": 3200, "duration_ms": 900, "speaker_id": "unknown_speaker_02", "confidence": 0.86, "text": "beta follow up action"}
      ],
      "frames": [
        {"timestamp_ms": 2000, "duration_ms": 1000, "confidence": 0.82, "text": "screen shows gamma config"},
        {"timestamp_ms": 3000, "duration_ms": 1000, "confidence": 0.80, "text": "screen shows gamma config"},
        {"timestamp_ms": 4200, "duration_ms": 1000, "confidence": 0.70, "text": "   "}
      ]
    }"#;

    #[test]
    fn fixture_parser_orders_audio_and_deduped_ocr_segments() {
        let plan = parse_fixture_plan(FIXTURE).expect("fixture parses");

        assert_eq!(plan.media_hash, "sha256:0000000000000000000000000000000000000000000000000000000000000087");
        assert_eq!(plan.skipped_frames, 2);
        assert_eq!(
            plan.segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
            vec![
                "alpha recording launch note",
                "screen shows gamma config",
                "beta follow up action",
            ],
        );
        assert_eq!(
            plan.segments.iter().map(|s| s.start_ms).collect::<Vec<_>>(),
            vec![0, 2000, 3200],
        );
    }
}
```

- [ ] **Step 3: Run the failing test**

Run: `cargo nextest run -p cairn-cli recording::tests::fixture_parser_orders_audio_and_deduped_ocr_segments --locked`

Expected: FAIL because `parse_fixture_plan` is missing.

- [ ] **Step 4: Implement fixture parser and normalization**

Add this code above the test module in `recording.rs`:

```rust
#[derive(Debug, serde::Deserialize)]
struct FixturePlan {
    media_path: PathBuf,
    media_sha256: String,
    duration_ms: u64,
    file_size: u64,
    #[serde(default)]
    audio: Vec<FixtureAudio>,
    #[serde(default)]
    frames: Vec<FixtureFrame>,
}

#[derive(Debug, serde::Deserialize)]
struct FixtureAudio {
    start_ms: u64,
    duration_ms: u64,
    speaker_id: String,
    confidence: f32,
    text: String,
}

#[derive(Debug, serde::Deserialize)]
struct FixtureFrame {
    timestamp_ms: u64,
    duration_ms: u64,
    confidence: f32,
    text: String,
}

fn parse_fixture_plan(raw: &str) -> anyhow::Result<RecordingPlan> {
    let fixture: FixturePlan = serde_json::from_str(raw)?;
    if !is_sha256_wire(&fixture.media_sha256) {
        anyhow::bail!("recording fixture media_sha256 must be sha256:<64 lowercase hex>");
    }

    let mut segments = Vec::new();
    for audio in fixture.audio {
        let text = normalize_text(&audio.text);
        if text.is_empty() || audio.duration_ms == 0 {
            continue;
        }
        segments.push(RecordingSegment {
            start_ms: audio.start_ms,
            duration_ms: audio.duration_ms,
            text,
            kind: SegmentKind::AudioTranscript {
                speaker_id: audio.speaker_id,
                confidence: audio.confidence,
            },
        });
    }

    let mut skipped_frames = 0_u64;
    let mut previous_frame_text: Option<String> = None;
    for frame in fixture.frames {
        let text = normalize_text(&frame.text);
        if text.is_empty() || frame.duration_ms == 0 {
            skipped_frames += 1;
            continue;
        }
        if previous_frame_text.as_deref() == Some(text.as_str()) {
            skipped_frames += 1;
            continue;
        }
        previous_frame_text = Some(text.clone());
        segments.push(RecordingSegment {
            start_ms: frame.timestamp_ms,
            duration_ms: frame.duration_ms,
            text,
            kind: SegmentKind::FrameOcr {
                confidence: frame.confidence,
            },
        });
    }

    segments.sort_by_key(|segment| (segment.start_ms, segment.kind.sort_rank()));

    Ok(RecordingPlan {
        media_path: fixture.media_path,
        media_hash: fixture.media_sha256,
        duration_ms: fixture.duration_ms,
        file_size: fixture.file_size,
        skipped_frames,
        segments,
    })
}

impl SegmentKind {
    const fn sort_rank(&self) -> u8 {
        match self {
            Self::AudioTranscript { .. } => 0,
            Self::FrameOcr { .. } => 1,
        }
    }
}

fn normalize_text(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_sha256_wire(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}
```

- [ ] **Step 5: Run the parser test**

Run: `cargo nextest run -p cairn-cli recording::tests::fixture_parser_orders_audio_and_deduped_ocr_segments --locked`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-cli/src/verbs/ingest.rs crates/cairn-cli/src/verbs/ingest/recording.rs
git commit -m "feat(cli): add recording fixture segment planner"
```

## Task 3: Deterministic Segment IDs And Payload JSON

**Files:**
- Modify: `crates/cairn-cli/src/verbs/ingest/recording.rs`
- Test: unit tests inside `recording.rs`

- [ ] **Step 1: Write failing tests for deterministic IDs and payload hashes**

Add this test to `recording.rs`:

```rust
#[test]
fn segment_payloads_are_deterministic_and_body_safe() {
    let plan = parse_fixture_plan(FIXTURE).expect("fixture parses");
    let segment = &plan.segments[0];

    let payload = build_segment_payload(&plan, segment).expect("payload builds");
    let again = build_segment_payload(&plan, segment).expect("payload builds twice");

    assert_eq!(payload.segment_id, again.segment_id);
    assert!(payload.segment_id.starts_with("recseg-"));
    assert!(payload.payload_hash.starts_with("sha256:"));
    assert_eq!(payload.payload_json, again.payload_json);

    let value: serde_json::Value =
        serde_json::from_slice(&payload.payload_json).expect("payload JSON");
    assert_eq!(value["media"]["sha256"], plan.media_hash);
    assert_eq!(value["segment"]["text"], "alpha recording launch note");
    assert!(
        value["media"].get("copied_path").is_none(),
        "payload must not imply media was copied"
    );
}
```

- [ ] **Step 2: Run the failing test**

Run: `cargo nextest run -p cairn-cli recording::tests::segment_payloads_are_deterministic_and_body_safe --locked`

Expected: FAIL because `build_segment_payload` is missing.

- [ ] **Step 3: Implement segment payload construction**

Add imports:

```rust
use sha2::{Digest as _, Sha256};
```

Add this struct and helpers:

```rust
#[derive(Debug, Clone, PartialEq)]
struct SegmentPayload {
    segment_id: String,
    payload_hash: String,
    payload_json: Vec<u8>,
}

fn build_segment_payload(
    plan: &RecordingPlan,
    segment: &RecordingSegment,
) -> anyhow::Result<SegmentPayload> {
    let track_kind = match segment.kind {
        SegmentKind::AudioTranscript { .. } => "audio_transcript",
        SegmentKind::FrameOcr { .. } => "frame_ocr",
    };
    let normalized_for_id = normalize_text(&segment.text).to_lowercase();
    let id_input = format!(
        "{}\n{}\n{}\n{}\n{}",
        plan.media_hash, track_kind, segment.start_ms, segment.duration_ms, normalized_for_id
    );
    let segment_id = format!("recseg-{}", hex_prefix(&Sha256::digest(id_input.as_bytes()), 24));

    let detail = match &segment.kind {
        SegmentKind::AudioTranscript {
            speaker_id,
            confidence,
        } => serde_json::json!({
            "speaker_id": speaker_id,
            "confidence": confidence,
        }),
        SegmentKind::FrameOcr { confidence } => serde_json::json!({
            "confidence": confidence,
        }),
    };
    let value = serde_json::json!({
        "media": {
            "path": plan.media_path.to_string_lossy(),
            "sha256": plan.media_hash,
            "file_size": plan.file_size,
            "duration_ms": plan.duration_ms,
        },
        "segment": {
            "id": segment_id,
            "track_kind": track_kind,
            "start_ms": segment.start_ms,
            "duration_ms": segment.duration_ms,
            "text": segment.text,
            "detail": detail,
        },
        "tools": {
            "ffmpeg": "fixture",
            "ocr": "fixture"
        }
    });
    let payload_json = serde_json::to_vec(&value)?;
    let payload_hash = format!("sha256:{:x}", Sha256::digest(&payload_json));
    Ok(SegmentPayload {
        segment_id,
        payload_hash,
        payload_json,
    })
}

fn hex_prefix(bytes: &[u8], chars: usize) -> String {
    let mut out = String::with_capacity(chars);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{b:02x}");
        if out.len() >= chars {
            out.truncate(chars);
            break;
        }
    }
    out
}
```

- [ ] **Step 4: Run the payload tests**

Run: `cargo nextest run -p cairn-cli recording::tests::segment_payloads_are_deterministic_and_body_safe --locked`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/verbs/ingest/recording.rs
git commit -m "feat(cli): build recording segment payloads"
```

## Task 4: Extract Shared Capture Batch Import Helper

**Files:**
- Modify: `crates/cairn-cli/src/verbs/capture_trace.rs`
- Test: `crates/cairn-cli/tests/capture_trace_verb.rs`

- [ ] **Step 1: Write failing test for vector import helper**

Add this test to `crates/cairn-cli/tests/capture_trace_verb.rs` near existing handler tests. Reuse existing helper functions in that test file where available; if names differ, use the file's existing `capture_trace_event` fixture builder.

```rust
#[tokio::test]
async fn capture_trace_imports_events_from_memory_vector() {
    let vault = tempfile::tempdir().expect("temp vault");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    let store = cairn_store_sqlite::open(vault.path().join(".cairn/cairn.db"))
        .await
        .expect("open store");
    let (events, payload_paths) = one_turn_capture_events(vault.path());
    for (path, bytes) in payload_paths {
        std::fs::create_dir_all(path.parent().expect("payload parent")).expect("payload dir");
        std::fs::write(path, bytes).expect("write payload");
    }

    let response =
        cairn_cli::verbs::capture_trace::run_events_handler(&store, vault.path(), events)
            .await
            .expect("events import");

    assert!(response.failed_turns.is_empty());
}
```

- [ ] **Step 2: Run the failing test**

Run: `cargo nextest run -p cairn-cli --test capture_trace_verb capture_trace_imports_events_from_memory_vector --locked`

Expected: FAIL because `run_events_handler` is missing or private.

- [ ] **Step 3: Extract the helper**

In `crates/cairn-cli/src/verbs/capture_trace.rs`, add a public helper:

```rust
/// Persist an already-materialized batch of capture events.
///
/// This is used by `cairn ingest --recording` after it has staged and hashed
/// derived payload JSON files. Behavior must match `run_handler`.
pub async fn run_events_handler(
    store: &SqliteMemoryStore,
    vault_root: &Path,
    events: Vec<CaptureEvent>,
) -> anyhow::Result<CaptureTraceResponse> {
    run_events_handler_inner(store, vault_root, events, None).await
}
```

Refactor `run_handler_inner` so file reading is a wrapper:

```rust
async fn run_handler_inner(
    store: &SqliteMemoryStore,
    vault_root: &Path,
    from: &Path,
    scope_binding: Option<&ScopeTuple>,
) -> anyhow::Result<CaptureTraceResponse> {
    let events = read_jsonl_events(from).await?;
    run_events_handler_inner(store, vault_root, events, scope_binding).await
}
```

Move the existing body of `run_handler_inner` after `let events = ...` into:

```rust
#[allow(
    clippy::too_many_lines,
    reason = "trace import keeps validation, projection, and per-turn atomicity in one ordered transaction flow"
)]
async fn run_events_handler_inner(
    store: &SqliteMemoryStore,
    vault_root: &Path,
    events: Vec<CaptureEvent>,
    scope_binding: Option<&ScopeTuple>,
) -> anyhow::Result<CaptureTraceResponse> {
    // existing implementation body starts with the refuse_if_degraded guard
}
```

Keep `run_handler_with_scope` and `run_blocks_handler` behavior unchanged.

- [ ] **Step 4: Run capture trace tests**

Run: `cargo nextest run -p cairn-cli --test capture_trace_verb --locked`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/verbs/capture_trace.rs crates/cairn-cli/tests/capture_trace_verb.rs
git commit -m "refactor(cli): expose capture event batch import"
```

## Task 5: RecordingBatch Text Extraction

**Files:**
- Modify: `crates/cairn-cli/src/verbs/capture_trace.rs`
- Test: `crates/cairn-cli/tests/capture_trace_verb.rs`

- [ ] **Step 1: Write failing test for `RecordingBatch` payload text**

Add a test that creates one `CaptureEvent` with `SourceFamily::RecordingBatch`, `CapturePayload::RecordingBatch`, and a payload JSON containing `segment.text = "recording batch body unique"`. Then call `run_events_handler` and assert the stored trace row body is the segment text.

Use this event shape in the test:

```rust
let event = CaptureEvent {
    event_id: CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FD0").expect("valid ULID"),
    captured_at: Rfc3339Timestamp::parse("2026-05-13T12:00:00Z").expect("valid ts"),
    source_family: SourceFamily::RecordingBatch,
    sensor_id: Identity::parse("snr:local:recording:default:v1").expect("valid sensor"),
    mode: CaptureMode::Explicit,
    refs: Some(CaptureRefs {
        session_id: Some("01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned()),
        turn_id: Some("turn-recording".to_owned()),
        tool_id: None,
    }),
    payload_ref: "sources/recordings/hash/recseg-1.json".to_owned(),
    payload_hash: PayloadHash::parse(format!("sha256:{hash}")).expect("valid hash"),
    payload: CapturePayload::RecordingBatch {
        segment_start_ms: 0,
        segment_duration_ms: 1000,
    },
    actor_chain: vec![ActorChainEntry {
        role: ChainRole::Author,
        identity: Identity::parse("snr:local:recording:default:v1").expect("valid identity"),
    }],
};
```

- [ ] **Step 2: Run the failing test**

Run: `cargo nextest run -p cairn-cli --test capture_trace_verb capture_trace_imports_recording_batch_segment_text --locked`

Expected: FAIL because `trace_text` treats recording payload JSON as raw UTF-8 and stores the whole JSON or fails expectations.

- [ ] **Step 3: Implement recording payload text extraction**

In `trace_text`, change the family-specific branch to:

```rust
fn trace_text(event: &CaptureEvent, body_bytes: &[u8]) -> anyhow::Result<String> {
    match &event.payload {
        CapturePayload::Voice { .. } => voice_transcript_text(body_bytes),
        CapturePayload::RecordingBatch { .. } => recording_segment_text(body_bytes),
        _ => String::from_utf8(body_bytes.to_vec()).context("routed body is not valid UTF-8"),
    }
}
```

Add:

```rust
fn recording_segment_text(body_bytes: &[u8]) -> anyhow::Result<String> {
    let raw: serde_json::Value =
        serde_json::from_slice(body_bytes).context("recording payload is not valid JSON")?;
    let text = raw
        .get("segment")
        .and_then(|segment| segment.get("text"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("recording payload missing segment.text"))?;
    if text.trim().is_empty() {
        anyhow::bail!("recording payload segment.text is empty");
    }
    Ok(text.to_owned())
}
```

- [ ] **Step 4: Run recording extraction test**

Run: `cargo nextest run -p cairn-cli --test capture_trace_verb capture_trace_imports_recording_batch_segment_text --locked`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/verbs/capture_trace.rs crates/cairn-cli/tests/capture_trace_verb.rs
git commit -m "feat(cli): extract recording batch segment text"
```

## Task 6: Build Recording Capture Events And Stage Payloads

**Files:**
- Modify: `crates/cairn-cli/src/verbs/ingest/recording.rs`
- Test: unit tests inside `recording.rs`

- [ ] **Step 1: Write failing test for event construction**

Add this test:

```rust
#[test]
fn recording_plan_builds_valid_capture_events_and_payload_refs() {
    let plan = parse_fixture_plan(FIXTURE).expect("fixture parses");
    let batch = build_capture_batch(&plan).expect("batch builds");

    assert_eq!(batch.events.len(), 3);
    assert_eq!(batch.payloads.len(), 3);
    assert_eq!(batch.events[0].source_family, cairn_core::domain::SourceFamily::RecordingBatch);
    assert!(batch.events[0].payload_ref.starts_with("sources/recordings/"));
    assert_eq!(
        batch.events.iter().map(|e| e.payload_ref.as_str()).collect::<Vec<_>>(),
        batch
            .payloads
            .iter()
            .map(|p| p.vault_relative_path.as_str())
            .collect::<Vec<_>>()
    );
    for event in &batch.events {
        event.validate_for_capture().expect("recording event valid");
    }
}
```

- [ ] **Step 2: Run the failing test**

Run: `cargo nextest run -p cairn-cli recording::tests::recording_plan_builds_valid_capture_events_and_payload_refs --locked`

Expected: FAIL because `build_capture_batch` is missing.

- [ ] **Step 3: Implement capture batch construction**

Add imports:

```rust
use cairn_core::domain::{
    ActorChainEntry, CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, CaptureRefs,
    ChainRole, Identity, PayloadHash, Rfc3339Timestamp, SourceFamily,
};
use ulid::Ulid;
```

Add:

```rust
const RECORDING_SENSOR_ID: &str = "snr:local:recording:default:v1";
const RECORDING_SESSION_ID: &str = "recording-batch";

#[derive(Debug, Clone, PartialEq)]
struct StagedPayload {
    vault_relative_path: String,
    bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
struct CaptureBatch {
    events: Vec<CaptureEvent>,
    payloads: Vec<StagedPayload>,
}

fn build_capture_batch(plan: &RecordingPlan) -> anyhow::Result<CaptureBatch> {
    let sensor = Identity::parse(RECORDING_SENSOR_ID).map_err(anyhow::Error::msg)?;
    let captured_at = Rfc3339Timestamp::parse("2026-05-13T00:00:00Z").map_err(anyhow::Error::msg)?;
    let mut events = Vec::with_capacity(plan.segments.len());
    let mut payloads = Vec::with_capacity(plan.segments.len());
    let recording_dir = plan.media_hash.trim_start_matches("sha256:");

    for segment in &plan.segments {
        let payload = build_segment_payload(plan, segment)?;
        let event_id = deterministic_event_id(&payload.segment_id)?;
        let rel = format!("sources/recordings/{recording_dir}/{}.json", payload.segment_id);
        let event = CaptureEvent {
            event_id,
            captured_at: captured_at.clone(),
            source_family: SourceFamily::RecordingBatch,
            sensor_id: sensor.clone(),
            mode: CaptureMode::Explicit,
            refs: Some(CaptureRefs {
                session_id: Some(RECORDING_SESSION_ID.to_owned()),
                turn_id: Some(format!("recording-{recording_dir}")),
                tool_id: None,
            }),
            payload_ref: rel.clone(),
            payload_hash: PayloadHash::parse(payload.payload_hash).map_err(anyhow::Error::msg)?,
            payload: CapturePayload::RecordingBatch {
                segment_start_ms: segment.start_ms,
                segment_duration_ms: segment.duration_ms,
            },
            actor_chain: vec![ActorChainEntry {
                role: ChainRole::Author,
                identity: sensor.clone(),
            }],
        };
        event.validate_for_capture().map_err(anyhow::Error::msg)?;
        payloads.push(StagedPayload {
            vault_relative_path: rel,
            bytes: payload.payload_json,
        });
        events.push(event);
    }

    Ok(CaptureBatch { events, payloads })
}

fn deterministic_event_id(seed: &str) -> anyhow::Result<CaptureEventId> {
    let digest = Sha256::digest(seed.as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[0] &= 0x7f;
    let ulid = Ulid::from_bytes(bytes).to_string();
    CaptureEventId::parse(ulid).map_err(anyhow::Error::msg)
}
```

- [ ] **Step 4: Run event construction tests**

Run: `cargo nextest run -p cairn-cli recording::tests::recording_plan_builds_valid_capture_events_and_payload_refs --locked`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/verbs/ingest/recording.rs
git commit -m "feat(cli): build recording capture events"
```

## Task 7: CLI Recording Handler With Fixture Mode

**Files:**
- Modify: `crates/cairn-cli/src/verbs/ingest.rs`
- Modify: `crates/cairn-cli/src/verbs/ingest/recording.rs`
- Test: `crates/cairn-cli/tests/recording_ingest.rs`
- Create: `fixtures/v0/recordings/recording-fixture.json`
- Create: `fixtures/v0/recordings/demo.mp4`

- [ ] **Step 1: Add fixture files**

Create `fixtures/v0/recordings/recording-fixture.json` with the fixture JSON from this plan's Test Strategy section.

Create `fixtures/v0/recordings/demo.mp4` as a small text sentinel file with this content:

```text
fixture media sentinel; not a real mp4
```

The fixture-mode tests use `CAIRN_RECORDING_FIXTURE_JSON`, so this file exists only to satisfy path validation and no-copy checks.

- [ ] **Step 2: Write failing CLI integration test**

Create `crates/cairn-cli/tests/recording_ingest.rs`:

```rust
#![allow(missing_docs)]

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

fn cli() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
    cmd.env_remove("CAIRN_VAULT");
    cmd.env_remove("CAIRN_REGISTRY");
    cmd
}

fn bootstrap_vault(vault: &Path) {
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: vault.to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
}

fn run_recording(vault: &Path, recording: &Path, fixture: &Path) -> Output {
    cli()
        .current_dir(vault)
        .env("CAIRN_RECORDING_FIXTURE_JSON", fixture)
        .args([
            "ingest",
            "--kind",
            "transcript",
            "--recording",
            recording.to_str().expect("utf-8 recording"),
            "--json",
        ])
        .output()
        .expect("run recording ingest")
}

#[test]
fn recording_fixture_ingests_ordered_audio_and_ocr_segments() {
    let dir = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(dir.path());
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fixture = root.join("fixtures/v0/recordings/recording-fixture.json");
    let recording = root.join("fixtures/v0/recordings/demo.mp4");

    let out = run_recording(dir.path(), &recording, &fixture);
    assert_eq!(
        out.status.code(),
        Some(0),
        "recording ingest failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let value: Value = serde_json::from_slice(&out.stdout).expect("response JSON");
    assert_eq!(value["status"], "committed");
    assert_eq!(value["verb"], "ingest");
    assert_eq!(value["data"]["recording_summary"]["segments"], 3);
    assert_eq!(value["data"]["recording_summary"]["audio_segments"], 2);
    assert_eq!(value["data"]["recording_summary"]["frame_ocr_segments"], 1);
    assert_eq!(value["data"]["recording_summary"]["skipped_frames"], 2);

    let media_copies = walkdir::WalkDir::new(dir.path())
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_name() == "demo.mp4")
        .count();
    assert_eq!(media_copies, 0, "original media must not be copied into vault");

    let search = cli()
        .current_dir(dir.path())
        .args(["search", "--mode", "keyword", "gamma config", "--json"])
        .output()
        .expect("search after recording ingest");
    assert_eq!(search.status.code(), Some(0));
    let search_json: Value = serde_json::from_slice(&search.stdout).expect("search JSON");
    assert_eq!(search_json["data"]["hits"].as_array().map(Vec::len), Some(1));
}
```

- [ ] **Step 3: Run the failing integration test**

Run: `cargo nextest run -p cairn-cli --test recording_ingest recording_fixture_ingests_ordered_audio_and_ocr_segments --locked`

Expected: FAIL because `ingest --recording` is not dispatched.

- [ ] **Step 4: Dispatch `--recording` in ingest**

In `crates/cairn-cli/src/verbs/ingest.rs`, add before `--jsonl` dispatch:

```rust
if let Some(recording_path) = sub.get_one::<std::path::PathBuf>("recording") {
    return recording::run(sub, json, recording_path, &vault_root, config);
}
```

Update `ingest_source_count`:

```rust
+ u8::from(sub.get_one::<PathBuf>("recording").is_some())
```

Update the error string to include `--recording`.

Update `ingest_args_from_matches` to set:

```rust
recording: sub
    .get_one::<PathBuf>("recording")
    .map(|p| p.to_string_lossy().into_owned()),
```

- [ ] **Step 5: Implement handler in fixture mode**

Add public handler code to `recording.rs`:

```rust
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use cairn_core::generated::common::Ulid;
use cairn_core::generated::envelope::{
    Response, ResponseData, ResponsePolicyTrace, ResponseStatus, ResponseVerb,
};
use cairn_core::generated::verbs::ingest::{IngestData, IngestDataRecordingSummary};
use clap::ArgMatches;

use crate::verbs::envelope::{emit_json, human_error, new_operation_id};

pub fn run(
    _sub: &ArgMatches,
    json: bool,
    recording_path: &Path,
    vault_root: &Path,
    _config: cairn_core::config::CairnConfig,
) -> ExitCode {
    let started = Instant::now();
    let result = run_fixture_mode(recording_path, vault_root, started);
    match result {
        Ok(resp) => {
            if json {
                emit_json(&resp);
            } else if let Some(ResponseData::Ingest(data)) = &resp.data {
                println!(
                    "cairn ingest --recording: committed {} segment(s)",
                    data.recording_summary
                        .as_ref()
                        .and_then(|s| s.segments)
                        .unwrap_or(0)
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            let resp = Response {
                contract: "cairn.mcp.v1".to_owned(),
                data: None,
                error: Some(serde_json::json!({
                    "code": "InvalidArgs",
                    "message": format!("{e:#}"),
                })),
                operation_id: new_operation_id(),
                policy_trace: Vec::<ResponsePolicyTrace>::new(),
                status: ResponseStatus::Rejected,
                target: None,
                verb: ResponseVerb::Ingest,
            };
            if json {
                emit_json(&resp);
            } else {
                human_error("ingest", "InvalidArgs", &format!("{e:#}"), &resp.operation_id);
            }
            ExitCode::from(64)
        }
    }
}
```

Implement `run_fixture_mode` to:

1. Check `recording_path.exists()`.
2. Read `CAIRN_RECORDING_FIXTURE_JSON`.
3. Parse plan with `parse_fixture_plan`.
4. Build batch with `build_capture_batch`.
5. Write payload files under `vault_root.join(payload.vault_relative_path)`.
6. Open `vault_root/.cairn/cairn.db`.
7. Call `crate::verbs::capture_trace::run_events_handler`.
8. Emit `IngestData { recording_summary: Some(...) }`.

Use this skeleton:

```rust
fn run_fixture_mode(
    recording_path: &Path,
    vault_root: &Path,
    started: Instant,
) -> anyhow::Result<Response> {
    if !recording_path.exists() {
        anyhow::bail!("recording not found: {}", recording_path.display());
    }
    let fixture_path = std::env::var_os("CAIRN_RECORDING_FIXTURE_JSON")
        .ok_or_else(|| anyhow::anyhow!("real recording runtime is not wired; set CAIRN_RECORDING_FIXTURE_JSON for deterministic fixture mode"))?;
    let raw = std::fs::read_to_string(&fixture_path)?;
    let plan = parse_fixture_plan(&raw)?;
    let batch = build_capture_batch(&plan)?;
    for payload in &batch.payloads {
        let target = vault_root.join(&payload.vault_relative_path);
        std::fs::create_dir_all(target.parent().expect("payload parent"))?;
        std::fs::write(target, &payload.bytes)?;
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let import = rt.block_on(async {
        let store = cairn_store_sqlite::open(vault_root.join(".cairn/cairn.db")).await?;
        crate::verbs::capture_trace::run_events_handler(&store, vault_root, batch.events).await
    });
    if let Err(err) = import {
        let recording_dir = vault_root.join("sources/recordings").join(plan.media_hash.trim_start_matches("sha256:"));
        let _ = std::fs::remove_dir_all(recording_dir);
        return Err(err);
    }

    let audio_segments = plan
        .segments
        .iter()
        .filter(|s| matches!(s.kind, SegmentKind::AudioTranscript { .. }))
        .count() as u64;
    let frame_ocr_segments = plan
        .segments
        .iter()
        .filter(|s| matches!(s.kind, SegmentKind::FrameOcr { .. }))
        .count() as u64;
    let summary = IngestDataRecordingSummary {
        audio_segments: Some(audio_segments),
        elapsed_ms: Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
        frame_ocr_segments: Some(frame_ocr_segments),
        media_hash: Some(plan.media_hash.clone()),
        records_written: Some(plan.segments.len() as u64),
        segments: Some(plan.segments.len() as u64),
        skipped_frames: Some(plan.skipped_frames),
    };
    Ok(Response {
        contract: "cairn.mcp.v1".to_owned(),
        data: Some(ResponseData::Ingest(IngestData {
            cache_hits: None,
            cache_misses: None,
            cache_writes: None,
            files_processed: None,
            jsonl_summary: None,
            plan_ref: None,
            record_id: Ulid(
                batch
                    .events
                    .first()
                    .map(|event| event.event_id.as_str().to_owned())
                    .unwrap_or_else(|| "00000000000000000000000000".to_owned()),
            ),
            recording_summary: Some(summary),
            session_id: RECORDING_SESSION_ID.to_owned(),
        })),
        error: None,
        operation_id: new_operation_id(),
        policy_trace: Vec::<ResponsePolicyTrace>::new(),
        status: ResponseStatus::Committed,
        target: None,
        verb: ResponseVerb::Ingest,
    })
}
```

- [ ] **Step 6: Run recording fixture test**

Run: `cargo nextest run -p cairn-cli --test recording_ingest recording_fixture_ingests_ordered_audio_and_ocr_segments --locked`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-cli/src/verbs/ingest.rs crates/cairn-cli/src/verbs/ingest/recording.rs crates/cairn-cli/tests/recording_ingest.rs fixtures/v0/recordings
git commit -m "feat(cli): ingest recording fixture through capture pipeline"
```

## Task 8: Unsupported And Corrupt Recording Rejections

**Files:**
- Modify: `crates/cairn-cli/src/verbs/ingest/recording.rs`
- Modify: `crates/cairn-cli/tests/recording_ingest.rs`

- [ ] **Step 1: Write failing rejection tests**

Add these tests:

```rust
#[test]
fn recording_rejects_unsupported_extension_without_db_writes() {
    let dir = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(dir.path());
    let unsupported = dir.path().join("note.txt");
    std::fs::write(&unsupported, "not media").expect("write unsupported");

    let out = cli()
        .current_dir(dir.path())
        .args([
            "ingest",
            "--kind",
            "transcript",
            "--recording",
            unsupported.to_str().expect("utf-8 path"),
            "--json",
        ])
        .output()
        .expect("run unsupported recording");

    assert_eq!(out.status.code(), Some(64));
    let value: Value = serde_json::from_slice(&out.stdout).expect("response JSON");
    assert_eq!(value["status"], "rejected");
    assert!(
        value["error"]["message"]
            .as_str()
            .expect("message")
            .contains("unsupported recording format")
    );
    assert!(!dir.path().join("sources/recordings").exists());
}

#[test]
fn recording_rejects_missing_fixture_runtime_without_db_writes() {
    let dir = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(dir.path());
    let recording = dir.path().join("demo.mp4");
    std::fs::write(&recording, "not real media").expect("write recording sentinel");

    let out = cli()
        .current_dir(dir.path())
        .env_remove("CAIRN_RECORDING_FIXTURE_JSON")
        .args([
            "ingest",
            "--kind",
            "transcript",
            "--recording",
            recording.to_str().expect("utf-8 path"),
            "--json",
        ])
        .output()
        .expect("run recording without runtime");

    assert_eq!(out.status.code(), Some(64));
    let value: Value = serde_json::from_slice(&out.stdout).expect("response JSON");
    assert_eq!(value["status"], "rejected");
    assert!(
        value["error"]["message"]
            .as_str()
            .expect("message")
            .contains("real recording runtime is not wired")
    );
    assert!(!dir.path().join("sources/recordings").exists());
}
```

- [ ] **Step 2: Run failing rejection tests**

Run: `cargo nextest run -p cairn-cli --test recording_ingest recording_rejects --locked`

Expected: first test fails until extension validation is added.

- [ ] **Step 3: Add extension validation before fixture parsing**

In `run_fixture_mode`, before reading the fixture env var:

```rust
validate_supported_recording_extension(recording_path)?;
```

Add:

```rust
fn validate_supported_recording_extension(path: &Path) -> anyhow::Result<()> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| anyhow::anyhow!("unsupported recording format: missing extension"))?;
    match ext.as_str() {
        "mp4" | "m4a" | "mp3" | "mkv" | "webm" | "wav" => Ok(()),
        other => anyhow::bail!(
            "unsupported recording format `{other}`; supported: mp4, m4a, mp3, mkv, webm, wav"
        ),
    }
}
```

- [ ] **Step 4: Run rejection tests**

Run: `cargo nextest run -p cairn-cli --test recording_ingest recording_rejects --locked`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/verbs/ingest/recording.rs crates/cairn-cli/tests/recording_ingest.rs
git commit -m "feat(cli): reject invalid recording inputs"
```

## Task 9: Runtime Dependency Checks And Media Probe

**Files:**
- Modify: `crates/cairn-cli/src/verbs/ingest/recording.rs`
- Test: unit tests inside `recording.rs`

- [ ] **Step 1: Write failing tests for supported extensions, command planning, and `ffprobe` JSON parsing**

Add tests:

```rust
#[test]
fn ffmpeg_commands_are_constructed_without_shell_interpolation() {
    let input = PathBuf::from("/tmp/demo file.mp4");
    let temp = PathBuf::from("/tmp/cairn-recording");
    let commands = build_ffmpeg_plan(&input, &temp);

    assert_eq!(commands.audio.program, "ffmpeg");
    assert!(commands.audio.args.contains(&"-i".to_owned()));
    assert!(commands.audio.args.contains(&input.to_string_lossy().to_string()));
    assert_eq!(commands.frames.program, "ffmpeg");
    assert!(
        commands
            .frames
            .args
            .iter()
            .any(|arg| arg.contains("fps=1")),
        "P0 frame sampling should default to 1 fps"
    );
}

#[test]
fn ffprobe_json_extracts_duration_and_stream_presence() {
    let raw = r#"{
      "format": {"duration": "5.200000", "size": "1234"},
      "streams": [
        {"codec_type": "audio"},
        {"codec_type": "video"}
      ]
    }"#;
    let meta = parse_ffprobe_json(raw).expect("metadata parses");

    assert_eq!(meta.duration_ms, 5200);
    assert_eq!(meta.file_size, 1234);
    assert!(meta.has_audio);
    assert!(meta.has_video);
}
```

- [ ] **Step 2: Run the failing test**

Run: `cargo nextest run -p cairn-cli recording::tests::ffmpeg --locked`

Expected: FAIL because `build_ffmpeg_plan` and `parse_ffprobe_json` are missing.

- [ ] **Step 3: Add runtime planning and probe types**

Add:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandPlan {
    program: String,
    args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FfmpegPlan {
    probe: CommandPlan,
    audio: CommandPlan,
    frames: CommandPlan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MediaMetadata {
    duration_ms: u64,
    file_size: u64,
    has_audio: bool,
    has_video: bool,
}

fn build_ffmpeg_plan(input: &Path, temp_dir: &Path) -> FfmpegPlan {
    let audio_out = temp_dir.join("audio.wav");
    let frames_out = temp_dir.join("frame-%06d.png");
    FfmpegPlan {
        probe: CommandPlan {
            program: "ffprobe".to_owned(),
            args: vec![
                "-v".to_owned(),
                "error".to_owned(),
                "-show_format".to_owned(),
                "-show_streams".to_owned(),
                "-of".to_owned(),
                "json".to_owned(),
                input.to_string_lossy().to_string(),
            ],
        },
        audio: CommandPlan {
            program: "ffmpeg".to_owned(),
            args: vec![
                "-hide_banner".to_owned(),
                "-loglevel".to_owned(),
                "error".to_owned(),
                "-y".to_owned(),
                "-i".to_owned(),
                input.to_string_lossy().to_string(),
                "-vn".to_owned(),
                "-ac".to_owned(),
                "1".to_owned(),
                "-ar".to_owned(),
                "16000".to_owned(),
                audio_out.to_string_lossy().to_string(),
            ],
        },
        frames: CommandPlan {
            program: "ffmpeg".to_owned(),
            args: vec![
                "-hide_banner".to_owned(),
                "-loglevel".to_owned(),
                "error".to_owned(),
                "-y".to_owned(),
                "-i".to_owned(),
                input.to_string_lossy().to_string(),
                "-vf".to_owned(),
                "fps=1".to_owned(),
                frames_out.to_string_lossy().to_string(),
            ],
        },
    }
}

fn parse_ffprobe_json(raw: &str) -> anyhow::Result<MediaMetadata> {
    let value: serde_json::Value = serde_json::from_str(raw)?;
    let duration_secs = value
        .pointer("/format/duration")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("ffprobe output missing format.duration"))?
        .parse::<f64>()?;
    let file_size = value
        .pointer("/format/size")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("ffprobe output missing format.size"))?
        .parse::<u64>()?;
    let streams = value
        .get("streams")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("ffprobe output missing streams"))?;
    let has_audio = streams.iter().any(|stream| {
        stream.get("codec_type").and_then(serde_json::Value::as_str) == Some("audio")
    });
    let has_video = streams.iter().any(|stream| {
        stream.get("codec_type").and_then(serde_json::Value::as_str) == Some("video")
    });
    if !has_audio && !has_video {
        anyhow::bail!("recording contains no audio or video streams");
    }
    Ok(MediaMetadata {
        duration_ms: (duration_secs * 1000.0).round() as u64,
        file_size,
        has_audio,
        has_video,
    })
}
```

- [ ] **Step 4: Add command execution helper**

Add:

```rust
fn run_command_capture_stdout(plan: &CommandPlan) -> anyhow::Result<String> {
    let output = std::process::Command::new(&plan.program)
        .args(&plan.args)
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run {}: {e}", plan.program))?;
    if !output.status.success() {
        anyhow::bail!(
            "{} failed with status {:?}: {}",
            plan.program,
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout)
        .map_err(|e| anyhow::anyhow!("{} emitted non-UTF8 stdout: {e}", plan.program))
}

fn run_command_no_stdout(plan: &CommandPlan) -> anyhow::Result<()> {
    let output = std::process::Command::new(&plan.program)
        .args(&plan.args)
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run {}: {e}", plan.program))?;
    if !output.status.success() {
        anyhow::bail!(
            "{} failed with status {:?}: {}",
            plan.program,
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}
```

- [ ] **Step 5: Run runtime planning tests**

Run: `cargo nextest run -p cairn-cli recording::tests::ffmpeg_commands_are_constructed_without_shell_interpolation --locked`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-cli/src/verbs/ingest/recording.rs
git commit -m "feat(cli): add recording media runtime probing"
```

## Task 10: WAV Chunk Source And ASR Runtime

**Files:**
- Modify: `crates/cairn-cli/src/verbs/ingest/recording.rs`
- Test: unit tests inside `recording.rs`

- [ ] **Step 1: Write failing tests for WAV parsing and ASR chunk conversion**

Add a helper in tests to build a minimal 16-bit PCM WAV, then assert a chunk is produced:

```rust
#[test]
fn wav_reader_yields_voice_audio_chunks() {
    let wav = pcm16_wav_bytes(16_000, &[0_i16, 16_384, -16_384, 0]);
    let chunks = wav_bytes_to_chunks(
        &wav,
        "01ARZ3NDEKTSV4RRFFQ69G5FE0",
        "2026-05-13T12:00:00Z",
    )
    .expect("wav chunks");

    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].device.sample_rate_hz, 16_000);
    assert_eq!(chunks[0].device.channels, 1);
    assert_eq!(chunks[0].duration_ms, 0);
    assert_eq!(chunks[0].samples.len(), 4);
    assert!((chunks[0].samples[1] - 0.5).abs() < 0.001);
}
```

- [ ] **Step 2: Run the failing WAV test**

Run: `cargo nextest run -p cairn-cli recording::tests::wav_reader_yields_voice_audio_chunks --locked`

Expected: FAIL because `wav_bytes_to_chunks` is missing.

- [ ] **Step 3: Implement a minimal PCM WAV parser**

Add imports:

```rust
use cairn_core::domain::{CaptureEventId, Rfc3339Timestamp};
use cairn_sensors_local::voice::{VoiceAudioChunk, VoiceDeviceMetadata, VoiceTranscriber};
```

Add:

```rust
fn wav_bytes_to_chunks(
    bytes: &[u8],
    event_seed: &str,
    captured_at: &str,
) -> anyhow::Result<Vec<VoiceAudioChunk>> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        anyhow::bail!("extracted audio is not a RIFF/WAVE file");
    }
    let mut offset = 12_usize;
    let mut sample_rate = None;
    let mut channels = None;
    let mut bits_per_sample = None;
    let mut data: Option<&[u8]> = None;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let len = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().expect("slice")) as usize;
        offset += 8;
        if offset + len > bytes.len() {
            anyhow::bail!("WAV chunk length exceeds file size");
        }
        match id {
            b"fmt " => {
                if len < 16 {
                    anyhow::bail!("WAV fmt chunk too short");
                }
                let audio_format = u16::from_le_bytes(bytes[offset..offset + 2].try_into().expect("slice"));
                if audio_format != 1 {
                    anyhow::bail!("only PCM WAV audio is supported");
                }
                channels = Some(u16::from_le_bytes(bytes[offset + 2..offset + 4].try_into().expect("slice")));
                sample_rate = Some(u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().expect("slice")));
                bits_per_sample = Some(u16::from_le_bytes(bytes[offset + 14..offset + 16].try_into().expect("slice")));
            }
            b"data" => data = Some(&bytes[offset..offset + len]),
            _ => {}
        }
        offset += len + (len % 2);
    }
    let sample_rate = sample_rate.ok_or_else(|| anyhow::anyhow!("WAV missing fmt sample rate"))?;
    let channels = channels.ok_or_else(|| anyhow::anyhow!("WAV missing channel count"))?;
    if channels != 1 {
        anyhow::bail!("recording audio extraction must produce mono WAV");
    }
    if bits_per_sample != Some(16) {
        anyhow::bail!("only 16-bit PCM WAV is supported");
    }
    let data = data.ok_or_else(|| anyhow::anyhow!("WAV missing data chunk"))?;
    let mut samples = Vec::with_capacity(data.len() / 2);
    for pair in data.chunks_exact(2) {
        let sample = i16::from_le_bytes([pair[0], pair[1]]);
        samples.push(f32::from(sample) / 32768.0);
    }
    if samples.is_empty() {
        return Ok(Vec::new());
    }
    let duration_ms = ((samples.len() as u64) * 1000) / u64::from(sample_rate);
    Ok(vec![VoiceAudioChunk {
        event_id: CaptureEventId::parse(event_seed).map_err(anyhow::Error::msg)?,
        captured_at: Rfc3339Timestamp::parse(captured_at).map_err(anyhow::Error::msg)?,
        started_at: Rfc3339Timestamp::parse(captured_at).map_err(anyhow::Error::msg)?,
        duration_ms,
        samples,
        device: VoiceDeviceMetadata {
            name: "recording file".to_owned(),
            host: "ffmpeg".to_owned(),
            sample_rate_hz: sample_rate,
            channels,
        },
        refs: None,
    }])
}
```

The test helper `pcm16_wav_bytes` can live under `#[cfg(test)]` and write a standard 44-byte WAV header plus little-endian samples.

- [ ] **Step 4: Add ASR conversion with mockable transcriber**

Add:

```rust
fn transcribe_chunks<T: VoiceTranscriber>(
    chunks: &[VoiceAudioChunk],
    transcriber: &T,
) -> anyhow::Result<Vec<RecordingSegment>> {
    let mut segments = Vec::new();
    let mut cursor_ms = 0_u64;
    for chunk in chunks {
        let transcript = transcriber
            .transcribe(chunk)
            .map_err(|e| anyhow::anyhow!("recording audio transcription failed: {e}"))?;
        let text = normalize_text(&transcript.text);
        if !text.is_empty() {
            segments.push(RecordingSegment {
                start_ms: cursor_ms,
                duration_ms: chunk.duration_ms.max(1),
                text,
                kind: SegmentKind::AudioTranscript {
                    speaker_id: transcript.speaker_id,
                    confidence: transcript.confidence,
                },
            });
        }
        cursor_ms = cursor_ms.saturating_add(chunk.duration_ms.max(1));
    }
    Ok(segments)
}
```

- [ ] **Step 5: Wire sherpa-onnx behind the existing `voice-runtime` feature**

Add a config builder:

```rust
#[cfg(feature = "voice-runtime")]
fn build_sherpa_transcriber_from_env() -> anyhow::Result<cairn_sensors_local::voice_runtime::SherpaOnnxTranscriber> {
    use cairn_sensors_local::voice_runtime::{SherpaOnnxTranscriber, SherpaOnnxTranscriberConfig};
    let model = std::env::var_os("CAIRN_SHERPA_MODEL")
        .ok_or_else(|| anyhow::anyhow!("CAIRN_SHERPA_MODEL must point to a SenseVoice ONNX model"))?;
    let tokens = std::env::var_os("CAIRN_SHERPA_TOKENS")
        .ok_or_else(|| anyhow::anyhow!("CAIRN_SHERPA_TOKENS must point to sherpa tokens.txt"))?;
    let config = SherpaOnnxTranscriberConfig::sense_voice(model.into(), tokens.into());
    SherpaOnnxTranscriber::from_config(config).map_err(|e| anyhow::anyhow!("load sherpa-onnx transcriber: {e}"))
}

#[cfg(not(feature = "voice-runtime"))]
fn build_sherpa_transcriber_from_env() -> anyhow::Result<()> {
    anyhow::bail!("recording audio transcription requires building cairn-cli with --features voice-runtime")
}
```

- [ ] **Step 6: Run WAV and ASR unit tests**

Run: `cargo nextest run -p cairn-cli recording::tests::wav_reader_yields_voice_audio_chunks --locked`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-cli/src/verbs/ingest/recording.rs crates/cairn-cli/Cargo.toml
git commit -m "feat(cli): add recording audio transcription runtime"
```

## Task 11: Frame OCR Runtime

**Files:**
- Modify: `crates/cairn-cli/src/verbs/ingest/recording.rs`
- Test: unit tests inside `recording.rs`

- [ ] **Step 1: Write failing OCR parsing and dedupe tests**

Add:

```rust
#[test]
fn ocr_outputs_become_deduped_frame_segments() {
    let frames = vec![
        FrameOcrObservation { timestamp_ms: 1000, duration_ms: 1000, confidence: 1.0, text: " screen shows gamma config ".to_owned() },
        FrameOcrObservation { timestamp_ms: 2000, duration_ms: 1000, confidence: 1.0, text: "screen shows gamma config".to_owned() },
        FrameOcrObservation { timestamp_ms: 3000, duration_ms: 1000, confidence: 1.0, text: "".to_owned() },
        FrameOcrObservation { timestamp_ms: 4000, duration_ms: 1000, confidence: 1.0, text: "delta dashboard".to_owned() },
    ];

    let (segments, skipped) = frame_observations_to_segments(frames);

    assert_eq!(skipped, 2);
    assert_eq!(
        segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
        vec!["screen shows gamma config", "delta dashboard"]
    );
}
```

- [ ] **Step 2: Run the failing OCR tests**

Run: `cargo nextest run -p cairn-cli recording::tests::ocr_outputs_become_deduped_frame_segments --locked`

Expected: FAIL because `FrameOcrObservation` and `frame_observations_to_segments` are missing.

- [ ] **Step 3: Implement OCR observation conversion**

Add:

```rust
#[derive(Debug, Clone, PartialEq)]
struct FrameOcrObservation {
    timestamp_ms: u64,
    duration_ms: u64,
    confidence: f32,
    text: String,
}

fn frame_observations_to_segments(
    observations: Vec<FrameOcrObservation>,
) -> (Vec<RecordingSegment>, u64) {
    let mut skipped = 0_u64;
    let mut previous: Option<String> = None;
    let mut segments = Vec::new();
    for observation in observations {
        let text = normalize_text(&observation.text);
        if text.is_empty() || observation.duration_ms == 0 {
            skipped += 1;
            continue;
        }
        if previous.as_deref() == Some(text.as_str()) {
            skipped += 1;
            continue;
        }
        previous = Some(text.clone());
        segments.push(RecordingSegment {
            start_ms: observation.timestamp_ms,
            duration_ms: observation.duration_ms,
            text,
            kind: SegmentKind::FrameOcr {
                confidence: observation.confidence,
            },
        });
    }
    (segments, skipped)
}
```

- [ ] **Step 4: Implement tesseract CLI OCR boundary**

Add:

```rust
fn run_tesseract_ocr(frame_path: &Path) -> anyhow::Result<String> {
    let output = std::process::Command::new("tesseract")
        .arg(frame_path)
        .arg("stdout")
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run tesseract OCR: {e}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "tesseract OCR failed with status {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout).map_err(|e| anyhow::anyhow!("tesseract emitted non-UTF8 stdout: {e}"))
}

fn frame_timestamp_ms_from_name(path: &Path) -> u64 {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    let digits = stem.rsplit('-').next().unwrap_or("0");
    digits.parse::<u64>().unwrap_or(0).saturating_mul(1000)
}
```

The fixed 1 fps P0 sampler makes this filename convention deterministic.

- [ ] **Step 5: Run OCR tests**

Run: `cargo nextest run -p cairn-cli recording::tests::ocr_outputs_become_deduped_frame_segments --locked`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-cli/src/verbs/ingest/recording.rs
git commit -m "feat(cli): add recording frame OCR runtime"
```

## Task 12: Real Recording Runtime Integration

**Files:**
- Modify: `crates/cairn-cli/src/verbs/ingest/recording.rs`
- Modify: `crates/cairn-cli/tests/recording_ingest.rs`

- [ ] **Step 1: Write failing test for no fixture env using actionable dependency errors**

Keep the existing no-fixture test, but update its expected message to accept real runtime dependency checks:

```rust
assert!(
    value["error"]["message"]
        .as_str()
        .expect("message")
        .contains("ffprobe")
        || value["error"]["message"]
            .as_str()
            .expect("message")
            .contains("voice-runtime")
        || value["error"]["message"]
            .as_str()
            .expect("message")
            .contains("tesseract")
);
```

- [ ] **Step 2: Run the no-fixture test**

Run: `cargo nextest run -p cairn-cli --test recording_ingest recording_rejects_missing_fixture_runtime_without_db_writes --locked`

Expected: PASS with an actionable local dependency error and no `sources/recordings` directory.

- [ ] **Step 3: Implement real runtime plan assembly**

Replace fixture-only planning in `run_fixture_mode` with a general `build_recording_plan`:

```rust
fn build_recording_plan(recording_path: &Path) -> anyhow::Result<RecordingPlan> {
    if let Some(fixture_path) = std::env::var_os("CAIRN_RECORDING_FIXTURE_JSON") {
        let raw = std::fs::read_to_string(&fixture_path)?;
        return parse_fixture_plan(&raw);
    }
    build_real_runtime_plan(recording_path)
}
```

Implement:

```rust
fn build_real_runtime_plan(recording_path: &Path) -> anyhow::Result<RecordingPlan> {
    validate_supported_recording_extension(recording_path)?;
    let temp = tempfile::tempdir()?;
    let ffmpeg = build_ffmpeg_plan(recording_path, temp.path());
    let probe = run_command_capture_stdout(&ffmpeg.probe)?;
    let metadata = parse_ffprobe_json(&probe)?;
    let media_hash = sha256_file(recording_path)?;

    let mut segments = Vec::new();
    let mut skipped_frames = 0_u64;

    if metadata.has_audio {
        run_command_no_stdout(&ffmpeg.audio)?;
        let wav = std::fs::read(temp.path().join("audio.wav"))?;
        let chunks = wav_bytes_to_chunks(
            &wav,
            "01ARZ3NDEKTSV4RRFFQ69G5FE1",
            "2026-05-13T00:00:00Z",
        )?;
        #[cfg(feature = "voice-runtime")]
        {
            let transcriber = build_sherpa_transcriber_from_env()?;
            segments.extend(transcribe_chunks(&chunks, &transcriber)?);
        }
        #[cfg(not(feature = "voice-runtime"))]
        {
            let _ = chunks;
            build_sherpa_transcriber_from_env()?;
        }
    }

    if metadata.has_video {
        run_command_no_stdout(&ffmpeg.frames)?;
        let mut observations = Vec::new();
        for entry in std::fs::read_dir(temp.path())? {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) == Some("png") {
                let text = run_tesseract_ocr(&path)?;
                observations.push(FrameOcrObservation {
                    timestamp_ms: frame_timestamp_ms_from_name(&path),
                    duration_ms: 1000,
                    confidence: 1.0,
                    text,
                });
            }
        }
        observations.sort_by_key(|obs| obs.timestamp_ms);
        let (ocr_segments, skipped) = frame_observations_to_segments(observations);
        skipped_frames = skipped;
        segments.extend(ocr_segments);
    }

    segments.sort_by_key(|segment| (segment.start_ms, segment.kind.sort_rank()));
    if segments.is_empty() {
        anyhow::bail!("recording produced no transcript or OCR segments");
    }

    Ok(RecordingPlan {
        media_path: recording_path.to_path_buf(),
        media_hash,
        duration_ms: metadata.duration_ms,
        file_size: metadata.file_size,
        skipped_frames,
        segments,
    })
}
```

Add:

```rust
fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}
```

- [ ] **Step 4: Update handler to use `build_recording_plan`**

Rename `run_fixture_mode` to `run_recording_ingest` and replace direct fixture parsing with:

```rust
let plan = build_recording_plan(recording_path)?;
```

Keep staging, capture event import, cleanup, and response construction unchanged.

- [ ] **Step 5: Run recording tests**

Run: `cargo nextest run -p cairn-cli --test recording_ingest --locked`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-cli/src/verbs/ingest/recording.rs crates/cairn-cli/tests/recording_ingest.rs
git commit -m "feat(cli): wire real recording runtime path"
```

## Task 13: Record-level Forget Leakage Coverage

**Files:**
- Modify: `crates/cairn-cli/tests/recording_ingest.rs`

- [ ] **Step 1: Write failing forget leakage test**

Add:

```rust
fn run_json_ok(vault: &Path, args: &[&str]) -> Value {
    let out = cli()
        .current_dir(vault)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to run cairn {args:?}: {e}"));
    assert_eq!(
        out.status.code(),
        Some(0),
        "cairn {args:?} failed\nstderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    serde_json::from_slice(&out.stdout).expect("JSON")
}

#[test]
fn recording_derived_record_forget_removes_search_and_retrieve_text() {
    let dir = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(dir.path());
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fixture = root.join("fixtures/v0/recordings/recording-fixture.json");
    let recording = root.join("fixtures/v0/recordings/demo.mp4");

    let out = run_recording(dir.path(), &recording, &fixture);
    assert_eq!(out.status.code(), Some(0));

    let search = run_json_ok(dir.path(), &["search", "--mode", "keyword", "alpha recording launch note", "--json"]);
    let hits = search["data"]["hits"].as_array().expect("hits");
    assert_eq!(hits.len(), 1, "expected exactly one derived recording hit: {search}");
    let record_id = hits[0]["id"].as_str().expect("hit id").to_owned();

    let forget = run_json_ok(dir.path(), &["forget", "--record", &record_id, "--json"]);
    assert_eq!(forget["status"], "committed");

    let after_search = run_json_ok(dir.path(), &["search", "--mode", "keyword", "alpha recording launch note", "--json"]);
    assert_eq!(after_search["data"]["hits"].as_array().map(Vec::len), Some(0));

    let retrieve = run_json_ok(dir.path(), &["retrieve", &record_id, "--json"]);
    assert!(
        retrieve.to_string().contains("alpha recording launch note") == false,
        "retrieve after forget must not leak recording text: {retrieve}"
    );
}
```

- [ ] **Step 2: Run the forget leakage test**

Run: `cargo nextest run -p cairn-cli --test recording_ingest recording_derived_record_forget_removes_search_and_retrieve_text --locked`

Expected: PASS if recording records use existing store provenance correctly. If it fails, inspect whether recording records are stored as trace rows not searched by keyword; update the assertion to locate the derived record through retrieve/session APIs while preserving the no-leak invariant.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-cli/tests/recording_ingest.rs
git commit -m "test(cli): cover forget for recording-derived text"
```

## Task 14: Full Verification And Cleanup

**Files:**
- Modify only files needed for fixes from verification.

- [ ] **Step 1: Format**

Run: `cargo fmt --all`

Expected: no errors.

- [ ] **Step 2: Run focused tests**

Run:

```bash
cargo nextest run -p cairn-cli --test recording_ingest --locked
cargo nextest run -p cairn-cli --test capture_trace_verb --locked
cargo nextest run -p cairn-cli --test cli ingest_help_lists_recording_flag --locked
```

Expected: all PASS.

- [ ] **Step 3: Run codegen drift check**

Run: `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check`

Expected: PASS.

- [ ] **Step 4: Run docs generation check**

Run: `cargo run -p cairn-cli --bin cairn-docgen --locked -- --check`

Expected: PASS. If it reports generated docs drift, run `cargo run -p cairn-cli --bin cairn-docgen --locked`, review the docs diff, and commit it as `docs: update generated CLI reference`.

- [ ] **Step 5: Run workspace tests**

Run: `cargo nextest run --workspace --locked --no-fail-fast`

Expected: PASS.

- [ ] **Step 6: Run boundary check**

Run: `./scripts/check-core-boundary.sh`

Expected: PASS.

- [ ] **Step 7: Final status review**

Run: `git status --short`

Expected: clean working tree. If generated snapshots are pending from codegen or docs, review and commit them before finishing.

## Self-review

Spec coverage:

- CLI `cairn ingest --recording <path>`: Task 1 and Task 7.
- Full audio + video OCR scope: Task 2 models audio and OCR segments, Task 9 probes/extracts media, Task 10 implements WAV/ASR, Task 11 implements OCR, and Task 12 wires the real runtime path.
- Hashes/chunk boundaries without media copy: Task 3 and Task 7.
- Normal capture/filter/classify/store routing: Task 4, Task 5, Task 7.
- Unsupported/corrupt actionable errors and no authoritative writes: Task 8.
- Record-level forget leakage: Task 13.
- Verification checklist: Task 14.

Placeholder scan:

- The plan contains concrete implementation instructions throughout.
- Every code-writing step includes exact code or a concrete skeleton with function names.
- Every test step includes exact commands and expected failure/pass behavior.

Type consistency:

- `recording` is added to `IngestArgs` and `IngestDataRecordingSummary` through IDL/codegen in Task 1.
- `RecordingPlan`, `RecordingSegment`, `SegmentPayload`, `CaptureBatch`, and `StagedPayload` are introduced before subsequent tasks use them.
- `run_events_handler` is introduced in Task 4 before `recording::run` uses it in Task 7.
