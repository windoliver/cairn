//! Integration tests for `cairn capture_trace` JSONL parser (issue #77).

use std::io::Write as _;
use std::path::Path;

use cairn_cli::verbs::capture_trace::{read_jsonl_events, run_handler};
use cairn_core::domain::{
    ActorChainEntry, ChainRole, CaptureEvent, CaptureEventId, CaptureMode, CapturePayload,
    CaptureRefs, Identity, PayloadHash, Rfc3339Timestamp, SessionId, SourceFamily,
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

// ── run_handler integration tests ─────────────────────────────────────────────

/// Compute the lowercase hex SHA-256 of `text`.
fn sha256_hex(text: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    format!("{:x}", h.finalize())
}

/// Write `content` to `vault/sources/hook/<filename>`, creating the
/// `sources/hook/` directory if needed, and return the
/// `vault_root`-relative path (`"sources/hook/<filename>"`).
fn write_source(vault: &Path, filename: &str, content: &str) -> String {
    let dir = vault.join("sources").join("hook");
    std::fs::create_dir_all(&dir).expect("create sources/hook dir");
    let abs = dir.join(filename);
    std::fs::write(&abs, content).expect("write source file");
    format!("sources/hook/{filename}")
}

/// Build a [`CaptureEvent`] for a `Hook` payload. The `payload_ref` and
/// `payload_hash` are derived from `body_content` which is written to
/// `vault/sources/hook/<event_id>.txt` by the caller.
///
/// `tool_id` is placed in `refs.tool_id`, satisfying the `tool_call_id`
/// requirement for `PreTool`/`PostTool`.
#[allow(clippy::too_many_arguments)]
fn make_event(
    event_id: &str,
    hook_name: &str,
    session_id: &str,
    turn_id: &str,
    timestamp: &str,
    tool_id: Option<String>,
    payload_ref: &str,
    payload_hash_hex: &str,
) -> CaptureEvent {
    let sensor =
        Identity::parse("snr:local:hook:cc-session:v1").expect("invariant: valid sensor id");
    let hash_str = format!("sha256:{payload_hash_hex}");
    CaptureEvent {
        event_id: CaptureEventId::parse(event_id).expect("invariant: valid ULID"),
        sensor_id: sensor.clone(),
        // Auto mode with sensor as Author — the natural mode for hook events.
        // MemoryRecord::validate now allows sensor authors on Trace records
        // (brief §5.0: sensors are the canonical authors of raw captures).
        capture_mode: CaptureMode::Auto,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: sensor,
            at: Rfc3339Timestamp::parse(timestamp).expect("invariant: valid RFC-3339"),
        }],
        refs: Some(CaptureRefs {
            session_id: Some(session_id.to_owned()),
            turn_id: Some(turn_id.to_owned()),
            tool_id,
        }),
        payload_hash: PayloadHash::parse(&hash_str).expect("invariant: valid sha256 hash"),
        payload_ref: payload_ref.to_owned(),
        captured_at: Rfc3339Timestamp::parse(timestamp).expect("invariant: valid RFC-3339"),
        payload: CapturePayload::Hook {
            hook_name: hook_name.to_owned(),
            tool_name: None,
        },
        source_family: SourceFamily::Hook,
    }
}

/// Open a fresh in-memory [`SqliteMemoryStore`].
async fn open_test_store_in_memory() -> cairn_store_sqlite::SqliteMemoryStore {
    cairn_store_sqlite::open_in_memory()
        .await
        .expect("open in-memory store")
}

/// Write four `CaptureEvent`s to `jsonl_path` for a single turn:
/// `UserPromptSubmit` → `PreToolUse` → `PostToolUse` → `Stop`.
///
/// Source files are written under `vault.join("sources/hook/")`.
///
/// # Session / turn / event-id constants
/// - Session: `01ARZ3NDEKTSV4RRFFQ69G5FAV`
/// - Turn: `turn-1`
/// - Event ids: `01ARZ3NDEKTSV4RRFFQ69G5FAA` … `FAD`
/// - Tool call id: `toolu_test_01`
fn write_fixture(vault: &Path, jsonl_path: &Path) {
    // ULIDs for each event.
    let id_user = "01ARZ3NDEKTSV4RRFFQ69G5FAA";
    let id_pre  = "01ARZ3NDEKTSV4RRFFQ69G5FAB";
    let id_post = "01ARZ3NDEKTSV4RRFFQ69G5FAC";
    let id_stop = "01ARZ3NDEKTSV4RRFFQ69G5FAD";

    let session = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let turn    = "turn-1";
    let tool_id = "toolu_test_01";

    // Write sources and compute hashes.
    let user_body  = "Hello, please run ls";
    let pre_body   = r#"{"tool":"bash","input":{"command":"ls"}}"#;
    let post_body  = r#"{"tool":"bash","output":"file.txt\ndir/"}"#;
    let stop_body  = "session ended";

    let user_ref  = write_source(vault, &format!("{id_user}.txt"), user_body);
    let pre_ref   = write_source(vault, &format!("{id_pre}.txt"),  pre_body);
    let post_ref  = write_source(vault, &format!("{id_post}.txt"), post_body);
    let stop_ref  = write_source(vault, &format!("{id_stop}.txt"), stop_body);

    let events = vec![
        make_event(
            id_user, "UserPromptSubmit", session, turn,
            "2026-05-02T00:00:01Z",
            None,
            &user_ref,
            &sha256_hex(user_body),
        ),
        make_event(
            id_pre, "PreToolUse", session, turn,
            "2026-05-02T00:00:02Z",
            Some(tool_id.to_owned()),
            &pre_ref,
            &sha256_hex(pre_body),
        ),
        make_event(
            id_post, "PostToolUse", session, turn,
            "2026-05-02T00:00:03Z",
            Some(tool_id.to_owned()),
            &post_ref,
            &sha256_hex(post_body),
        ),
        make_event(
            id_stop, "Stop", session, turn,
            "2026-05-02T00:00:04Z",
            None,
            &stop_ref,
            &sha256_hex(stop_body),
        ),
    ];

    let mut f = std::fs::File::create(jsonl_path).expect("create JSONL file");
    for ev in &events {
        let line = serde_json::to_string(ev).expect("serialize event");
        writeln!(f, "{line}").expect("write JSONL line");
    }
}

#[tokio::test]
#[allow(
    clippy::expect_used,
    reason = "test: panics surface broken invariants immediately"
)]
async fn capture_trace_single_turn_persists_and_summarizes() {
    let vault = tempfile::tempdir().expect("tempdir");
    let store = open_test_store_in_memory().await;
    let jsonl_path = vault.path().join("trace.jsonl");

    write_fixture(vault.path(), &jsonl_path);

    let resp = run_handler(&store, vault.path(), &jsonl_path)
        .await
        .expect("run_handler should succeed");

    assert!(
        resp.failed_turns.is_empty(),
        "expected no failures, got: {:?}",
        resp.failed_turns
    );

    // Verify the 4 events were persisted and a summary was written.
    store
        .with_tx(|tx| {
            let session_id =
                SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid session_id");
            let rows = tx.list_trace_events(&session_id, "turn-1")?;
            assert_eq!(rows.len(), 4, "expected 4 trace events, got {}", rows.len());
            assert!(
                tx.turn_summary_exists(&session_id, "turn-1")?,
                "turn summary should exist after Stop event"
            );
            Ok(())
        })
        .await
        .expect("store query should succeed");
}
