//! Integration tests for `cairn capture_trace` JSONL parser (issue #77).

use std::io::Write as _;
use std::path::Path;

use cairn_cli::verbs::capture_trace::{
    read_jsonl_events, run_blocks_handler, run_events_handler, run_events_handler_with_scope,
    run_handler,
};
use cairn_core::domain::{
    ActorChainEntry, CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, CaptureRefs,
    ChainRole, Identity, PayloadHash, Rfc3339Timestamp, ScopeTuple, SessionId, SourceFamily,
    TerminalContext,
};
use sha2::{Digest as _, Sha256};
use ulid::Ulid;

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

const ULID_FULL_USER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAN";
const ULID_FULL_AGENT: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAP";
const ULID_FULL_PRE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAQ";
const ULID_FULL_TOOL_OUTPUT: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAR";
const ULID_FULL_POST: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAS";
const ULID_FULL_STOP: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAT";

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
    write_source_for_family(vault, "hook", filename, content)
}

fn write_source_for_family(vault: &Path, family: &str, filename: &str, content: &str) -> String {
    let dir = vault.join("sources").join(family);
    std::fs::create_dir_all(&dir).expect("create sources family dir");
    let abs = dir.join(filename);
    std::fs::write(&abs, content).expect("write source file");
    format!("sources/{family}/{filename}")
}

fn stable_turn_id_for_blocks(session_id: &str, raw_blocks: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b"cairn:capture-trace:blocks:import:v1\0");
    h.update(session_id.as_bytes());
    h.update([0]);
    h.update(raw_blocks);
    h.update([0]);
    let digest = h.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    format!("trace-blocks-{}", Ulid::from_bytes(bytes))
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

#[allow(clippy::too_many_arguments)]
fn make_terminal_event(
    event_id: &str,
    session_id: &str,
    turn_id: &str,
    timestamp: &str,
    tool_id: &str,
    payload_ref: &str,
    payload_hash_hex: &str,
    context: TerminalContext,
) -> CaptureEvent {
    let sensor =
        Identity::parse("snr:local:terminal:default:v1").expect("invariant: valid sensor id");
    let hash_str = format!("sha256:{payload_hash_hex}");
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
            tool_id: Some(tool_id.to_owned()),
        }),
        payload_hash: PayloadHash::parse(&hash_str).expect("invariant: valid sha256 hash"),
        payload_ref: payload_ref.to_owned(),
        captured_at: Rfc3339Timestamp::parse(timestamp).expect("invariant: valid RFC-3339"),
        payload: CapturePayload::Terminal {
            command: "bash -lc 'cargo test -p cairn-core'".into(),
            exit_code: Some(0),
            context: Some(context),
        },
        source_family: SourceFamily::Terminal,
    }
}

#[allow(clippy::too_many_arguments)]
fn make_agent_event(
    event_id: &str,
    session_id: &str,
    turn_id: &str,
    timestamp: &str,
    payload_ref: &str,
    payload_hash_hex: &str,
) -> CaptureEvent {
    let sensor =
        Identity::parse("snr:local:proactive:codex:v1").expect("invariant: valid sensor id");
    let author =
        Identity::parse("agt:codex:gpt-5:main:v1").expect("invariant: valid agent identity");
    let hash_str = format!("sha256:{payload_hash_hex}");
    CaptureEvent {
        event_id: CaptureEventId::parse(event_id).expect("invariant: valid ULID"),
        sensor_id: sensor,
        capture_mode: CaptureMode::Proactive,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: author,
            at: Rfc3339Timestamp::parse(timestamp).expect("invariant: valid RFC-3339"),
        }],
        refs: Some(CaptureRefs {
            session_id: Some(session_id.to_owned()),
            turn_id: Some(turn_id.to_owned()),
            tool_id: None,
        }),
        payload_hash: PayloadHash::parse(&hash_str).expect("invariant: valid sha256 hash"),
        payload_ref: payload_ref.to_owned(),
        captured_at: Rfc3339Timestamp::parse(timestamp).expect("invariant: valid RFC-3339"),
        payload: CapturePayload::Proactive {
            kind: "agent_message".into(),
            rationale: "assistant trace message".into(),
        },
        source_family: SourceFamily::Proactive,
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
    let events = one_turn_capture_events(vault);

    let mut f = std::fs::File::create(jsonl_path).expect("create JSONL file");
    for ev in &events {
        let line = serde_json::to_string(ev).expect("serialize event");
        writeln!(f, "{line}").expect("write JSONL line");
    }
}

