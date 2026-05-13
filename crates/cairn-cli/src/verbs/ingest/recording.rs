//! Recording batch ingestion for `cairn ingest --recording`.

#![allow(
    dead_code,
    reason = "Recording planner pieces are staged before runtime ingestion wiring."
)]

use cairn_core::domain::canonical::canonical_bytes;
use sha2::{Digest as _, Sha256};
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
enum SegmentKind {
    AudioTranscript { speaker_id: String, confidence: f32 },
    FrameOcr { confidence: f32 },
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

#[derive(Debug, Clone, PartialEq)]
struct SegmentPayload {
    segment_id: String,
    payload_hash: String,
    payload_json: Vec<u8>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
struct FixtureAudio {
    start_ms: u64,
    duration_ms: u64,
    speaker_id: String,
    confidence: f32,
    text: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureFrame {
    timestamp_ms: u64,
    duration_ms: u64,
    confidence: f32,
    text: String,
}

#[derive(Debug)]
struct FrameObservation {
    timestamp_ms: u64,
    fixture_index: usize,
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
    let mut frames = Vec::new();
    for (fixture_index, frame) in fixture.frames.into_iter().enumerate() {
        let text = normalize_text(&frame.text);
        if text.is_empty() || frame.duration_ms == 0 {
            skipped_frames += 1;
            continue;
        }
        frames.push(FrameObservation {
            timestamp_ms: frame.timestamp_ms,
            fixture_index,
            duration_ms: frame.duration_ms,
            confidence: frame.confidence,
            text,
        });
    }

    frames.sort_by_key(|frame| (frame.timestamp_ms, frame.fixture_index));

