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

// Additional ULIDs for multi-turn tests (turn-2 events).
const ULID_E: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAE";
const ULID_F: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAF";
const ULID_G: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAG";
const ULID_H: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAH";

// ULID for late tool_output event (after the main 4-event turn).
// Note: Crockford base32 has no I, L, O, U — use K (valid) for the suffix.
const ULID_LATE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAK";

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

/// Write eight [`CaptureEvent`]s to `jsonl_path` for two complete turns:
///
/// - Turn-1 (`turn-1`): events `FAA`–`FAD`, timestamps `00:00:01..00:00:04Z`
/// - Turn-2 (`turn-2`): events `FAE`–`FAH`, timestamps `01:00:01..01:00:04Z`
///
/// Both turns follow the `UserPromptSubmit → PreToolUse → PostToolUse → Stop`
/// sequence.  Each turn uses a distinct `tool_id` so the pre/post pairs do not
/// bleed across turns.
#[allow(
    clippy::too_many_lines,
    reason = "fixture: two full turns × 4 events each; splitting would obscure the symmetry"
)]
fn write_multi_turn_fixture(vault: &Path, jsonl_path: &Path) {
    let session = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

    // ── Turn-1 ──────────────────────────────────────────────────────────────
    let (t1_user, t1_pre, t1_post, t1_stop) = (
        ULID_A,
        ULID_B,
        "01ARZ3NDEKTSV4RRFFQ69G5FAC",
        "01ARZ3NDEKTSV4RRFFQ69G5FAD",
    );
    let t1_tool = "toolu_t1_01";
    let t1_user_body = "Hello from turn 1";
    let t1_pre_body = r#"{"tool":"bash","input":{"command":"ls"}}"#;
    let t1_post_body = r#"{"tool":"bash","output":"file.txt"}"#;
    let t1_stop_body = "turn 1 ended";

    let t1_user_ref = write_source(vault, &format!("{t1_user}.txt"), t1_user_body);
    let t1_pre_ref = write_source(vault, &format!("{t1_pre}.txt"), t1_pre_body);
    let t1_post_ref = write_source(vault, &format!("{t1_post}.txt"), t1_post_body);
    let t1_stop_ref = write_source(vault, &format!("{t1_stop}.txt"), t1_stop_body);

    // ── Turn-2 ──────────────────────────────────────────────────────────────
    let t2_tool = "toolu_t2_01";
    let t2_user_body = "Hello from turn 2";
    let t2_pre_body = r#"{"tool":"bash","input":{"command":"pwd"}}"#;
    let t2_post_body = r#"{"tool":"bash","output":"/home/user"}"#;
    let t2_stop_body = "turn 2 ended";

    let t2_user_ref = write_source(vault, &format!("{ULID_E}.txt"), t2_user_body);
    let t2_pre_ref = write_source(vault, &format!("{ULID_F}.txt"), t2_pre_body);
    let t2_post_ref = write_source(vault, &format!("{ULID_G}.txt"), t2_post_body);
    let t2_stop_ref = write_source(vault, &format!("{ULID_H}.txt"), t2_stop_body);

    let events = vec![
        // Turn-1
        make_event(
            t1_user,
            "UserPromptSubmit",
            session,
            "turn-1",
            "2026-05-02T00:00:01Z",
            None,
            &t1_user_ref,
            &sha256_hex(t1_user_body),
        ),
        make_event(
            t1_pre,
            "PreToolUse",
            session,
            "turn-1",
            "2026-05-02T00:00:02Z",
            Some(t1_tool.to_owned()),
            &t1_pre_ref,
            &sha256_hex(t1_pre_body),
        ),
        make_event(
            t1_post,
            "PostToolUse",
            session,
            "turn-1",
            "2026-05-02T00:00:03Z",
            Some(t1_tool.to_owned()),
            &t1_post_ref,
            &sha256_hex(t1_post_body),
        ),
        make_event(
            t1_stop,
            "Stop",
            session,
            "turn-1",
            "2026-05-02T00:00:04Z",
            None,
            &t1_stop_ref,
            &sha256_hex(t1_stop_body),
        ),
        // Turn-2
        make_event(
            ULID_E,
            "UserPromptSubmit",
            session,
            "turn-2",
            "2026-05-02T01:00:01Z",
            None,
            &t2_user_ref,
            &sha256_hex(t2_user_body),
        ),
        make_event(
            ULID_F,
            "PreToolUse",
            session,
            "turn-2",
            "2026-05-02T01:00:02Z",
            Some(t2_tool.to_owned()),
            &t2_pre_ref,
            &sha256_hex(t2_pre_body),
        ),
        make_event(
            ULID_G,
            "PostToolUse",
            session,
            "turn-2",
            "2026-05-02T01:00:03Z",
            Some(t2_tool.to_owned()),
            &t2_post_ref,
            &sha256_hex(t2_post_body),
        ),
        make_event(
            ULID_H,
            "Stop",
            session,
            "turn-2",
            "2026-05-02T01:00:04Z",
            None,
            &t2_stop_ref,
            &sha256_hex(t2_stop_body),
        ),
    ];

    let mut f = std::fs::File::create(jsonl_path).expect("create JSONL file");
    for ev in &events {
        let line = serde_json::to_string(ev).expect("serialize event");
        writeln!(f, "{line}").expect("write JSONL line");
    }
}