fn one_turn_capture_events(vault: &Path) -> Vec<CaptureEvent> {
    // ULIDs for each event.
    let id_user = "01ARZ3NDEKTSV4RRFFQ69G5FAA";
    let id_pre = "01ARZ3NDEKTSV4RRFFQ69G5FAB";
    let id_post = "01ARZ3NDEKTSV4RRFFQ69G5FAC";
    let id_stop = "01ARZ3NDEKTSV4RRFFQ69G5FAD";

    let session = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let turn = "turn-1";
    let tool_id = "toolu_test_01";

    // Write sources and compute hashes.
    let user_body = "Hello, please run ls";
    let pre_body = r#"{"tool":"bash","input":{"command":"ls"}}"#;
    let post_body = r#"{"tool":"bash","output":"file.txt\ndir/"}"#;
    let stop_body = "session ended";

    let user_ref = write_source(vault, &format!("{id_user}.txt"), user_body);
    let pre_ref = write_source(vault, &format!("{id_pre}.txt"), pre_body);
    let post_ref = write_source(vault, &format!("{id_post}.txt"), post_body);
    let stop_ref = write_source(vault, &format!("{id_stop}.txt"), stop_body);

    vec![
        make_event(
            id_user,
            "UserPromptSubmit",
            session,
            turn,
            "2026-05-02T00:00:01Z",
            None,
            &user_ref,
            &sha256_hex(user_body),
        ),
        make_event(
            id_pre,
            "PreToolUse",
            session,
            turn,
            "2026-05-02T00:00:02Z",
            Some(tool_id.to_owned()),
            &pre_ref,
            &sha256_hex(pre_body),
        ),
        make_event(
            id_post,
            "PostToolUse",
            session,
            turn,
            "2026-05-02T00:00:03Z",
            Some(tool_id.to_owned()),
            &post_ref,
            &sha256_hex(post_body),
        ),
        make_event(
            id_stop,
            "Stop",
            session,
            turn,
            "2026-05-02T00:00:04Z",
            None,
            &stop_ref,
            &sha256_hex(stop_body),
        ),
    ]
}