    let mut previous_frame_text: Option<String> = None;
    for frame in frames {
        if previous_frame_text.as_deref() == Some(frame.text.as_str()) {
            skipped_frames += 1;
            continue;
        }
        previous_frame_text = Some(frame.text.clone());
        segments.push(RecordingSegment {
            start_ms: frame.timestamp_ms,
            duration_ms: frame.duration_ms,
            text: frame.text,
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

fn build_segment_payload(
    plan: &RecordingPlan,
    segment: &RecordingSegment,
) -> anyhow::Result<SegmentPayload> {
    let track_kind = match &segment.kind {
        SegmentKind::AudioTranscript { .. } => "audio_transcript",
        SegmentKind::FrameOcr { .. } => "frame_ocr",
    };
    let normalized_for_id = normalize_text(&segment.text).to_lowercase();
    let id_input = format!(
        "{}\n{}\n{}\n{}\n{}",
        plan.media_hash, track_kind, segment.start_ms, segment.duration_ms, normalized_for_id
    );
    let segment_id = format!(
        "recseg-{}",
        hex_prefix(&Sha256::digest(id_input.as_bytes()), 24)
    );

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
            "timestamp_ms": segment.start_ms,
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
    let payload_json = canonical_bytes(&value)?;
    let payload_hash = format!("sha256:{:x}", Sha256::digest(&payload_json));

    Ok(SegmentPayload {
        segment_id,
        payload_hash,
        payload_json,
    })
}

fn build_segment_payloads(plan: &RecordingPlan) -> anyhow::Result<Vec<SegmentPayload>> {
    let mut seen_segment_ids = HashSet::new();
    let mut payloads = Vec::with_capacity(plan.segments.len());

    for segment in &plan.segments {
        let payload = build_segment_payload(plan, segment)?;
        if !seen_segment_ids.insert(payload.segment_id.clone()) {
            anyhow::bail!(
                "duplicate recording segment_id {} for media {} at start_ms {}",
                payload.segment_id,
                plan.media_hash,
                segment.start_ms
            );
        }
        payloads.push(payload);
    }

    Ok(payloads)
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

        assert_eq!(
            plan.media_hash,
            "sha256:0000000000000000000000000000000000000000000000000000000000000087"
        );
        assert_eq!(plan.skipped_frames, 2);
        assert_eq!(
            plan.segments
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>(),
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

    #[test]
    fn segment_payloads_are_deterministic_and_body_safe() {
        let plan = parse_fixture_plan(FIXTURE).expect("fixture parses");
        let segment = &plan.segments[0];

        let payload = build_segment_payload(&plan, segment).expect("payload builds");
        let again = build_segment_payload(&plan, segment).expect("payload builds twice");

        assert_eq!(payload.segment_id, again.segment_id);
        assert!(payload.segment_id.starts_with("recseg-"));
        assert_eq!(payload.segment_id.len(), "recseg-".len() + 24);
        let segment_id_suffix = payload
            .segment_id
            .strip_prefix("recseg-")
            .expect("recseg prefix");
        assert!(
            segment_id_suffix
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
            "segment id suffix must be lowercase hex"
        );
        assert_eq!(payload.payload_hash, again.payload_hash);
        assert!(payload.payload_hash.starts_with("sha256:"));
        assert_eq!(payload.payload_hash.len(), "sha256:".len() + 64);
        assert!(
            payload.payload_hash["sha256:".len()..]
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
            "payload hash must be lowercase hex"
        );
        assert_eq!(
            payload.payload_hash,
            format!("sha256:{:x}", Sha256::digest(&payload.payload_json))
        );
        assert_eq!(payload.payload_json, again.payload_json);
        assert_eq!(payload.segment_id, "recseg-af067ba975f4fc04c20d1743");
        assert_eq!(
            payload.payload_json.as_slice(),
            br#"{"media":{"duration_ms":5200,"file_size":1234,"path":"fixtures/v0/recordings/demo.mp4","sha256":"sha256:0000000000000000000000000000000000000000000000000000000000000087"},"segment":{"detail":{"confidence":0.9100000262260437,"speaker_id":"unknown_speaker_01"},"duration_ms":1800,"id":"recseg-af067ba975f4fc04c20d1743","start_ms":0,"text":"alpha recording launch note","track_kind":"audio_transcript"},"tools":{"ffmpeg":"fixture","ocr":"fixture"}}"#
        );
        assert_eq!(
            payload.payload_hash,
            "sha256:4c23dd7dcc78517f84748fb146eca9cae2059ef67f93579e9f383c24ecd79edf"
        );

        let value: serde_json::Value =
            serde_json::from_slice(&payload.payload_json).expect("payload JSON");
        assert_eq!(value["media"]["sha256"], plan.media_hash);
        assert_eq!(value["segment"]["text"], "alpha recording launch note");
        assert!(
            value["media"].get("copied_path").is_none(),
            "payload must not imply media was copied"
        );
    }

    #[test]
    fn duplicate_segment_ids_are_rejected_before_payloads_are_staged() {
        let plan = RecordingPlan {
            media_path: PathBuf::from("fixtures/v0/recordings/demo.mp4"),
            media_hash: "sha256:0000000000000000000000000000000000000000000000000000000000000087"
                .to_owned(),
            duration_ms: 5200,
            file_size: 1234,
            skipped_frames: 0,
            segments: vec![
                RecordingSegment {
                    start_ms: 1000,
                    duration_ms: 500,
                    text: "Alpha  Recording".to_owned(),
                    kind: SegmentKind::AudioTranscript {
                        speaker_id: "speaker_01".to_owned(),
                        confidence: 0.91,
                    },
                },
                RecordingSegment {
                    start_ms: 1000,
                    duration_ms: 500,
                    text: " alpha recording ".to_owned(),
                    kind: SegmentKind::AudioTranscript {
                        speaker_id: "speaker_02".to_owned(),
                        confidence: 0.72,
                    },
                },
            ],
        };

        let err = build_segment_payloads(&plan).expect_err("duplicate segment ids are rejected");
        let message = err.to_string();
        assert!(
            message.contains("duplicate recording segment_id"),
            "unexpected error: {message}"
        );
        assert!(message.contains("recseg-"), "unexpected error: {message}");
    }

    #[test]
    fn frame_payload_uses_segment_start_as_timestamp_and_confidence_detail() {
        let plan = parse_fixture_plan(FIXTURE).expect("fixture parses");
        let segment = plan
            .segments
            .iter()
            .find(|segment| matches!(segment.kind, SegmentKind::FrameOcr { .. }))
            .expect("frame segment");

        let payload = build_segment_payload(&plan, segment).expect("payload builds");
        let value: serde_json::Value =
            serde_json::from_slice(&payload.payload_json).expect("payload JSON");

        assert_eq!(value["segment"]["track_kind"], "frame_ocr");
        assert_eq!(value["segment"]["start_ms"], 2000);
        assert_eq!(value["segment"]["text"], "screen shows gamma config");
        let confidence = value["segment"]["detail"]["confidence"]
            .as_f64()
            .expect("confidence is numeric");
        assert!((confidence - 0.82).abs() < 0.000_001);
        assert_eq!(value["segment"]["detail"]["timestamp_ms"], 2000);
    }

    #[test]
    fn frame_dedupe_is_chronological_for_unsorted_fixtures() {
        let plan = parse_fixture_plan(
            r#"{
              "media_path": "fixtures/v0/recordings/demo.mp4",
              "media_sha256": "sha256:0000000000000000000000000000000000000000000000000000000000000087",
              "duration_ms": 5200,
              "file_size": 1234,
              "frames": [
                {"timestamp_ms": 3000, "duration_ms": 1000, "confidence": 0.70, "text": "same screen"},
                {"timestamp_ms": 1000, "duration_ms": 1000, "confidence": 0.90, "text": "same screen"}
              ]
            }"#,
        )
        .expect("fixture parses");

        assert_eq!(plan.skipped_frames, 1);
        assert_eq!(plan.segments.len(), 1);
        assert_eq!(plan.segments[0].start_ms, 1000);
        assert_eq!(plan.segments[0].text, "same screen");
    }

    #[test]
    fn invalid_media_sha256_is_rejected() {
        for media_sha256 in [
            "sha256:000000000000000000000000000000000000000000000000000000000000008",
            "sha256:000000000000000000000000000000000000000000000000000000000000008G",
            "0000000000000000000000000000000000000000000000000000000000000087",
        ] {
            let raw = format!(
                r#"{{
                  "media_path": "fixtures/v0/recordings/demo.mp4",
                  "media_sha256": "{media_sha256}",
                  "duration_ms": 5200,
                  "file_size": 1234
                }}"#
            );

            assert!(
                parse_fixture_plan(&raw).is_err(),
                "accepted invalid media_sha256: {media_sha256}"
            );
        }
    }

    #[test]
    fn unknown_fixture_fields_are_rejected() {
        let result = parse_fixture_plan(
            r#"{
              "media_path": "fixtures/v0/recordings/demo.mp4",
              "media_sha256": "sha256:0000000000000000000000000000000000000000000000000000000000000087",
              "duration_ms": 5200,
              "file_size": 1234,
              "unexpected": true
            }"#,
        );

        assert!(result.is_err(), "accepted unknown fixture field");
    }

    #[test]
    fn whitespace_normalized_adjacent_frames_are_deduped() {
        let plan = parse_fixture_plan(
            r#"{
              "media_path": "fixtures/v0/recordings/demo.mp4",
              "media_sha256": "sha256:0000000000000000000000000000000000000000000000000000000000000087",
              "duration_ms": 5200,
              "file_size": 1234,
              "frames": [
                {"timestamp_ms": 1000, "duration_ms": 1000, "confidence": 0.82, "text": "screen   shows\nconfig"},
                {"timestamp_ms": 2000, "duration_ms": 1000, "confidence": 0.80, "text": " screen shows config "}
              ]
            }"#,
        )
        .expect("fixture parses");

        assert_eq!(plan.skipped_frames, 1);
        assert_eq!(plan.segments.len(), 1);
        assert_eq!(plan.segments[0].text, "screen shows config");
    }

