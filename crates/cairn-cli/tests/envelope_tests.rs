//! Verify that every verb returns a valid cairn.mcp.v1 JSON envelope.
//! These tests invoke the compiled binary and will pass after Task 7 wires dispatch.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use cairn_core::domain::{
    ActorChainEntry, CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, CaptureRefs,
    ChainRole, Identity, MemoryRecord, PayloadHash, Rfc3339Timestamp, SourceFamily,
    TerminalContext,
};
use sha2::{Digest as _, Sha256};
use ulid::Ulid;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn seed_default_identity(vault: &Path) {
    let out = cli()
        .current_dir(vault)
        .args([
            "ingest",
            "--kind",
            "reference",
            "--body",
            "identity seed",
            "--json",
        ])
        .output()
        .expect("seed default identity");
    assert!(
        out.status.success(),
        "identity seed failed: {:?}\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn assert_rejected_capability_unavailable(verb_args: &[&str], capability: &str) {
    let out = {
        let mut cmd = cli();
        cmd.args(verb_args);
        cmd.output()
            .unwrap_or_else(|e| panic!("failed to run {verb_args:?}: {e}"))
    };
    assert_eq!(
        out.status.code(),
        Some(69),
        "verb {verb_args:?} should exit 69 (CapabilityUnavailable), got {:?}",
        out.status
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("verb {verb_args:?} JSON parse failed: {e}\nstdout: {stdout:?}")
    });
    assert_eq!(v["contract"], "cairn.mcp.v1", "verb {verb_args:?}");
    assert_eq!(v["status"], "rejected", "verb {verb_args:?}");
    assert_eq!(
        v["error"]["code"], "CapabilityUnavailable",
        "verb {verb_args:?}"
    );
    assert_eq!(
        v["error"]["data"]["capability"], capability,
        "verb {verb_args:?}"
    );
    assert!(v["operation_id"].is_string(), "verb {verb_args:?}");
    assert!(v["policy_trace"].is_array(), "verb {verb_args:?}");
}

fn sha256_hex(text: &str) -> String {
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    format!("{:x}", h.finalize())
}

fn write_capture_trace_event(vault: &Path, session: &str, turn: &str, body: &str) -> PathBuf {
    let event_id = "01ARZ3NDEKTSV4RRFFQ69G5FAM";
    let sources = vault.join("sources").join("hook");
    std::fs::create_dir_all(&sources).expect("create sources/hook");
    let payload_ref = format!("sources/hook/{event_id}.txt");
    std::fs::write(vault.join(&payload_ref), body).expect("write source body");

    let sensor =
        Identity::parse("snr:local:hook:cc-session:v1").expect("invariant: valid sensor id");
    let event = CaptureEvent {
        event_id: CaptureEventId::parse(event_id).expect("invariant: valid ULID"),
        sensor_id: sensor.clone(),
        capture_mode: CaptureMode::Auto,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: sensor,
            at: Rfc3339Timestamp::parse("2026-05-02T00:00:01Z").expect("invariant: valid RFC-3339"),
        }],
        refs: Some(CaptureRefs {
            session_id: Some(session.to_owned()),
            turn_id: Some(turn.to_owned()),
            tool_id: None,
        }),
        payload_hash: PayloadHash::parse(format!("sha256:{}", sha256_hex(body)))
            .expect("invariant: valid sha256 hash"),
        payload_ref,
        captured_at: Rfc3339Timestamp::parse("2026-05-02T00:00:01Z")
            .expect("invariant: valid RFC-3339"),
        payload: CapturePayload::Hook {
            hook_name: "UserPromptSubmit".to_owned(),
            tool_name: None,
        },
        source_family: SourceFamily::Hook,
    };

    let trace_path = vault.join("trace.jsonl");
    let mut f = std::fs::File::create(&trace_path).expect("create trace JSONL");
    let line = serde_json::to_string(&event).expect("serialize event");
    writeln!(f, "{line}").expect("write trace JSONL");
    trace_path
}

fn write_trace_blocks_fixture(vault: &Path, filename: &str) -> (PathBuf, Vec<u8>) {
    let path = vault.join(filename);
    let raw = serde_json::json!([
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
    .to_string()
    .into_bytes();
    std::fs::write(&path, &raw).expect("write trace blocks fixture");
    (path, raw)
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

fn write_source_for_family(vault: &Path, family: &str, event_id: &str, body: &str) -> String {
    let sources = vault.join("sources").join(family);
    std::fs::create_dir_all(&sources).expect("create source family dir");
    let payload_ref = format!("sources/{family}/{event_id}.txt");
    std::fs::write(vault.join(&payload_ref), body).expect("write source body");
    payload_ref
}

#[allow(clippy::too_many_arguments)]
fn make_trace_hook_event(
    event_id: &str,
    hook_name: &str,
    session: &str,
    turn: &str,
    timestamp: &str,
    tool_id: Option<&str>,
    payload_ref: &str,
    body: &str,
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
            session_id: Some(session.to_owned()),
            turn_id: Some(turn.to_owned()),
            tool_id: tool_id.map(ToOwned::to_owned),
        }),
        payload_hash: PayloadHash::parse(format!("sha256:{}", sha256_hex(body)))
            .expect("invariant: valid sha256 hash"),
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
fn make_trace_terminal_event(
    event_id: &str,
    session: &str,
    turn: &str,
    timestamp: &str,
    tool_id: &str,
    payload_ref: &str,
    body: &str,
) -> CaptureEvent {
    let sensor =
        Identity::parse("snr:local:terminal:default:v1").expect("invariant: valid sensor id");
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
            session_id: Some(session.to_owned()),
            turn_id: Some(turn.to_owned()),
            tool_id: Some(tool_id.to_owned()),
        }),
        payload_hash: PayloadHash::parse(format!("sha256:{}", sha256_hex(body)))
            .expect("invariant: valid sha256 hash"),
        payload_ref: payload_ref.to_owned(),
        captured_at: Rfc3339Timestamp::parse(timestamp).expect("invariant: valid RFC-3339"),
        payload: CapturePayload::Terminal {
            command: "bash -lc 'cargo test -p cairn-core'".into(),
            exit_code: Some(0),
            context: Some(TerminalContext::InteractiveTty),
        },
        source_family: SourceFamily::Terminal,
    }
}