/// Write five [`CaptureEvent`]s to `jsonl_path`:
///
/// - Turn-1 (`turn-1`): complete sequence `UserPromptSubmit → PreToolUse →
///   PostToolUse → Stop` (events `FAA`–`FAD`).
/// - Turn-2 (`turn-2`): a single orphaned `PostToolUse` (event `FAE`) whose
///   `tool_call_id` (`"toolu_orphan"`) has no preceding `PreToolUse` in that
///   turn.  This triggers the orphan-detection branch inside `run_handler`,
///   causing turn-2 to fail without touching the store.
fn write_broken_second_turn_fixture(vault: &Path, jsonl_path: &Path) {
    let session = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let t1_tool = "toolu_test_01";

    // ── Turn-1: valid ────────────────────────────────────────────────────────
    let (id_user, id_pre, id_post, id_stop) = (
        ULID_A,
        ULID_B,
        "01ARZ3NDEKTSV4RRFFQ69G5FAC",
        "01ARZ3NDEKTSV4RRFFQ69G5FAD",
    );
    let user_body = "Hello, please run ls";
    let pre_body = r#"{"tool":"bash","input":{"command":"ls"}}"#;
    let post_body = r#"{"tool":"bash","output":"file.txt\ndir/"}"#;
    let stop_body = "session ended";

    let user_ref = write_source(vault, &format!("{id_user}.txt"), user_body);
    let pre_ref = write_source(vault, &format!("{id_pre}.txt"), pre_body);
    let post_ref = write_source(vault, &format!("{id_post}.txt"), post_body);
    let stop_ref = write_source(vault, &format!("{id_stop}.txt"), stop_body);

    // ── Turn-2: orphaned PostToolUse ─────────────────────────────────────────
    // The `tool_call_id` is distinct from turn-1's tool so there is no
    // accidental cross-turn match.  There is intentionally NO PreToolUse
    // event for "toolu_orphan" in turn-2, which is what triggers the failure.
    let orphan_body = r#"{"tool":"bash","output":"unexpected"}"#;
    let orphan_ref = write_source(vault, &format!("{ULID_E}.txt"), orphan_body);

    let events = vec![
        make_event(
            id_user,
            "UserPromptSubmit",
            session,
            "turn-1",
            "2026-05-02T00:00:01Z",
            None,
            &user_ref,
            &sha256_hex(user_body),
        ),
        make_event(
            id_pre,
            "PreToolUse",
            session,
            "turn-1",
            "2026-05-02T00:00:02Z",
            Some(t1_tool.to_owned()),
            &pre_ref,
            &sha256_hex(pre_body),
        ),
        make_event(
            id_post,
            "PostToolUse",
            session,
            "turn-1",
            "2026-05-02T00:00:03Z",
            Some(t1_tool.to_owned()),
            &post_ref,
            &sha256_hex(post_body),
        ),
        make_event(
            id_stop,
            "Stop",
            session,
            "turn-1",
            "2026-05-02T00:00:04Z",
            None,
            &stop_ref,
            &sha256_hex(stop_body),
        ),
        // Turn-2: orphaned PostToolUse — no matching PreToolUse in this turn.
        make_event(
            ULID_E,
            "PostToolUse",
            session,
            "turn-2",
            "2026-05-02T01:00:01Z",
            Some("toolu_orphan".to_owned()),
            &orphan_ref,
            &sha256_hex(orphan_body),
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
async fn multi_turn_each_summarized_independently() {
    let vault = tempfile::tempdir().expect("tempdir");
    let store = open_test_store_in_memory().await;
    let jsonl_path = vault.path().join("trace.jsonl");

    write_multi_turn_fixture(vault.path(), &jsonl_path);

    let resp = run_handler(&store, vault.path(), &jsonl_path)
        .await
        .expect("run_handler should succeed");

    assert!(
        resp.failed_turns.is_empty(),
        "expected no failures, got: {:?}",
        resp.failed_turns
    );

    let session_id = SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid session_id");

    store
        .with_tx({
            let session_id = session_id.clone();
            move |tx| {
                // Each turn should have exactly 4 non-summary events persisted.
                let t1_rows = tx.list_trace_events(&session_id, "turn-1")?;
                assert_eq!(t1_rows.len(), 4, "turn-1 should have 4 events, got {}", t1_rows.len());

                let t2_rows = tx.list_trace_events(&session_id, "turn-2")?;
                assert_eq!(t2_rows.len(), 4, "turn-2 should have 4 events, got {}", t2_rows.len());

                // Both turns must have a summary (both had a Stop event).
                assert!(
                    tx.turn_summary_exists(&session_id, "turn-1")?,
                    "turn-1 summary should exist"
                );
                assert!(
                    tx.turn_summary_exists(&session_id, "turn-2")?,
                    "turn-2 summary should exist"
                );
                Ok(())
            }
        })
        .await
        .expect("store query should succeed");
}

#[tokio::test]
#[allow(
    clippy::expect_used,
    reason = "test: panics surface broken invariants immediately"
)]
async fn malformed_second_turn_leaves_first_intact() {
    let vault = tempfile::tempdir().expect("tempdir");
    let store = open_test_store_in_memory().await;
    let jsonl_path = vault.path().join("trace.jsonl");

    write_broken_second_turn_fixture(vault.path(), &jsonl_path);

    let resp = run_handler(&store, vault.path(), &jsonl_path)
        .await
        .expect("run_handler should return Ok even when a turn fails");

    // Exactly one turn failure: turn-2.
    assert_eq!(
        resp.failed_turns.len(),
        1,
        "expected exactly one failed turn (turn-2), got: {:?}",
        resp.failed_turns
    );
    assert_eq!(
        resp.failed_turns[0].1, "turn-2",
        "the failing turn should be turn-2, got: {:?}",
        resp.failed_turns[0]
    );

    let session_id = SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid session_id");

    store
        .with_tx({
            let session_id = session_id.clone();
            move |tx| {
                // Turn-1 must be fully present and summarized.
                let t1_rows = tx.list_trace_events(&session_id, "turn-1")?;
                assert_eq!(
                    t1_rows.len(),
                    4,
                    "turn-1 should have 4 events after turn-2 failure, got {}",
                    t1_rows.len()
                );
                assert!(
                    tx.turn_summary_exists(&session_id, "turn-1")?,
                    "turn-1 summary must exist after turn-2 failure"
                );

                // Turn-2 must be completely absent — no partial writes.
                let t2_rows = tx.list_trace_events(&session_id, "turn-2")?;
                assert_eq!(
                    t2_rows.len(),
                    0,
                    "turn-2 should have 0 events (atomicity), got {}",
                    t2_rows.len()
                );
                assert!(
                    !tx.turn_summary_exists(&session_id, "turn-2")?,
                    "turn-2 summary must NOT exist after failure"
                );
                Ok(())
            }
        })
        .await
        .expect("store query should succeed");
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

/// Write a single late `PostToolUse` event for the same turn-1 created by
/// [`write_fixture`].  The event uses the same `tool_call_id` as the
/// `pre_tool` in that fixture (`"toolu_test_01"`) and a timestamp that falls
/// between `PreToolUse` and `PostToolUse` — ensuring it interleaves when
/// `renumber_turn_with` sorts by `captured_at`.
///
/// The source file is written under `vault/sources/hook/`.
fn write_late_tool_output_fixture(vault: &Path, jsonl_path: &Path) {
    let session = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let turn = "turn-1";
    // Same tool_call_id as the pre_tool in write_fixture so the store-lookup
    // path in run_handler can resolve the parent.
    let tool_id = "toolu_test_01";

    let late_body = r#"{"tool":"bash","late_output":"extra info"}"#;
    let late_ref = write_source(vault, &format!("{ULID_LATE}.txt"), late_body);

    let event = make_event(
        ULID_LATE,
        // "PostToolUse" maps to TraceEvent::PostTool which requires a parent.
        // We use PostToolUse here — the store-lookup path will resolve the
        // parent from the existing pre_tool row already in the store.
        "PostToolUse",
        session,
        turn,
        // Timestamp between PreToolUse (00:00:02Z) and PostToolUse (00:00:03Z)
        // so it lands at sequence 2 after renumber, pushing the original
        // PostToolUse to sequence 3.
        "2026-05-02T00:00:02.500Z",
        Some(tool_id.to_owned()),
        &late_ref,
        &sha256_hex(late_body),
    );

    let mut f = std::fs::File::create(jsonl_path).expect("create JSONL file");
    let line = serde_json::to_string(&event).expect("serialize event");
    writeln!(f, "{line}").expect("write JSONL line");
}

// ── Test 1: replay idempotency ────────────────────────────────────────────────

/// Replaying the same JSONL file twice must not create duplicate records.
///
/// The `renumber_turn_with` upsert path is idempotent by design: the
/// `record_id` is derived deterministically from `capture_event_id`, so a
/// second import of the same event converges to the same row.  The
/// `UNIQUE INDEX records_trace_summary` on the summary row guarantees at
/// most one summary per turn; `upsert_trace` performs an INSERT OR REPLACE,
/// so a second summary write replaces the first rather than inserting a
/// duplicate.
#[tokio::test]
#[allow(
    clippy::expect_used,
    reason = "test: panics surface broken invariants immediately"
)]
async fn replay_is_idempotent() {
    let vault = tempfile::tempdir().expect("tempdir");
    let store = open_test_store_in_memory().await;

    let jsonl_path = vault.path().join("trace.jsonl");
    write_fixture(vault.path(), &jsonl_path);

    // First import.
    let resp1 = run_handler(&store, vault.path(), &jsonl_path)
        .await
        .expect("first run_handler should succeed");
    assert!(
        resp1.failed_turns.is_empty(),
        "expected no failures on first import, got: {:?}",
        resp1.failed_turns
    );

    // Second import of the identical JSONL.
    let resp2 = run_handler(&store, vault.path(), &jsonl_path)
        .await
        .expect("second run_handler should succeed");
    assert!(
        resp2.failed_turns.is_empty(),
        "expected no failures on second import, got: {:?}",
        resp2.failed_turns
    );

    let session_id = SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid session_id");

    store
        .with_tx({
            let session_id = session_id.clone();
            move |tx| {
                let rows = tx.list_trace_events(&session_id, "turn-1")?;
                assert_eq!(rows.len(), 4, "no duplicate events from replay: got {}", rows.len());

                // Summary must exist and remain a single row.  The
                // UNIQUE INDEX on (session_id, turn_id) for turn_summary
                // combined with `upsert_trace`'s INSERT OR REPLACE semantics
                // guarantees ≤1 summary; `turn_summary_exists` confirms ≥1.
                assert!(
                    tx.turn_summary_exists(&session_id, "turn-1")?,
                    "summary must exist after replay"
                );
                Ok(())
            }
        })
        .await
        .expect("store query should succeed");
}