/// Same four events as [`write_fixture`] but emitted in JSONL with the
/// `PostToolUse` line *before* its matching `PreToolUse`. Used to verify
/// that in-batch parent resolution is order-insensitive.
fn write_fixture_post_before_pre(vault: &Path, jsonl_path: &Path) {
    let id_user = "01ARZ3NDEKTSV4RRFFQ69G5FAA";
    let id_pre = "01ARZ3NDEKTSV4RRFFQ69G5FAB";
    let id_post = "01ARZ3NDEKTSV4RRFFQ69G5FAC";
    let id_stop = "01ARZ3NDEKTSV4RRFFQ69G5FAD";

    let session = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let turn = "turn-1";
    let tool_id = "toolu_test_01";

    let user_body = "Hello, please run ls";
    let pre_body = r#"{"tool":"bash","input":{"command":"ls"}}"#;
    let post_body = r#"{"tool":"bash","output":"file.txt\ndir/"}"#;
    let stop_body = "session ended";

    let user_ref = write_source(vault, &format!("{id_user}.txt"), user_body);
    let pre_ref = write_source(vault, &format!("{id_pre}.txt"), pre_body);
    let post_ref = write_source(vault, &format!("{id_post}.txt"), post_body);
    let stop_ref = write_source(vault, &format!("{id_stop}.txt"), stop_body);

    // Emission order: user → POST → PRE → stop. captured_at remains
    // canonical (PRE@02Z, POST@03Z) so renumber still sorts correctly.
    let events = vec![
        make_event(
            id_user,
            "UserPromptSubmit",
            session,
            turn,
            "2026-05-02T00:00:01Z",
            None,
            &user_ref,
            &sha256_hex(user_body),
        ),
        make_event(
            id_post,
            "PostToolUse",
            session,
            turn,
            "2026-05-02T00:00:03Z",
            Some(tool_id.to_owned()),
            &post_ref,
            &sha256_hex(post_body),
        ),
        make_event(
            id_pre,
            "PreToolUse",
            session,
            turn,
            "2026-05-02T00:00:02Z",
            Some(tool_id.to_owned()),
            &pre_ref,
            &sha256_hex(pre_body),
        ),
        make_event(
            id_stop,
            "Stop",
            session,
            turn,
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
                assert_eq!(
                    t1_rows.len(),
                    4,
                    "turn-1 should have 4 events, got {}",
                    t1_rows.len()
                );

                let t2_rows = tx.list_trace_events(&session_id, "turn-2")?;
                assert_eq!(
                    t2_rows.len(),
                    4,
                    "turn-2 should have 4 events, got {}",
                    t2_rows.len()
                );

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

/// Snapshot test: pin the JSON shape of [`CaptureTraceResponse`].
///
/// `trace_id` is a fresh ULID generated server-side per invocation, so the
/// value is replaced with a stable placeholder before snapshotting.
/// `failed_turns` is a `Vec<(String, String, String)>` which serde encodes
/// as JSON arrays — that shape is what we are pinning.
#[tokio::test]
#[allow(
    clippy::expect_used,
    reason = "test: panics surface broken invariants immediately"
)]
async fn capture_trace_response_envelope_snapshot() {
    let vault = tempfile::tempdir().expect("tempdir");
    let store = open_test_store_in_memory().await;
    let jsonl_path = vault.path().join("trace.jsonl");

    write_fixture(vault.path(), &jsonl_path);

    let resp = run_handler(&store, vault.path(), &jsonl_path)
        .await
        .expect("run_handler should succeed");

    // `trace_id` is a fresh ULID per invocation — substitute a stable
    // placeholder before snapshotting so the snapshot pins shape, not value.
    let mut value = serde_json::to_value(&resp).expect("serialize CaptureTraceResponse");
    value["trace_id"] = serde_json::Value::String("[trace_id]".into());

    insta::assert_json_snapshot!("capture_trace_response_envelope", value);
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

#[tokio::test]
#[allow(
    clippy::expect_used,
    reason = "test: panics surface broken invariants immediately"
)]
async fn capture_trace_imports_events_from_memory_vector() {
    let vault = tempfile::tempdir().expect("tempdir");
    let store = open_test_store_in_memory().await;
    let events = one_turn_capture_events(vault.path());

    let resp = run_events_handler(&store, vault.path(), events)
        .await
        .expect("run_events_handler should succeed");

    assert!(
        resp.failed_turns.is_empty(),
        "expected no failures, got: {:?}",
        resp.failed_turns
    );
    assert!(
        !resp.policy_trace.is_empty(),
        "vector import should run the shared policy filter path"
    );

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

#[tokio::test]
#[allow(
    clippy::expect_used,
    reason = "test: panics surface broken invariants immediately"
)]
async fn capture_trace_imports_events_from_memory_vector_with_scope() {
    let vault = tempfile::tempdir().expect("tempdir");
    let store = open_test_store_in_memory().await;
    let events = one_turn_capture_events(vault.path());
    let scope_binding = ScopeTuple {
        tenant: Some("tenant-vector".to_owned()),
        workspace: Some("workspace-vector".to_owned()),
        entity: Some("capture-trace-vector".to_owned()),
        ..ScopeTuple::default()
    };

    let resp = run_events_handler_with_scope(&store, vault.path(), events, scope_binding)
        .await
        .expect("run_events_handler_with_scope should succeed");

    assert!(
        resp.failed_turns.is_empty(),
        "expected no failures, got: {:?}",
        resp.failed_turns
    );
    assert!(
        !resp.policy_trace.is_empty(),
        "scoped vector import should run the shared policy filter path"
    );

    store
        .with_tx(|tx| {
            let session_id =
                SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid session_id");
            let rows = tx.list_trace_events(&session_id, "turn-1")?;
            assert_eq!(rows.len(), 4, "expected 4 trace events, got {}", rows.len());
            for row in rows {
                assert_eq!(row.scope.tenant.as_deref(), Some("tenant-vector"));
                assert_eq!(row.scope.workspace.as_deref(), Some("workspace-vector"));
                assert_eq!(row.scope.entity.as_deref(), Some("capture-trace-vector"));
            }
            Ok(())
        })
        .await
        .expect("store query should succeed");
}

