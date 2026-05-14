# Issue 87: Recording-to-text batch ingestion design

## Context

Issue #87 implements the P0 recording-to-text batch path from design brief
Section 9.1.a and the v0.1 sequencing requirement in Section 19. The required
CLI shape is:

```text
cairn ingest --recording <path>
```

The scope is the full P0 path in one PR: audio transcription, video frame OCR,
ordered `CaptureEvent` generation, normal extract/filter/classify/scope routing,
and record-level forget coverage for derived transcript content. Vision-LM
captioning and richer multimodal parsing remain out of scope because the brief
places those in P1/P2.

The current `origin/main` baseline already has the important prerequisite
surfaces: `CaptureEvent`, `SourceFamily::RecordingBatch`,
`CapturePayload::RecordingBatch`, voice transcription boundaries, capture trace
persistence, record-level forget, and status advertisement for
`recording_batch`. This issue should build on those surfaces rather than adding
a parallel recording store path.

## Recommended Approach

Implement recording ingestion as a batch adapter that emits normal
`RecordingBatch` capture events, then routes them through the same capture
pipeline used by local sensors.

The adapter should live at the CLI/local-sensor boundary rather than in
`cairn-core` because it shells out to local binaries and handles filesystem
payload materialization. Core should remain responsible for schema validation,
pipeline routing, filtering, classification, memory records, and store
contracts.

This approach keeps the CLI as ground truth, reuses existing capture semantics,
and avoids a second path for transcript persistence.

## CLI Contract

`cairn ingest --recording <path>` is mutually exclusive with the existing ingest
sources: positional `source`, `--body`, `--file`, `--folder`, `--url`, and
`--jsonl`.

The generated IDL and clap builder should gain a `recording` path field. The
ingest one-of validation should count `--recording` as a source mode and reject
mixed modes with an actionable `InvalidArgs` response.

The command should support `--json` like other ingest paths. On success, the
response should be `committed` and include normal ingest data plus an optional
`recording_summary` field on `IngestData`. The summary should include segment
counts, audio transcript count, frame OCR count, skipped frame count, original
media hash, and elapsed milliseconds. Human output should render the same
summary without raw transcript text.

## Data Flow

The recording path should execute in this order:

1. Resolve and validate the supplied path.
2. Reject unsupported extensions and missing/corrupt files before any DB write.
3. Probe media metadata with `ffmpeg`/`ffprobe`.
4. Compute the SHA-256 hash of the original media.
5. Create a per-run temporary directory for extracted audio and sampled frames.
6. Extract the audio track into a local PCM/WAV artifact.
7. Sample video frames at the P0 rate from design brief Section 9.1.a.
8. Run sherpa-onnx transcription over the audio chunks.
9. Run local OCR over sampled frames.
10. Deduplicate empty or repeated frame OCR segments.
11. Merge audio and OCR segments by timestamp.
12. Stage small derived payload JSON files in the per-run temp directory.
13. Emit one validated `CaptureEvent` per merged segment.
14. Move staged payload JSON into `sources/recordings/`.
15. Persist the events through the existing capture/filter/classify/store path.
16. Delete temporary extracted audio and frame files.

Steps 1 through 13 are a preparation phase. If they fail, the command must not
perform authoritative DB writes or leave derived payload files in the vault.
Steps 14 through 15 are the commit phase. A segment-level failure after
preparation should abort the operation rather than persisting a partial
transcript stream. If event persistence fails after payload files are moved into
the vault, the handler should remove the recording payload directory for that
run before returning the aborted response.

## Segment Model

Each emitted segment should use:

- `source_family = recording_batch`
- `payload = CapturePayload::RecordingBatch { segment_start_ms, segment_duration_ms }`
- `payload_ref = sources/recordings/<recording_hash>/<segment_id>.json`
- `payload_hash = sha256:<derived-payload-json-hash>`
- `sensor_id = snr:local:recording:default:v1`
- `mode = explicit`

The derived payload JSON should include:

- original media path metadata, stored as a path string only
- original media SHA-256
- file size and probed duration
- track kind: `audio_transcript` or `frame_ocr`
- segment start and duration in milliseconds
- text body
- speaker id and confidence for audio segments when available
- frame timestamp and OCR confidence when available
- extraction tool metadata, such as `ffmpeg` version and OCR backend

The original media must not be copied into the vault by default. The vault keeps
hashes, chunk boundaries, and small derived JSON payloads only.

`segment_id` should be deterministic from the recording hash, track kind,
segment start, segment duration, and normalized text hash. This makes repeated
ingestion and forget leakage tests independent of call order or wall-clock time.

## Audio Path

The audio path should reuse the existing voice transcription abstractions where
possible. If direct reuse is awkward because the current voice source expects
live chunks, add a file-backed source that yields `VoiceAudioChunk` values from
the extracted WAV/PCM stream.

For deterministic tests, the adapter must accept a mock transcriber or expose a
test-only fixture path that bypasses real model loading. The production path can
remain behind the existing `voice-runtime` feature if that matches current
workspace feature policy.

## Video OCR Path

The P0 video path should sample frames through `ffmpeg` and perform local OCR.
The first PR should keep OCR deliberately simple:

- sample at a fixed configured rate, with a default compatible with Section
  9.1.a
- skip frames whose OCR text is empty after trimming
- dedupe adjacent OCR outputs by normalized text hash
- emit frame OCR as timestamped `RecordingBatch` segments

If the repo does not yet have a screen OCR abstraction, add a minimal recording
OCR trait at the adapter boundary so tests can use deterministic OCR results
without requiring system OCR packages. Do not put shell-out or OCR runtime
dependencies in `cairn-core`.

## Persistence And Pipeline Reuse

The recording adapter should directly call an internal capture import helper
with the generated `CaptureEvent`s. If that helper cannot be extracted cleanly
in the PR, the fallback is a temporary JSONL file routed through the existing
`capture_trace` handler. The behavior must remain identical to `capture_trace`
ingestion:

- validate every event before persistence
- verify payload hashes
- route event body text through extract/filter/classify/scope
- store ordered transcript/trace records
- preserve policy traces without logging raw text at info or above

For `RecordingBatch`, trace text resolution should read derived payload JSON and
extract the segment text field, similar to the existing voice payload
`transcript.text` handling.

## Error Handling

Unsupported formats should return actionable errors that name the accepted
formats or the missing local dependency. Corrupt media, missing audio/video
tracks, missing OCR backend, missing ASR model, and failed payload hash
validation should all abort the operation without partial authoritative writes.

Preparation artifacts live in a temp directory and are cleaned up on both
success and failure.

## Forget Semantics

Derived records must carry enough provenance to link them back to the recording
hash and segment payload. Record-level forget should remove the derived text and
indexes through the existing forget path. Future re-ingestion of the same
recording must identify previously forgotten derived targets by record/source
hash so forgotten text is not resurrected.

The PR should include a regression that ingests a recording fixture, forgets a
derived transcript record, then verifies search/retrieve do not surface that
text and source provenance cannot recreate the forgotten content.

## Tests

Use TDD and add tests in this order:

1. `cairn ingest --recording` rejects unsupported extensions and corrupt media
   with no DB writes.
2. A recording fixture produces ordered audio transcript and frame OCR events.
3. Derived records include recording hash and segment boundaries.
4. The original media file is not copied into the vault by default.
5. Resulting transcript text routes through normal memory draft/storage behavior.
6. Record-level forget removes derived transcript content from search/retrieve
   and indexes.

Tests should avoid requiring real microphone access, cloud services, or a vision
model. Runtime-heavy sherpa/OCR fixture tests can be feature-gated, but the
default test suite must cover the adapter behavior with deterministic mock ASR
and OCR implementations.

## Implementation Notes

This design intentionally does not introduce a new core contract. The work
should fit behind existing `MemoryStore`, `SensorIngress`, capture schema, and
pipeline surfaces. If implementation reveals that recording ingestion cannot
reuse capture persistence without duplicating logic, extract a shared capture
batch helper from `capture_trace` rather than creating recording-only store
code.