    #[test]
    fn audio_sorts_before_frame_at_the_same_timestamp() {
        let plan = parse_fixture_plan(
            r#"{
              "media_path": "fixtures/v0/recordings/demo.mp4",
              "media_sha256": "sha256:0000000000000000000000000000000000000000000000000000000000000087",
              "duration_ms": 5200,
              "file_size": 1234,
              "audio": [
                {"start_ms": 1000, "duration_ms": 500, "speaker_id": "speaker_01", "confidence": 0.91, "text": "audio text"}
              ],
              "frames": [
                {"timestamp_ms": 1000, "duration_ms": 1000, "confidence": 0.82, "text": "frame text"}
              ]
            }"#,
        )
        .expect("fixture parses");

        assert_eq!(
            plan.segments
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>(),
            vec!["audio text", "frame text"],
        );
        assert!(matches!(
            plan.segments[0].kind,
            SegmentKind::AudioTranscript { .. }
        ));
        assert!(matches!(
            plan.segments[1].kind,
            SegmentKind::FrameOcr { .. }
        ));
    }

    #[test]
    fn empty_or_zero_duration_entries_are_skipped() {
        let plan = parse_fixture_plan(
            r#"{
              "media_path": "fixtures/v0/recordings/demo.mp4",
              "media_sha256": "sha256:0000000000000000000000000000000000000000000000000000000000000087",
              "duration_ms": 5200,
              "file_size": 1234,
              "audio": [
                {"start_ms": 0, "duration_ms": 0, "speaker_id": "speaker_01", "confidence": 0.91, "text": "zero audio"},
                {"start_ms": 500, "duration_ms": 500, "speaker_id": "speaker_02", "confidence": 0.80, "text": "   "},
                {"start_ms": 1000, "duration_ms": 500, "speaker_id": "speaker_03", "confidence": 0.70, "text": "kept audio"}
              ],
              "frames": [
                {"timestamp_ms": 1500, "duration_ms": 0, "confidence": 0.82, "text": "zero frame"},
                {"timestamp_ms": 2000, "duration_ms": 1000, "confidence": 0.80, "text": "   "},
                {"timestamp_ms": 2500, "duration_ms": 1000, "confidence": 0.70, "text": "kept frame"}
              ]
            }"#,
        )
        .expect("fixture parses");