#[tokio::test]
#[allow(
    clippy::expect_used,
    clippy::too_many_lines,
    reason = "test: panics surface broken invariants immediately"
)]
async fn capture_trace_imports_full_trace_scope_in_one_turn() {
    let vault = tempfile::tempdir().expect("tempdir");
    let store = open_test_store_in_memory().await;
    let jsonl_path = vault.path().join("trace.jsonl");

    let session = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let turn = "turn-full-scope";
    let tool_id = "toolu_full_scope_01";

    let user_body = "Please run the targeted tests.";
    let agent_body = "I will run the targeted core tests now.";
    let pre_body = r#"{"tool":"bash","input":{"command":"cargo test -p cairn-core"}}"#;
    let terminal_body = "\x1b[32mrunning 3 tests\x1b[0m\nall passed\n";
    let post_body = r#"{"tool":"bash","exit_code":0}"#;
    let stop_body = "turn ended";

    let user_ref = write_source(vault.path(), &format!("{ULID_FULL_USER}.txt"), user_body);
    let agent_ref = write_source_for_family(
        vault.path(),
        "proactive",
        &format!("{ULID_FULL_AGENT}.txt"),
        agent_body,
    );
    let pre_ref = write_source(vault.path(), &format!("{ULID_FULL_PRE}.txt"), pre_body);
    let terminal_ref = write_source_for_family(
        vault.path(),
        "terminal",
        &format!("{ULID_FULL_TOOL_OUTPUT}.txt"),
        terminal_body,
    );
    let post_ref = write_source(vault.path(), &format!("{ULID_FULL_POST}.txt"), post_body);
    let stop_ref = write_source(vault.path(), &format!("{ULID_FULL_STOP}.txt"), stop_body);

    let events = vec![
        make_event(
            ULID_FULL_USER,
            "UserPromptSubmit",
            session,
            turn,
            "2026-05-02T02:00:01Z",
            None,
            &user_ref,
            &sha256_hex(user_body),
        ),
        make_agent_event(
            ULID_FULL_AGENT,
            session,
            turn,
            "2026-05-02T02:00:02Z",
            &agent_ref,
            &sha256_hex(agent_body),
        ),
        make_event(
            ULID_FULL_PRE,
            "PreToolUse",
            session,
            turn,
            "2026-05-02T02:00:03Z",
            Some(tool_id.to_owned()),
            &pre_ref,
            &sha256_hex(pre_body),
        ),
        make_terminal_event(
            ULID_FULL_TOOL_OUTPUT,
            session,
            turn,
            "2026-05-02T02:00:04Z",
            tool_id,
            &terminal_ref,
            &sha256_hex(terminal_body),
            TerminalContext::InteractiveTty,
        ),
        make_event(
            ULID_FULL_POST,
            "PostToolUse",
            session,
            turn,
            "2026-05-02T02:00:05Z",
            Some(tool_id.to_owned()),
            &post_ref,
            &sha256_hex(post_body),
        ),
        make_event(
            ULID_FULL_STOP,
            "Stop",
            session,
            turn,
            "2026-05-02T02:00:06Z",
            None,
            &stop_ref,
            &sha256_hex(stop_body),
        ),
    ];
    let mut f = std::fs::File::create(&jsonl_path).expect("create JSONL file");
    for event in &events {
        let line = serde_json::to_string(event).expect("serialize event");
        writeln!(f, "{line}").expect("write JSONL line");
    }

    let resp = run_handler(&store, vault.path(), &jsonl_path)
        .await
        .expect("run_handler should succeed");

    assert!(
        resp.failed_turns.is_empty(),
        "expected no failures, got: {:?}",
        resp.failed_turns
    );

    let session_id = SessionId::parse(session).expect("valid session_id");
    store
        .with_tx(move |tx| {
            let rows = tx.list_trace_events(&session_id, turn)?;
            assert_eq!(rows.len(), 6, "expected full six-event turn");
            let trace_events: Vec<&str> = rows
                .iter()
                .map(|r| {
                    r.extra_frontmatter
                        .get("trace_event")
                        .and_then(|v| v.as_str())
                        .expect("trace_event string")
                })
                .collect();
            assert_eq!(
                trace_events,
                vec![
                    "user_message",
                    "agent_message",
                    "pre_tool",
                    "tool_output",
                    "post_tool",
                    "stop"
                ]
            );

            let tool_output = rows
                .iter()
                .find(|r| {
                    r.extra_frontmatter
                        .get("trace_event")
                        .and_then(|v| v.as_str())
                        == Some("tool_output")
                })
                .expect("tool_output row");
            assert!(tool_output.body.contains("running 3 tests"));
            assert!(tool_output.body.contains("all passed"));
            assert!(
                !tool_output.body.contains('\x1b'),
                "interactive terminal body should be squashed before persist"
            );
            let parent = tool_output.extra_frontmatter["trace"]["parent_event_id"]
                .as_str()
                .expect("tool_output parent_event_id");
            assert_eq!(parent, ULID_FULL_PRE);

            let post_tool = rows
                .iter()
                .find(|r| {
                    r.extra_frontmatter
                        .get("trace_event")
                        .and_then(|v| v.as_str())
                        == Some("post_tool")
                })
                .expect("post_tool row");
            let parent = post_tool.extra_frontmatter["trace"]["parent_event_id"]
                .as_str()
                .expect("post_tool parent_event_id");
            assert_eq!(parent, ULID_FULL_PRE);

            let agent = rows
                .iter()
                .find(|r| {
                    r.extra_frontmatter
                        .get("trace_event")
                        .and_then(|v| v.as_str())
                        == Some("agent_message")
                })
                .expect("agent_message row");
            assert_eq!(agent.body, agent_body);
            assert!(
                tx.turn_summary_exists(&session_id, turn)?,
                "full turn should create a summary after Stop"
            );
            Ok(())
        })
        .await
        .expect("store query should succeed");
}