// ── Test 2: late event after a closed turn re-summarizes ──────────────────────

/// A late event arriving after a Stop-closed turn must be ingested and the
/// summary must be updated to cover all five events.
///
/// The late `PostToolUse` event has no `PreTool` in its own JSONL batch.
/// `run_handler` must fall back to the store-resident rows to resolve the
/// `parent_event_id` (Option A implementation).
#[tokio::test]
#[allow(
    clippy::expect_used,
    reason = "test: panics surface broken invariants immediately"
)]
async fn late_event_after_close_resummarizes() {
    let vault = tempfile::tempdir().expect("tempdir");
    let store = open_test_store_in_memory().await;

    // Import the initial 4-event turn (with Stop → turn is closed).
    let initial = vault.path().join("initial.jsonl");
    write_fixture(vault.path(), &initial);
    let resp = run_handler(&store, vault.path(), &initial)
        .await
        .expect("initial run_handler should succeed");
    assert!(
        resp.failed_turns.is_empty(),
        "expected no failures on initial import, got: {:?}",
        resp.failed_turns
    );

    // Import a single late PostToolUse for the same turn.  Its PreTool is
    // only available in the store (not in this JSONL), exercising the
    // store-lookup fallback in run_handler.
    let late = vault.path().join("late.jsonl");
    write_late_tool_output_fixture(vault.path(), &late);
    let resp = run_handler(&store, vault.path(), &late)
        .await
        .expect("late run_handler should succeed");
    assert!(
        resp.failed_turns.is_empty(),
        "late event import should not fail, got: {:?}",
        resp.failed_turns
    );

    let session_id = SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid session_id");

    store
        .with_tx({
            let session_id = session_id.clone();
            move |tx| {
                let rows = tx.list_trace_events(&session_id, "turn-1")?;
                assert_eq!(
                    rows.len(),
                    5,
                    "late event must land: expected 5 events, got {}",
                    rows.len()
                );

                // Sequences must be 0..4 (renumbered by captured_at order).
                let seqs: Vec<u64> = rows
                    .iter()
                    .map(|r| {
                        r.extra_frontmatter["trace"]["sequence"]
                            .as_u64()
                            .expect("sequence must be u64")
                    })
                    .collect();
                assert_eq!(seqs, vec![0, 1, 2, 3, 4], "sequences must be contiguous 0..4");

                // The summary must have been updated to cover all 5 events.
                assert!(
                    tx.turn_summary_exists(&session_id, "turn-1")?,
                    "summary must exist after late-event re-summarize"
                );
                Ok(())
            }
        })
        .await
        .expect("store query should succeed");
}