fn make_trace_agent_event(
    event_id: &str,
    session: &str,
    turn: &str,
    timestamp: &str,
    payload_ref: &str,
    body: &str,
) -> CaptureEvent {
    let sensor =
        Identity::parse("snr:local:proactive:codex:v1").expect("invariant: valid sensor id");
    let author =
        Identity::parse("agt:codex:gpt-5:main:v1").expect("invariant: valid agent identity");
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
            session_id: Some(session.to_owned()),
            turn_id: Some(turn.to_owned()),
            tool_id: None,
        }),
        payload_hash: PayloadHash::parse(format!("sha256:{}", sha256_hex(body)))
            .expect("invariant: valid sha256 hash"),
        payload_ref: payload_ref.to_owned(),
        captured_at: Rfc3339Timestamp::parse(timestamp).expect("invariant: valid RFC-3339"),
        payload: CapturePayload::Proactive {
            kind: "agent_message".into(),
            rationale: "assistant trace message".into(),
        },
        source_family: SourceFamily::Proactive,
    }
}

fn write_full_scope_trace_fixture(vault: &Path, session: &str, turn: &str) -> PathBuf {
    let user_id = "01ARZ3NDEKTSV4RRFFQ69G5FAN";
    let agent_id = "01ARZ3NDEKTSV4RRFFQ69G5FAP";
    let pre_id = "01ARZ3NDEKTSV4RRFFQ69G5FAQ";
    let output_id = "01ARZ3NDEKTSV4RRFFQ69G5FAR";
    let post_id = "01ARZ3NDEKTSV4RRFFQ69G5FAS";
    let stop_id = "01ARZ3NDEKTSV4RRFFQ69G5FAT";
    let tool_id = "toolu_cli_full_scope_01";

    let user_body = "Please run the targeted tests.";
    let agent_body = "I will run the targeted core tests now.";
    let pre_body = r#"{"tool":"bash","input":{"command":"cargo test -p cairn-core"}}"#;
    let output_body = "\x1b[32mrunning 3 tests\x1b[0m\nall passed\n";
    let post_body = r#"{"tool":"bash","exit_code":0}"#;
    let stop_body = "turn ended";

    let events = [
        make_trace_hook_event(
            user_id,
            "UserPromptSubmit",
            session,
            turn,
            "2026-05-02T02:00:01Z",
            None,
            &write_source_for_family(vault, "hook", user_id, user_body),
            user_body,
        ),
        make_trace_agent_event(
            agent_id,
            session,
            turn,
            "2026-05-02T02:00:02Z",
            &write_source_for_family(vault, "proactive", agent_id, agent_body),
            agent_body,
        ),
        make_trace_hook_event(
            pre_id,
            "PreToolUse",
            session,
            turn,
            "2026-05-02T02:00:03Z",
            Some(tool_id),
            &write_source_for_family(vault, "hook", pre_id, pre_body),
            pre_body,
        ),
        make_trace_terminal_event(
            output_id,
            session,
            turn,
            "2026-05-02T02:00:04Z",
            tool_id,
            &write_source_for_family(vault, "terminal", output_id, output_body),
            output_body,
        ),
        make_trace_hook_event(
            post_id,
            "PostToolUse",
            session,
            turn,
            "2026-05-02T02:00:05Z",
            Some(tool_id),
            &write_source_for_family(vault, "hook", post_id, post_body),
            post_body,
        ),
        make_trace_hook_event(
            stop_id,
            "Stop",
            session,
            turn,
            "2026-05-02T02:00:06Z",
            None,
            &write_source_for_family(vault, "hook", stop_id, stop_body),
            stop_body,
        ),
    ];

    let trace_path = vault.join("full-scope-trace.jsonl");
    let mut f = std::fs::File::create(&trace_path).expect("create full-scope trace JSONL");
    for event in &events {
        writeln!(f, "{}", serde_json::to_string(event).expect("event json"))
            .expect("write full-scope trace JSONL");
    }
    trace_path
}

#[allow(dead_code)]
fn assert_aborted_not_found(verb_args: &[&str], target: &str) {
    let out = {
        let mut cmd = cli();
        cmd.args(verb_args);
        cmd.output()
            .unwrap_or_else(|e| panic!("failed to run {verb_args:?}: {e}"))
    };
    assert_eq!(
        out.status.code(),
        Some(78),
        "verb {verb_args:?} should exit 78 (NotFound aborted), got {:?}",
        out.status
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("verb {verb_args:?} JSON parse failed: {e}\nstdout: {stdout:?}")
    });
    assert_eq!(v["contract"], "cairn.mcp.v1", "verb {verb_args:?}");
    assert_eq!(v["status"], "aborted", "verb {verb_args:?}");
    assert_eq!(v["error"]["code"], "NotFound", "verb {verb_args:?}");
    assert_eq!(v["error"]["data"]["target"], target, "verb {verb_args:?}");
    assert!(v["operation_id"].is_string(), "verb {verb_args:?}");
    assert!(v["policy_trace"].is_array(), "verb {verb_args:?}");
}