#[tokio::test]
#[allow(
    clippy::expect_used,
    reason = "test: panics surface broken invariants immediately"
)]
async fn capture_trace_blocks_secret_body_before_persisting_turn() {
    let vault = tempfile::tempdir().expect("tempdir");
    let store = open_test_store_in_memory().await;
    let jsonl_path = vault.path().join("trace.jsonl");
    let session = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let turn = "turn-secret";
    let event_id = "01ARZ3NDEKTSV4RRFFQ69G5FAM";
    let body = "api_key = sk-test-12345678901234567890";
    let payload_ref = write_source(vault.path(), &format!("{event_id}.txt"), body);
    let event = make_event(
        event_id,
        "UserPromptSubmit",
        session,
        turn,
        "2026-05-02T00:00:01Z",
        None,
        &payload_ref,
        &sha256_hex(body),
    );

    let mut f = std::fs::File::create(&jsonl_path).expect("create JSONL file");
    let line = serde_json::to_string(&event).expect("serialize event");
    writeln!(f, "{line}").expect("write JSONL line");

    let resp = run_handler(&store, vault.path(), &jsonl_path)
        .await
        .expect("run_handler should return Ok when a turn is privacy-blocked");

    assert_eq!(
        resp.failed_turns.len(),
        1,
        "secret turn should be rejected exactly once: {:?}",
        resp.failed_turns
    );
    assert_eq!(resp.failed_turns[0].0, session);
    assert_eq!(resp.failed_turns[0].1, turn);
    assert!(
        resp.failed_turns[0].2.contains("privacy filter"),
        "failure should identify the pre-persist filter: {:?}",
        resp.failed_turns[0]
    );

    let session_id = SessionId::parse(session).expect("valid session_id");
    store
        .with_tx(move |tx| {
            let rows = tx.list_trace_events(&session_id, turn)?;
            assert_eq!(
                rows.len(),
                0,
                "privacy-blocked turn must not persist partial trace rows"
            );
            assert!(
                !tx.turn_summary_exists(&session_id, turn)?,
                "privacy-blocked turn must not create a summary"
            );
            Ok(())
        })
        .await
        .expect("store query should succeed");
}

