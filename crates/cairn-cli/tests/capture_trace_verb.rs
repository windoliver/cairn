//! Integration tests for `cairn capture_trace` JSONL parser (issue #77).

use std::io::Write as _;

use cairn_cli::verbs::capture_trace::read_jsonl_events;
use cairn_core::domain::{
    ActorChainEntry, ChainRole, CaptureEvent, CaptureEventId, CaptureMode, CapturePayload,
    CaptureRefs, Identity, PayloadHash, Rfc3339Timestamp, SourceFamily,
};

/// Build a minimal valid `Hook` [`CaptureEvent`] for use in tests.
///
/// Uses `CaptureMode::Auto` + `SourceFamily::Hook` + sensor
/// `snr:local:hook:cc-session:v1` — the canonical P0 hook sensor whose
/// label satisfies [`cairn_core::domain::validate_label`].
fn make_hook_event(
    event_id: &str,
    hook_name: &str,
    session_id: &str,
    turn_id: &str,
    timestamp: &str,
) -> CaptureEvent {
    let sensor =
        Identity::parse("snr:local:hook:cc-session:v1").expect("invariant: valid sensor id");
    CaptureEvent {
        event_id: CaptureEventId::parse(event_id).expect("invariant: valid ULID"),
        sensor_id: sensor.clone(),
        capture_mode: CaptureMode::Auto,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: sensor,
            at: Rfc3339Timestamp::parse(timestamp).expect("invariant: valid RFC-3339"),
        }],
        refs: Some(CaptureRefs {
            session_id: Some(session_id.to_owned()),
            turn_id: Some(turn_id.to_owned()),
            tool_id: None,
        }),
        payload_hash: PayloadHash::parse(
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        )
        .expect("invariant: valid sha256 of empty bytes"),
        payload_ref: "sources/hook/placeholder.json".into(),
        captured_at: Rfc3339Timestamp::parse(timestamp).expect("invariant: valid RFC-3339"),
        payload: CapturePayload::Hook {
            hook_name: hook_name.to_owned(),
            tool_name: None,
        },
        source_family: SourceFamily::Hook,
    }
}

// Three distinct ULIDs for the parametric tests.
const ULID_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAA";
const ULID_B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAB";
const ULID_C: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAC";

#[tokio::test]
async fn parses_jsonl_into_events() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("trace.jsonl");

    let events_src = [
        make_hook_event(
            ULID_A,
            "UserPromptSubmit",
            "sess-1",
            "turn-1",
            "2026-04-27T00:00:01Z",
        ),
        make_hook_event(
            ULID_B,
            "PreToolUse",
            "sess-1",
            "turn-1",
            "2026-04-27T00:00:02Z",
        ),
        make_hook_event(
            ULID_C,
            "PostToolUse",
            "sess-1",
            "turn-1",
            "2026-04-27T00:00:03Z",
        ),
    ];

    let mut f = std::fs::File::create(&path).expect("create file");
    for event in &events_src {
        let line = serde_json::to_string(event).expect("serialize event");
        writeln!(f, "{line}").expect("write line");
    }
    drop(f);

    let parsed = read_jsonl_events(&path).await.expect("read_jsonl_events");
    assert_eq!(parsed.len(), 3);
    assert_eq!(parsed[0].event_id.as_str(), ULID_A);
    assert_eq!(parsed[1].event_id.as_str(), ULID_B);
    assert_eq!(parsed[2].event_id.as_str(), ULID_C);
}

#[tokio::test]
async fn skips_blank_lines() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("trace.jsonl");

    let event = make_hook_event(
        ULID_A,
        "UserPromptSubmit",
        "sess-1",
        "turn-1",
        "2026-04-27T00:00:01Z",
    );
    let line = serde_json::to_string(&event).expect("serialize event");

    let mut f = std::fs::File::create(&path).expect("create file");
    writeln!(f).expect("blank line before");
    writeln!(f, "{line}").expect("event line");
    writeln!(f).expect("blank line after");
    drop(f);

    let parsed = read_jsonl_events(&path).await.expect("read_jsonl_events");
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].event_id.as_str(), ULID_A);
}

#[tokio::test]
async fn malformed_line_returns_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("trace.jsonl");
    std::fs::write(&path, "{not valid json}\n").expect("write file");

    let result = read_jsonl_events(&path).await;
    assert!(result.is_err(), "expected an error for malformed JSONL");
    let msg = format!("{:#}", result.unwrap_err());
    assert!(
        msg.contains("parse CaptureEvent"),
        "error should mention parse CaptureEvent, got: {msg}"
    );
}