#[test]
fn ingest_returns_committed_envelope() {
    let dir = tempfile::tempdir().expect("tempdir");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: dir.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    let out = cli()
        .current_dir(dir.path())
        .args(["ingest", "--kind", "user", "--body", "hello", "--json"])
        .output()
        .expect("cairn ingest --json");
    assert_eq!(
        out.status.code(),
        Some(0),
        "ingest should exit 0 (committed), got {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("ingest JSON parse failed: {e}\nstdout: {stdout:?}"));
    assert_eq!(v["contract"], "cairn.mcp.v1");
    assert_eq!(v["status"], "committed");
    assert_eq!(v["verb"], "ingest");
    assert!(v["error"].is_null());
    assert!(v["data"]["record_id"].is_string());
    assert!(v["operation_id"].is_string());
    assert!(v["policy_trace"].is_array());
}

#[test]
fn search_keyword_json_exits_zero_with_hits() {
    // Search is now wired to `cairn_core::verbs::search::run`. Keyword mode
    // (the default) works without an embedder — it opens (or creates) the
    // SQLite store and runs FTS5. Against an empty vault the result is an
    // empty `hits` array; the exit code is 0 (success).
    //
    // The vault-binding gate added in round-2 requires `.cairn/vault.id`
    // to exist before the store is opened, so seed a minimal sentinel
    // here. We don't go through `cairn bootstrap` to avoid the BGE
    // model download it would trigger.
    let tmp = tempfile::tempdir().expect("tempdir");
    let cairn_dir = tmp.path().join(".cairn");
    std::fs::create_dir_all(&cairn_dir).expect("create .cairn");
    std::fs::write(cairn_dir.join("vault.id"), b"01HZZ0000000000000000000AB\n")
        .expect("write vault.id");
    let out = {
        let mut cmd = cli();
        cmd.env("CAIRN_VAULT", tmp.path());
        cmd.args(["search", "--mode", "keyword", "test query", "--json"]);
        cmd.output()
            .expect("cairn search --mode keyword test query --json should run")
    };
    assert_eq!(
        out.status.code(),
        Some(0),
        "keyword search against an empty vault must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    // The committed wire shape is the IDL `Response` envelope
    // (round-8 review #1). Round-trip through the generated
    // `Response` deserializer so a future serde-annotation drift
    // breaks the test instead of silently emitting a non-IDL
    // envelope to advertised search clients.
    let resp: cairn_core::generated::envelope::Response = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("search envelope parse failed: {e}\nstdout: {stdout:?}"));
    assert_eq!(resp.contract, "cairn.mcp.v1");
    assert!(
        matches!(
            resp.status,
            cairn_core::generated::envelope::ResponseStatus::Committed
        ),
        "successful search must be status=committed"
    );
    assert!(matches!(
        resp.verb,
        cairn_core::generated::envelope::ResponseVerb::Search
    ));
    let data = resp.data.expect("committed search must have data");
    let cairn_core::generated::envelope::ResponseData::Search(payload) = data else {
        panic!("data must be Search variant");
    };
    assert!(
        payload.hits.is_empty(),
        "empty vault must yield zero hits; got {} hits",
        payload.hits.len()
    );
}

#[test]
fn retrieve_record_missing_returns_committed_empty_record_envelope() {
    let dir = tempfile::tempdir().expect("tempdir");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: dir.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    let seed = cli()
        .current_dir(dir.path())
        .args([
            "ingest",
            "--kind",
            "reference",
            "--body",
            "seed default issuer",
            "--json",
        ])
        .output()
        .expect("seed issuer");
    assert_eq!(
        seed.status.code(),
        Some(0),
        "seed ingest failed: {}",
        String::from_utf8_lossy(&seed.stderr)
    );
    let record_id = "01HQZX9F5N0000000000000000";
    let out = cli()
        .current_dir(dir.path())
        .args(["retrieve", record_id, "--json"])
        .output()
        .unwrap_or_else(|e| panic!("failed to run retrieve: {e}"));
    assert_eq!(
        out.status.code(),
        Some(0),
        "authorized missing record should commit an empty retrieve shape, got {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("retrieve JSON parse failed: {e}\nstdout: {stdout:?}"));
    assert_eq!(v["contract"], "cairn.mcp.v1");
    assert_eq!(v["status"], "committed");
    assert_eq!(v["verb"], "retrieve");
    assert_eq!(v["target"], "record");
    assert_eq!(v["data"]["record_id"], record_id);
    assert_eq!(v["data"]["kind"], "unknown");
    assert!(v["data"].get("body").is_none());
    assert!(v["operation_id"].is_string());
    assert!(v["policy_trace"].is_array());
}

#[test]
fn summarize_returns_committed_envelope() {
    let dir = tempfile::tempdir().expect("tempdir");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: dir.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    let seed = cli()
        .current_dir(dir.path())
        .args([
            "ingest",
            "--kind",
            "reference",
            "--body",
            "summarize envelope body",
            "--json",
        ])
        .output()
        .expect("seed record");
    assert_eq!(
        seed.status.code(),
        Some(0),
        "seed ingest failed: {}",
        String::from_utf8_lossy(&seed.stderr)
    );
    let seed_json: serde_json::Value = serde_json::from_slice(&seed.stdout).expect("seed json");
    let record_id = seed_json["data"]["record_id"].as_str().expect("record_id");

    let out = cli()
        .current_dir(dir.path())
        .args(["summarize", record_id, "--json"])
        .output()
        .unwrap_or_else(|e| panic!("failed to run summarize: {e}"));
    assert_eq!(
        out.status.code(),
        Some(0),
        "summarize should exit 0 (committed), got {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("summarize JSON parse failed: {e}\nstdout: {stdout:?}"));
    assert_eq!(v["contract"], "cairn.mcp.v1");
    assert_eq!(v["status"], "committed");
    assert_eq!(v["verb"], "summarize");
    assert!(v["error"].is_null());
    assert!(
        v["data"]["summary"]
            .as_str()
            .expect("summary")
            .contains("summarize envelope body")
    );
    assert!(v["operation_id"].is_string());
    assert!(v["policy_trace"].is_array());
}

#[test]
fn assemble_hot_returns_committed_envelope() {
    // `assemble_hot` is wired to real hot-memory sources. The verb exits 0
    // and returns a committed envelope with six segments (default recipe).
    // Bootstrap a tempdir vault — the verb fails closed on a non-vault cwd.
    let dir = tempfile::tempdir().expect("tempdir");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: dir.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    seed_default_identity(dir.path());
    let out = cli()
        .current_dir(dir.path())
        .args(["assemble_hot", "--json"])
        .output()
        .unwrap_or_else(|e| panic!("failed to run assemble_hot: {e}"));
    assert_eq!(
        out.status.code(),
        Some(0),
        "assemble_hot should exit 0 (committed), got {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("assemble_hot JSON parse failed: {e}\nstdout: {stdout:?}"));
    assert_eq!(v["contract"], "cairn.mcp.v1");
    assert_eq!(v["status"], "committed");
    assert_eq!(v["verb"], "assemble_hot");
    assert!(
        v["error"].is_null(),
        "committed envelope must not have error"
    );
    assert!(v["data"]["segments"].is_array(), "segments must be present");
    assert!(v["operation_id"].is_string());
    assert!(v["policy_trace"].is_array());
}

#[test]
fn capture_trace_returns_committed_envelope() {
    let dir = tempfile::tempdir().expect("tempdir");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: dir.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    let trace_path = dir.path().join("empty-trace.jsonl");
    std::fs::write(&trace_path, "").expect("write empty trace file");
    let out = cli()
        .current_dir(dir.path())
        .args([
            "capture_trace",
            "--from",
            trace_path.to_str().expect("utf-8 trace path"),
            "--json",
        ])
        .output()
        .expect("cairn capture_trace --json");
    assert_eq!(
        out.status.code(),
        Some(0),
        "capture_trace should exit 0 (committed), got {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("capture_trace JSON parse failed: {e}\nstdout: {stdout:?}"));
    assert_eq!(v["contract"], "cairn.mcp.v1");
    assert_eq!(v["status"], "committed");
    assert_eq!(v["verb"], "capture_trace");
    assert!(v["error"].is_null());
    assert!(v["data"]["trace_id"].is_string());
    assert_eq!(v["data"]["failed_turns"].as_array().map(Vec::len), Some(0));
    assert!(v["operation_id"].is_string());
    assert!(v["policy_trace"].is_array());
}

#[test]
#[allow(clippy::too_many_lines)] // end-to-end envelope assertion; splitting hides setup/expectation pairing
fn capture_trace_blocks_returns_committed_envelope_and_hides_reasoning_by_default() {
    let dir = tempfile::tempdir().expect("tempdir");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: dir.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    let session = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let (blocks_path, raw_blocks) = write_trace_blocks_fixture(dir.path(), "trace-blocks.json");
    let turn_id = stable_turn_id_for_blocks(session, &raw_blocks);

    let out = cli()
        .current_dir(dir.path())
        .args([
            "capture_trace",
            "--blocks",
            &format!("@{}", blocks_path.display()),
            "--session",
            session,
            "--json",
        ])
        .output()
        .expect("cairn capture_trace --blocks --json");
    assert_eq!(
        out.status.code(),
        Some(0),
        "capture_trace --blocks should exit 0 (committed), got {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("capture_trace --blocks JSON parse failed: {e}\nstdout: {stdout:?}")
    });
    assert_eq!(v["contract"], "cairn.mcp.v1");
    assert_eq!(v["status"], "committed");
    assert_eq!(v["verb"], "capture_trace");
    assert!(v["error"].is_null());
    assert!(v["data"]["trace_id"].is_string());
    assert_eq!(v["data"]["failed_turns"].as_array().map(Vec::len), Some(0));

    let conn =
        rusqlite::Connection::open(dir.path().join(".cairn").join("cairn.db")).expect("open db");
    let mut stmt = conn
        .prepare("SELECT record_json FROM records WHERE active = 1")
        .expect("prepare record query");
    let records = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query records")
        .map(|row| {
            let json = row.expect("record_json row");
            serde_json::from_str::<MemoryRecord>(&json)
                .unwrap_or_else(|e| panic!("record_json parse failed: {e}\njson: {json}"))
        })
        .collect::<Vec<_>>();
    let mut turn_records = records
        .iter()
        .filter(|record| {
            record
                .extra_frontmatter
                .get("trace")
                .and_then(|trace| trace.get("turn_id"))
                .and_then(|value| value.as_str())
                == Some(turn_id.as_str())
        })
        .collect::<Vec<_>>();
    turn_records.sort_by_key(|record| {
        record
            .extra_frontmatter
            .get("trace")
            .and_then(|trace| trace.get("sequence"))
            .and_then(serde_json::Value::as_u64)
            .expect("trace sequence")
    });
    assert_eq!(
        turn_records.len(),
        4,
        "expected one row per direct trace block"
    );
    assert_eq!(turn_records[0].kind.as_str(), "reasoning");
    assert_eq!(
        turn_records[0].extra_frontmatter["trace"]["block_index"].as_u64(),
        Some(0)
    );
    assert_eq!(
        turn_records[0].extra_frontmatter["trace_blocks"][0]["signature"].as_str(),
        Some("sig_trace_blocks_v1")
    );
    assert_eq!(
        turn_records[1].extra_frontmatter["trace"]["tool_call_id"].as_str(),
        Some("tool-1")
    );
    assert_eq!(
        turn_records[2].extra_frontmatter["trace"]["parent_event_id"].as_str(),
        turn_records[1].extra_frontmatter["trace"]["capture_event_id"].as_str()
    );
    assert_eq!(turn_records[3].body, "final answer");

    let hidden = cli()
        .current_dir(dir.path())
        .args([
            "search",
            "--mode",
            "keyword",
            "private chain of thought",
            "--json",
        ])
        .output()
        .expect("search without reasoning");
    assert_eq!(
        hidden.status.code(),
        Some(0),
        "search should exit 0; stderr: {}",
        String::from_utf8_lossy(&hidden.stderr)
    );
    let hidden_stdout = String::from_utf8(hidden.stdout).expect("utf-8");
    let hidden_v: serde_json::Value = serde_json::from_str(hidden_stdout.trim())
        .unwrap_or_else(|e| panic!("search JSON parse failed: {e}\nstdout: {hidden_stdout:?}"));
    assert_eq!(
        hidden_v["data"]["hits"].as_array().map(Vec::len),
        Some(0),
        "reasoning rows should be hidden by default"
    );

    let visible = cli()
        .current_dir(dir.path())
        .args([
            "search",
            "--mode",
            "keyword",
            "private chain of thought",
            "--include-reasoning",
            "--json",
        ])
        .output()
        .expect("search with reasoning");
    assert_eq!(
        visible.status.code(),
        Some(0),
        "search --include-reasoning should exit 0; stderr: {}",
        String::from_utf8_lossy(&visible.stderr)
    );
    let visible_stdout = String::from_utf8(visible.stdout).expect("utf-8");
    let visible_v: serde_json::Value =
        serde_json::from_str(visible_stdout.trim()).unwrap_or_else(|e| {
            panic!("search --include-reasoning JSON parse failed: {e}\nstdout: {visible_stdout:?}")
        });
    let hits = visible_v["data"]["hits"].as_array().expect("hits array");
    assert_eq!(
        hits.len(),
        1,
        "reasoning row should become visible when opted in"
    );
    let snippet = hits[0]["snippet"].as_str().expect("snippet");
    assert!(
        snippet.contains("private") && snippet.contains("chain") && snippet.contains("thought"),
        "expected reasoning snippet tokens in opted-in search response: {hits:?}"
    );
}

#[test]
fn capture_trace_blocks_cli_reimport_is_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: dir.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    let session = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let (blocks_path, raw_blocks) = write_trace_blocks_fixture(dir.path(), "trace-blocks.json");
    let turn_id = stable_turn_id_for_blocks(session, &raw_blocks);

    for label in ["first", "second"] {
        let out = cli()
            .current_dir(dir.path())
            .args([
                "capture_trace",
                "--blocks",
                blocks_path.to_str().expect("utf-8 blocks path"),
                "--session",
                session,
                "--json",
            ])
            .output()
            .unwrap_or_else(|e| panic!("{label} capture_trace --blocks --json failed: {e}"));
        assert_eq!(
            out.status.code(),
            Some(0),
            "{label} import should commit; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let conn =
        rusqlite::Connection::open(dir.path().join(".cairn").join("cairn.db")).expect("open db");
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records
             WHERE active = 1
               AND json_extract(record_json, '$.extra_frontmatter.trace.turn_id') = ?1",
            [turn_id.as_str()],
            |row| row.get(0),
        )
        .expect("count trace rows");
    assert_eq!(count, 4, "re-import should not duplicate direct trace rows");
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "CLI-level regression intentionally covers a full six-event trace turn"
)]
fn capture_trace_cli_imports_full_trace_scope_e2e() {
    let dir = tempfile::tempdir().expect("tempdir");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: dir.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    let session = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let turn = "turn-cli-full-scope";
    let trace_path = write_full_scope_trace_fixture(dir.path(), session, turn);

    let out = cli()
        .current_dir(dir.path())
        .args([
            "capture_trace",
            "--from",
            trace_path.to_str().expect("utf-8 trace path"),
            "--json",
        ])
        .output()
        .expect("cairn capture_trace --json");
    assert_eq!(
        out.status.code(),
        Some(0),
        "capture_trace should exit 0 (committed), got {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("capture_trace JSON parse failed: {e}\nstdout: {stdout:?}"));
    assert_eq!(v["contract"], "cairn.mcp.v1");
    assert_eq!(v["status"], "committed");
    assert_eq!(v["verb"], "capture_trace");
    assert_eq!(v["data"]["failed_turns"].as_array().map(Vec::len), Some(0));

    let conn =
        rusqlite::Connection::open(dir.path().join(".cairn").join("cairn.db")).expect("open db");
    let mut stmt = conn
        .prepare("SELECT record_json FROM records WHERE active = 1")
        .expect("prepare record query");
    let records = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query records")
        .map(|row| {
            let json = row.expect("record_json row");
            serde_json::from_str::<MemoryRecord>(&json)
                .unwrap_or_else(|e| panic!("record_json parse failed: {e}\njson: {json}"))
        })
        .collect::<Vec<_>>();

    let mut turn_records = records
        .iter()
        .filter(|record| {
            record
                .extra_frontmatter
                .get("trace")
                .and_then(|trace| trace.get("turn_id"))
                .and_then(|value| value.as_str())
                == Some(turn)
        })
        .collect::<Vec<_>>();
    let summary_count = turn_records
        .iter()
        .filter(|record| {
            record
                .extra_frontmatter
                .get("trace_event")
                .and_then(|value| value.as_str())
                == Some("turn_summary")
        })
        .count();
    assert_eq!(summary_count, 1, "Stop should create one turn summary");
    turn_records.retain(|record| {
        record
            .extra_frontmatter
            .get("trace_event")
            .and_then(|value| value.as_str())
            != Some("turn_summary")
    });
    turn_records.sort_by_key(|record| {
        record
            .extra_frontmatter
            .get("trace")
            .and_then(|trace| trace.get("sequence"))
            .and_then(serde_json::Value::as_u64)
            .expect("trace sequence")
    });

    let trace_events = turn_records
        .iter()
        .map(|record| {
            record
                .extra_frontmatter
                .get("trace_event")
                .and_then(|value| value.as_str())
                .expect("trace_event")
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        trace_events,
        [
            "user_message",
            "agent_message",
            "pre_tool",
            "tool_output",
            "post_tool",
            "stop",
        ]
    );

    let tool_output = turn_records
        .iter()
        .find(|record| {
            record
                .extra_frontmatter
                .get("trace_event")
                .and_then(|value| value.as_str())
                == Some("tool_output")
        })
        .expect("tool_output row");
    assert!(tool_output.body.contains("running 3 tests"));
    assert!(tool_output.body.contains("all passed"));
    assert!(
        !tool_output.body.contains('\x1b'),
        "CLI capture_trace must squash interactive terminal body before persist"
    );
    let parent = tool_output.extra_frontmatter["trace"]["parent_event_id"]
        .as_str()
        .expect("tool_output parent_event_id");
    assert_eq!(parent, "01ARZ3NDEKTSV4RRFFQ69G5FAQ");

    let post_tool = turn_records
        .iter()
        .find(|record| {
            record
                .extra_frontmatter
                .get("trace_event")
                .and_then(|value| value.as_str())
                == Some("post_tool")
        })
        .expect("post_tool row");
    let parent = post_tool.extra_frontmatter["trace"]["parent_event_id"]
        .as_str()
        .expect("post_tool parent_event_id");
    assert_eq!(parent, "01ARZ3NDEKTSV4RRFFQ69G5FAQ");
}

#[test]
fn capture_trace_committed_envelope_exposes_failed_turns() {
    let dir = tempfile::tempdir().expect("tempdir");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: dir.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    let session = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let turn = "turn-secret";
    let trace_path = write_capture_trace_event(
        dir.path(),
        session,
        turn,
        "api_key = sk-test-12345678901234567890",
    );
    let out = cli()
        .current_dir(dir.path())
        .args([
            "capture_trace",
            "--from",
            trace_path.to_str().expect("utf-8 trace path"),
            "--json",
        ])
        .output()
        .expect("cairn capture_trace --json");
    assert_eq!(
        out.status.code(),
        Some(0),
        "capture_trace should commit the import operation while surfacing failed turns; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("capture_trace JSON parse failed: {e}\nstdout: {stdout:?}"));
    assert_eq!(v["status"], "committed");
    let failed = v["data"]["failed_turns"]
        .as_array()
        .expect("failed_turns array");
    assert_eq!(failed.len(), 1, "expected one failed turn: {failed:?}");
    assert_eq!(failed[0]["session_id"], session);
    assert_eq!(failed[0]["turn_id"], turn);
    assert!(
        failed[0]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("pii_blocked")),
        "reason should carry the stable filter code: {:?}",
        failed[0]["reason"]
    );
    assert!(
        !stdout.contains("sk-test"),
        "public envelope must not echo secret body bytes: {stdout}"
    );
    assert!(
        v["policy_trace"]
            .as_array()
            .is_some_and(|entries| !entries.is_empty()),
        "privacy-blocked import should include body-free policy trace entries"
    );
}

#[test]
fn capture_trace_human_mode_reports_partial_failures() {
    let dir = tempfile::tempdir().expect("tempdir");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: dir.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    let trace_path = write_capture_trace_event(
        dir.path(),
        "01ARZ3NDEKTSV4RRFFQ69G5FAV",
        "turn-secret",
        "api_key = sk-test-12345678901234567890",
    );
    let out = cli()
        .current_dir(dir.path())
        .args([
            "capture_trace",
            "--from",
            trace_path.to_str().expect("utf-8 trace path"),
        ])
        .output()
        .expect("cairn capture_trace");
    assert_eq!(
        out.status.code(),
        Some(0),
        "capture_trace human mode should commit while surfacing failed turns; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(
        stdout.contains("1 failed turn(s)"),
        "human output must report partial failures: {stdout}"
    );
    assert!(
        stdout.contains("privacy_filter:pii_blocked"),
        "human output should include sanitized stable reason: {stdout}"
    );
    assert!(
        !stdout.contains("sk-test"),
        "human output must not echo secret body bytes: {stdout}"
    );
}

#[test]
fn capture_trace_rejects_session_arg_until_scoped_import_supported() {
    let dir = tempfile::tempdir().expect("tempdir");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: dir.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    let trace_path = dir.path().join("empty-trace.jsonl");
    std::fs::write(&trace_path, "").expect("write empty trace file");
    let out = cli()
        .current_dir(dir.path())
        .args([
            "capture_trace",
            "--from",
            trace_path.to_str().expect("utf-8 trace path"),
            "--session",
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "--json",
        ])
        .output()
        .expect("cairn capture_trace --session --json");
    assert_eq!(
        out.status.code(),
        Some(64),
        "unsupported --session must fail closed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("capture_trace --session JSON parse failed: {e}\nstdout: {stdout:?}")
    });
    assert_eq!(v["status"], "rejected");
    assert_eq!(v["error"]["code"], "InvalidArgs");
    assert_eq!(v["error"]["data"]["field"], "session_id");
}

#[test]
fn capture_trace_blocks_rejects_missing_session_arg() {
    let dir = tempfile::tempdir().expect("tempdir");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: dir.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    let (blocks_path, _) = write_trace_blocks_fixture(dir.path(), "trace-blocks.json");
    let out = cli()
        .current_dir(dir.path())
        .args([
            "capture_trace",
            "--blocks",
            blocks_path.to_str().expect("utf-8 blocks path"),
            "--json",
        ])
        .output()
        .expect("cairn capture_trace --blocks --json");
    assert_eq!(
        out.status.code(),
        Some(64),
        "missing --session must fail closed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("capture_trace --blocks JSON parse failed: {e}\nstdout: {stdout:?}")
    });
    assert_eq!(v["status"], "rejected");
    assert_eq!(v["error"]["code"], "InvalidArgs");
    assert_eq!(v["error"]["data"]["field"], "session_id");
}

#[test]
fn capture_trace_rejects_multiple_input_modes() {
    let dir = tempfile::tempdir().expect("tempdir");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: dir.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    let trace_path = dir.path().join("empty-trace.jsonl");
    std::fs::write(&trace_path, "").expect("write empty trace file");
    let (blocks_path, _) = write_trace_blocks_fixture(dir.path(), "trace-blocks.json");
    let out = cli()
        .current_dir(dir.path())
        .args([
            "capture_trace",
            "--from",
            trace_path.to_str().expect("utf-8 trace path"),
            "--blocks",
            blocks_path.to_str().expect("utf-8 blocks path"),
            "--session",
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "--json",
        ])
        .output()
        .expect("cairn capture_trace --from --blocks --json");
    assert!(
        !out.status.success(),
        "passing --from and --blocks together must be rejected; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let envelope: serde_json::Value =
        serde_json::from_str(&stdout).expect("InvalidArgs envelope on stdout");
    assert_eq!(envelope["status"], "rejected");
    assert_eq!(envelope["error"]["code"], "InvalidArgs");
    assert_eq!(envelope["error"]["data"]["field"], "from|blocks");
}

#[test]
fn forget_session_returns_capability_unavailable() {
    assert_rejected_capability_unavailable(
        &[
            "forget",
            "--session",
            "01JXXXXXXXXXXXXXXXXXXXXXXX",
            "--json",
        ],
        "cairn.mcp.v1.forget.session",
    );
}

#[test]
fn forget_scope_returns_capability_unavailable() {
    assert_rejected_capability_unavailable(
        &["forget", "--scope", r#"{"tenant":"default"}"#, "--json"],
        "cairn.mcp.v1.forget.scope",
    );
}

#[test]
fn status_in_unbound_dir_advertises_no_capabilities() {
    // When `cairn status` runs without a real vault behind it, every
    // capability gates on `.cairn/vault.id`. A tempdir without bootstrap
    // therefore advertises nothing — same shape as `Sdk::new`. This is
    // the fail-closed P0 contract: do not promise capabilities that no
    // store can back.
    let out = {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
        cmd.env_remove("CAIRN_VAULT");
        let tmp = tempfile::tempdir().expect("tempdir");
        cmd.current_dir(tmp.path());
        cmd.args(["status", "--json"]);
        let res = cmd.output().expect("status --json should run");
        std::mem::forget(tmp);
        res
    };
    assert_eq!(
        out.status.code(),
        Some(0),
        "status should exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("status JSON parse failed: {e}\nstdout: {stdout:?}"));
    let caps = v["capabilities"].as_array().expect("capabilities array");
    assert!(
        caps.is_empty(),
        "unbound vault must advertise no capabilities; got {caps:?}"
    );
}

#[test]
fn status_in_bound_vault_advertises_search_and_policy_trace() {
    // After #49 + the round-2 vault-presence gate, `cairn status` only
    // advertises capabilities when the vault is actually bootstrapped
    // (`.cairn/vault.id` exists). Hand-fabricate a minimal vault and
    // assert keyword + policy_trace appear, while semantic/hybrid are
    // absent because no embedding model is on disk. Skipping the real
    // `cairn bootstrap` avoids the ~25 MB BGE model download it would
    // otherwise trigger; we only need the sentinel for status to flip
    // out of the no-vault path.
    use std::collections::BTreeSet;

    let tmp = tempfile::tempdir().expect("tempdir");
    let vault_root = tmp.path().to_owned();
    let cairn_dir = vault_root.join(".cairn");
    std::fs::create_dir_all(&cairn_dir).expect("create .cairn");
    std::fs::write(cairn_dir.join("vault.id"), b"01HZZ0000000000000000000AB\n")
        .expect("write vault.id");
    let out = Command::new(env!("CARGO_BIN_EXE_cairn"))
        .env("CAIRN_VAULT", &vault_root)
        .args(["status", "--json"])
        .output()
        .expect("status --json should run");
    assert_eq!(
        out.status.code(),
        Some(0),
        "status should exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("status JSON parse failed: {e}\nstdout: {stdout:?}"));
    let caps: BTreeSet<String> = v["capabilities"]
        .as_array()
        .expect("capabilities array")
        .iter()
        .filter_map(|c| c.as_str().map(str::to_owned))
        .collect();
    assert!(
        caps.contains("cairn.mcp.v1.search.keyword"),
        "keyword must be advertised in a bound vault; got {caps:?}"
    );
    assert!(
        caps.contains("cairn.mcp.v1.policy_trace"),
        "policy_trace must be advertised in a bound vault; got {caps:?}"
    );
    assert!(
        !caps.contains("cairn.mcp.v1.search.semantic"),
        "semantic must NOT be advertised when no embedding model is on disk; got {caps:?}"
    );
    assert!(
        !caps.contains("cairn.mcp.v1.search.hybrid"),
        "hybrid must NOT be advertised when no embedding model is on disk \
         (runtime resolves an embedder for hybrid mode); got {caps:?}"
    );
    for wired_cap in [
        "cairn.mcp.v1.retrieve.session",
        "cairn.mcp.v1.retrieve.turn",
        "cairn.mcp.v1.retrieve.tool_call",
    ] {
        assert!(
            caps.contains(wired_cap),
            "wired retrieve capability {wired_cap} must be advertised; got {caps:?}"
        );
    }
    for stub_cap in [
        "cairn.mcp.v1.retrieve.record",
        "cairn.mcp.v1.retrieve.full",
        "cairn.mcp.v1.retrieve.folder",
        "cairn.mcp.v1.retrieve.scope",
        "cairn.mcp.v1.retrieve.profile",
        "cairn.mcp.v1.forget.session",
    ] {
        assert!(
            !caps.contains(stub_cap),
            "stub-only capability {stub_cap} must NOT be advertised; got {caps:?}"
        );
    }
    assert!(
        caps.contains("cairn.mcp.v1.forget.record"),
        "forget.record must be advertised once the runtime path is wired; got {caps:?}"
    );
}

#[test]
fn status_in_malformed_config_vault_exits_ex_config() {
    // Codex round-1 finding: `unwrap_or_default` on the config load was
    // hiding genuine errors and advertising defaults. A malformed
    // `.cairn/config.yaml` must propagate as exit 78 (EX_CONFIG) so
    // operators see the failure instead of silently negotiating against
    // discarded config.
    let tmp = tempfile::tempdir().expect("tempdir");
    let cairn_dir = tmp.path().join(".cairn");
    std::fs::create_dir_all(&cairn_dir).expect("create .cairn");
    std::fs::write(cairn_dir.join("config.yaml"), b": :\nnot: [valid yaml")
        .expect("write malformed config");
    // Bind the vault so the vault-presence gate would normally let the
    // status path proceed; failure must still come from the config load.
    std::fs::write(cairn_dir.join("vault.id"), b"01HZZ0000000000000000000AB\n")
        .expect("write vault.id");
    let out = Command::new(env!("CARGO_BIN_EXE_cairn"))
        .env("CAIRN_VAULT", tmp.path())
        .args(["status", "--json"])
        .output()
        .expect("status should run");
    assert_eq!(
        out.status.code(),
        Some(78),
        "malformed config must exit EX_CONFIG (78); got {:?}, stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn status_with_unbound_registry_default_exits_ex_config() {
    // Round-7 review #2: a registry default that points at a directory
    // without `.cairn/vault.id` must NOT silently fall through to the
    // empty-capabilities path. Operators who created a vault entry but
    // never bootstrapped it should see EX_CONFIG so they discover the
    // missing bootstrap, not an apparently healthy `capabilities: []`
    // (which the same directory would also reject from `cairn search`
    // and `cairn admin`).
    use cairn_cli::vault::registry::VaultRegistryStore;
    use cairn_core::config::{VaultEntry, VaultRegistry};

    // Vault dir with `.cairn/` but no `vault.id` — i.e. the operator
    // created a directory and registered it but never ran bootstrap.
    let vault_dir = tempfile::tempdir().expect("vault tempdir");
    std::fs::create_dir_all(vault_dir.path().join(".cairn")).expect("create .cairn");

    // Registry on disk pointing at that vault as the default.
    let reg_dir = tempfile::tempdir().expect("reg tempdir");
    let reg_path = reg_dir.path().join("vaults.toml");
    let store = VaultRegistryStore::new(reg_path.clone());
    let mut reg = VaultRegistry::default();
    reg.default = Some("home".into());
    reg.vaults.push(VaultEntry::new(
        "home",
        vault_dir.path().to_str().expect("utf-8 vault path"),
        None,
        None,
    ));
    store.save(&reg).expect("save registry");

    // Run from a *different* tempdir so the cwd-walk leg cannot match
    // anything — the registry default is the only source that can fire.
    let cwd = tempfile::tempdir().expect("cwd tempdir");

    let out = Command::new(env!("CARGO_BIN_EXE_cairn"))
        .env_remove("CAIRN_VAULT")
        .env("CAIRN_REGISTRY", &reg_path)
        .current_dir(cwd.path())
        .args(["status", "--json"])
        .output()
        .expect("status --json should run");
    assert_eq!(
        out.status.code(),
        Some(78),
        "unbound registry default must exit EX_CONFIG (78); got {:?}, stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn search_capability_unavailable_emits_rejected_envelope() {
    // Round-9 review #1: `CapabilityUnavailable` belongs to the
    // *rejected* error family in the IDL — `Aborted +
    // CapabilityUnavailable` would fail the generated `Response`
    // validator. Round-trip the failure envelope through the same
    // deserializer generated clients use so any future drift in the
    // helper or a stray `Aborted` regression breaks the test instead
    // of silently producing wire-invalid traffic.
    //
    // `--mode semantic` against an unbound, unmodelled vault is the
    // simplest path to this envelope: the CLI capability gate fires
    // before embedder resolution and emits the rejected envelope with
    // `code=CapabilityUnavailable` (round-9 review #2).
    let tmp = tempfile::tempdir().expect("tempdir");
    let cairn_dir = tmp.path().join(".cairn");
    std::fs::create_dir_all(&cairn_dir).expect("create .cairn");
    std::fs::write(cairn_dir.join("vault.id"), b"01HZZ0000000000000000000AB\n")
        .expect("write vault.id");

    let out = Command::new(env!("CARGO_BIN_EXE_cairn"))
        .env("CAIRN_VAULT", tmp.path())
        .env_remove("CAIRN_MOCK_EMBEDDER")
        .args(["search", "test", "--mode", "semantic", "--json"])
        .output()
        .expect("cairn search --mode semantic --json should run");
    assert_eq!(
        out.status.code(),
        Some(69),
        "unadvertised mode must exit EX_UNAVAILABLE (69); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let resp: cairn_core::generated::envelope::Response = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| {
            panic!("CapabilityUnavailable envelope parse failed: {e}\nstdout: {stdout:?}")
        });
    assert!(
        matches!(
            resp.status,
            cairn_core::generated::envelope::ResponseStatus::Rejected
        ),
        "CapabilityUnavailable must be status=rejected per IDL; got {:?}",
        resp.status
    );
    let err = resp.error.expect("rejected envelope must carry error");
    assert_eq!(err["code"], "CapabilityUnavailable");
    assert_eq!(
        err["data"]["capability"], "cairn.mcp.v1.search.semantic",
        "unadvertised semantic mode must surface the semantic capability id; got {err}"
    );
}

#[test]
fn search_default_response_omits_excluded_field() {
    use cairn_core::generated::verbs::search::SearchData;

    // Construct a SearchData without `excluded` (the explain=false path
    // never populates it). Serialized JSON must not contain the
    // "excluded" key — the IDL marks it Option<...> with
    // skip_serializing_if = "Option::is_none".
    let data = SearchData {
        excluded: None,
        hits: Vec::new(),
        next_cursor: None,
        score_explain: None,
        degraded_legs: None,
    };
    let json = serde_json::to_string(&data).expect("serializable");
    assert!(
        !json.contains("\"excluded\""),
        "default SearchData must not emit excluded field; got {json}"
    );
}

#[test]
fn search_with_excluded_emits_field() {
    use cairn_core::generated::common::{RecordExclusion, RecordExclusionGate, Ulid};
    use cairn_core::generated::verbs::search::SearchData;

    // Sanity check the inverse: when explain=true populates excluded,
    // the JSON does carry the field. This guards against a future
    // serde annotation accidentally hiding the field permanently.
    let data = SearchData {
        excluded: Some(vec![RecordExclusion {
            target_id: Ulid("01HQZX9F5N0000000000000000".to_owned()),
            gate: RecordExclusionGate::ReadFilterStaleness,
            detail: String::new(),
        }]),
        hits: Vec::new(),
        next_cursor: None,
        score_explain: None,
        degraded_legs: None,
    };
    let json = serde_json::to_string(&data).expect("serializable");
    assert!(
        json.contains("\"excluded\""),
        "excluded must appear when set; got {json}"
    );
}

#[test]
fn search_without_degraded_legs_omits_field() {
    use cairn_core::generated::verbs::search::SearchData;

    // `degraded_legs: None` (a healthy hybrid result) must not emit
    // the field. Keeps the wire shape stable for callers that only
    // read it under partial-failure conditions.
    let data = SearchData {
        excluded: None,
        hits: Vec::new(),
        next_cursor: None,
        score_explain: None,
        degraded_legs: None,
    };
    let json = serde_json::to_string(&data).expect("serializable");
    assert!(
        !json.contains("\"degraded_legs\""),
        "healthy SearchData must not emit degraded_legs; got {json}"
    );
}

#[test]
fn search_with_degraded_legs_emits_full_entry() {
    use cairn_core::generated::verbs::search::{
        DegradedLegEntry, DegradedLegEntryLeg, DegradedLegEntryReason, DegradedLegEntrySource,
        SearchData,
    };

    // A hybrid response with a per-source graph-leg failure must
    // serialize the full `{leg, reason, source}` shape so callers can
    // attribute the partial-recall to the right seed source.
    let data = SearchData {
        excluded: None,
        hits: Vec::new(),
        next_cursor: None,
        score_explain: None,
        degraded_legs: Some(vec![
            DegradedLegEntry {
                leg: DegradedLegEntryLeg::Semantic,
                reason: DegradedLegEntryReason::SqlError,
                source: None,
            },
            DegradedLegEntry {
                leg: DegradedLegEntryLeg::Graph,
                reason: DegradedLegEntryReason::CapabilityUnavailable,
                source: Some(DegradedLegEntrySource::AuthSemanticSeed),
            },
        ]),
    };
    let json = serde_json::to_string(&data).expect("serializable");
    let v: serde_json::Value = serde_json::from_str(&json).expect("roundtrip");
    let arr = v["degraded_legs"]
        .as_array()
        .expect("degraded_legs is an array");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["leg"], "semantic");
    assert_eq!(arr[0]["reason"], "sql_error");
    assert!(
        arr[0].get("source").is_none() || arr[0]["source"].is_null(),
        "semantic leg must omit `source`; got {}",
        arr[0],
    );
    assert_eq!(arr[1]["leg"], "graph");
    assert_eq!(arr[1]["reason"], "capability_unavailable");
    assert_eq!(arr[1]["source"], "auth_semantic_seed");
}