#[tokio::test]
async fn run_blocks_handler_persists_trace_blocks_with_reasoning_signature() {
    let vault = tempfile::tempdir().expect("tempdir");
    let store = open_test_store_in_memory().await;
    let blocks_path = vault.path().join("trace-blocks.json");
    std::fs::write(
        &blocks_path,
        serde_json::json!([
            {
                "kind": "reasoning",
                "text": "private chain of thought",
                "signature": "sig_trace_blocks_v1"
            },
            {
                "kind": "tool_use",
                "tool": "Read",
                "input": {"file": "README.md"},
                "id": "tool-1"
            },
            {
                "kind": "tool_result",
                "tool_use_id": "tool-1",
                "content": "file contents",
                "is_error": false
            },
            {
                "kind": "text",
                "text": "final answer"
            }
        ])
        .to_string(),
    )
    .expect("write trace blocks json");

    let session = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let raw_blocks = std::fs::read(&blocks_path).expect("read trace blocks bytes");
    let turn_id = stable_turn_id_for_blocks(session, &raw_blocks);
    let resp = run_blocks_handler(&store, vault.path(), &blocks_path, session)
        .await
        .expect("run_blocks_handler should succeed");
    assert!(
        resp.failed_turns.is_empty(),
        "expected no failures, got: {:?}",
        resp.failed_turns
    );

    let session_id = SessionId::parse(session).expect("valid session_id");
    store
        .with_tx(move |tx| {
            let rows = tx.list_trace_events(&session_id, &turn_id)?;
            assert_eq!(rows.len(), 4, "expected one row per trace block");
            let row_kinds: Vec<_> = rows.iter().map(|row| row.kind.as_str()).collect();
            assert_eq!(row_kinds, vec!["reasoning", "trace", "trace", "trace"]);
            let row_events: Vec<_> = rows
                .iter()
                .map(|row| row.extra_frontmatter["trace_event"].as_str().unwrap_or(""))
                .collect();
            assert_eq!(
                row_events,
                vec!["agent_message", "pre_tool", "tool_output", "agent_message"]
            );

            let row = &rows[0];
            assert_eq!(
                row.extra_frontmatter["trace_event"].as_str(),
                Some("agent_message")
            );
            assert!(
                row.body.contains("private chain of thought"),
                "reasoning row should preserve reasoning body"
            );

            let blocks = row
                .extra_frontmatter
                .get("trace_blocks")
                .and_then(|value| value.as_array())
                .expect("trace_blocks array");
            assert_eq!(blocks.len(), 1);
            assert_eq!(blocks[0]["kind"].as_str(), Some("reasoning"));
            assert_eq!(blocks[0]["signature"].as_str(), Some("sig_trace_blocks_v1"));
            assert_eq!(
                row.extra_frontmatter["trace"]["block_index"].as_u64(),
                Some(0)
            );

            let tool_use = &rows[1];
            assert!(tool_use.body.contains("Read"));
            assert_eq!(
                tool_use.extra_frontmatter["trace"]["tool_call_id"].as_str(),
                Some("tool-1")
            );

            let tool_result = &rows[2];
            assert_eq!(
                tool_result.extra_frontmatter["trace"]["parent_event_id"].as_str(),
                tool_use.extra_frontmatter["trace"]["capture_event_id"].as_str()
            );

            let text = &rows[3];
            assert_eq!(text.body, "final answer");
            Ok(())
        })
        .await
        .expect("store query should succeed");
}

#[tokio::test]
async fn run_blocks_handler_is_idempotent_for_same_input() {
    let vault = tempfile::tempdir().expect("tempdir");
    let store = open_test_store_in_memory().await;
    let blocks_path = vault.path().join("trace-blocks.json");
    std::fs::write(
        &blocks_path,
        serde_json::json!([
            { "kind": "reasoning", "text": "private chain of thought", "signature": "sig_trace_blocks_v1" },
            { "kind": "text", "text": "final answer" }
        ])
        .to_string(),
    )
    .expect("write trace blocks json");

    let session = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let raw_blocks = std::fs::read(&blocks_path).expect("read trace blocks bytes");
    let turn_id = stable_turn_id_for_blocks(session, &raw_blocks);
    run_blocks_handler(&store, vault.path(), &blocks_path, session)
        .await
        .expect("first import");
    run_blocks_handler(&store, vault.path(), &blocks_path, session)
        .await
        .expect("second import");

    let session_id = SessionId::parse(session).expect("valid session_id");
    store
        .with_tx(move |tx| {
            let rows = tx.list_trace_events(&session_id, &turn_id)?;
            assert_eq!(rows.len(), 2, "re-import should not duplicate rows");
            Ok(())
        })
        .await
        .expect("store query should succeed");
}

