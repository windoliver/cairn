//! Recording batch ingestion for `cairn ingest --recording`.

#![allow(
    dead_code,
    reason = "Task 2 stages recording planner pieces before runtime ingestion wiring."
)]

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