        assert_eq!(plan.skipped_frames, 2);
        assert_eq!(
            plan.segments
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>(),
            vec!["kept audio", "kept frame"],
        );
    }

    #[test]
    fn kind_specific_fields_are_preserved() {
        let plan = parse_fixture_plan(
            r#"{
              "media_path": "fixtures/v0/recordings/demo.mp4",
              "media_sha256": "sha256:0000000000000000000000000000000000000000000000000000000000000087",
              "duration_ms": 5200,
              "file_size": 1234,
              "audio": [
                {"start_ms": 0, "duration_ms": 500, "speaker_id": "speaker_99", "confidence": 0.91, "text": "audio text"}
              ],
              "frames": [
                {"timestamp_ms": 1000, "duration_ms": 1000, "confidence": 0.82, "text": "frame text"}
              ]
            }"#,
        )
        .expect("fixture parses");

        match &plan.segments[0].kind {
            SegmentKind::AudioTranscript {
                speaker_id,
                confidence,
            } => {
                assert_eq!(speaker_id, "speaker_99");
                assert_eq!(*confidence, 0.91);
            }
            SegmentKind::FrameOcr { .. } => panic!("expected audio segment"),
        }
        match &plan.segments[1].kind {
            SegmentKind::FrameOcr { confidence } => assert_eq!(*confidence, 0.82),
            SegmentKind::AudioTranscript { .. } => panic!("expected frame segment"),
        }
    }
}