#[tokio::test]
#[allow(
    clippy::expect_used,
    reason = "test: panics surface broken invariants immediately"
)]
async fn in_batch_post_before_pre_resolves_parent() {
    let vault = tempfile::tempdir().expect("tempdir");
    let store = open_test_store_in_memory().await;
    let jsonl_path = vault.path().join("trace.jsonl");

    write_fixture_post_before_pre(vault.path(), &jsonl_path);

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
        .with_tx(move |tx| {
            let rows = tx.list_trace_events(&session_id, "turn-1")?;
            assert_eq!(rows.len(), 4, "expected 4 events");

            // The post_tool row's parent_event_id must match the pre_tool's
            // capture_event_id even though post arrived before pre in the JSONL.
            let post = rows
                .iter()
                .find(|r| {
                    r.extra_frontmatter
                        .get("trace_event")
                        .and_then(|v| v.as_str())
                        == Some("post_tool")
                })
                .expect("post_tool row");
            let parent = post.extra_frontmatter["trace"]["parent_event_id"]
                .as_str()
                .expect("parent_event_id present");
            assert_eq!(parent, "01ARZ3NDEKTSV4RRFFQ69G5FAB");
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

/// Write a single-turn fixture containing a `PreCompact` hook snapshot.
///
/// The event belongs to its own turn and intentionally omits a `Stop` hook:
/// pre-compaction snapshots are lifecycle boundaries, not end-of-turn
/// summaries. The source file is written under `vault/sources/hook/`.
fn write_pre_compact_fixture(vault: &Path, jsonl_path: &Path) {
    let session = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let turn = "turn-pre-compact";
    let event_id = "01ARZ3NDEKTSV4RRFFQ69G5FAM";
    let body = "transcript snapshot before compaction";
    let payload_ref = write_source(vault, &format!("{event_id}.txt"), body);

    let event = make_event(
        event_id,
        "PreCompact",
        session,
        turn,
        "2026-05-02T00:00:05Z",
        None,
        &payload_ref,
        &sha256_hex(body),
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
                assert_eq!(
                    rows.len(),
                    4,
                    "no duplicate events from replay: got {}",
                    rows.len()
                );

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

#[tokio::test]
#[allow(
    clippy::expect_used,
    reason = "test: panics surface broken invariants immediately"
)]
async fn pre_compact_event_is_captured_fail_closed() {
    let vault = tempfile::tempdir().expect("tempdir");
    let store = open_test_store_in_memory().await;
    let jsonl_path = vault.path().join("trace.jsonl");

    write_pre_compact_fixture(vault.path(), &jsonl_path);

    let resp = run_handler(&store, vault.path(), &jsonl_path)
        .await
        .expect("run_handler should succeed");

    assert!(
        resp.failed_turns.is_empty(),
        "PreCompact should persist cleanly, got failures: {:?}",
        resp.failed_turns
    );

    let session_id = SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid session_id");
    store
        .with_tx(move |tx| {
            let rows = tx.list_trace_events(&session_id, "turn-pre-compact")?;
            assert_eq!(rows.len(), 1, "expected one pre-compact trace event");
            assert_eq!(
                rows[0].extra_frontmatter["trace_event"].as_str(),
                Some("pre_compact"),
                "trace_event should record the pre_compact lifecycle boundary"
            );
            assert!(
                !tx.turn_summary_exists(&session_id, "turn-pre-compact")?,
                "pre-compact snapshots should not synthesize a turn summary without Stop"
            );
            Ok(())
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
                assert_eq!(
                    seqs,
                    vec![0, 1, 2, 3, 4],
                    "sequences must be contiguous 0..4"
                );

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
