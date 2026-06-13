//! Integration tests for [`cairn_cli::mcp::CliMutationHost`] — the
//! `MutatingVerbHost` implementation that routes MCP `ingest`,
//! `capture_trace`, and `forget` calls through the same signed verb runtime
//! the CLI verbs use (brief §5.6 WAL; CLAUDE.md §4 invariant 3).
//!
//! These tests exercise the host against a real bootstrapped vault: the
//! signed write path (identity provisioning, server challenge, WAL
//! admission, store tx) runs end-to-end, exactly as it does for
//! `cairn ingest --body` / `cairn forget --record` / `cairn capture_trace
//! --from`.

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::io::Write as _;
use std::path::Path;

use cairn_cli::mcp::CliMutationHost;
use cairn_core::config::CairnConfig;
use cairn_core::domain::{
    ActorChainEntry, CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, CaptureRefs,
    ChainRole, Identity, PayloadHash, Rfc3339Timestamp, SourceFamily,
};
use cairn_core::generated::common::Ulid;
use cairn_core::generated::envelope::{Response, ResponseData, ResponseStatus, ResponseVerb};
use cairn_core::generated::verbs::capture_trace::CaptureTraceArgs;
use cairn_core::generated::verbs::forget::ForgetArgs;
use cairn_core::generated::verbs::ingest::IngestArgs;
use cairn_mcp::MutatingVerbHost as _;

fn bootstrap_vault(vault: &Path) {
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: vault.to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
}

/// Record consent + flip `sensors.hooks.enabled` in the vault config —
/// the same opt-in a real operator performs before `capture_trace` will
/// accept hook events (sensor gate, brief §14).
fn enable_hook_sensor(vault: &Path) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cairn"))
        .args([
            "sensor",
            "enable",
            "hook",
            "--reason",
            "mcp_mutation_host_test",
            "--vault",
        ])
        .arg(vault)
        .arg("--json")
        .output()
        .expect("spawn cairn sensor enable hook");
    assert!(
        out.status.success(),
        "sensor enable hook failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Load the vault's on-disk config the way `cairn mcp` does at boot.
fn vault_config(vault: &Path) -> CairnConfig {
    cairn_cli::config::load(vault, &cairn_cli::config::CliOverrides::default())
        .expect("load vault config")
}

fn host_for(vault: &Path) -> CliMutationHost {
    CliMutationHost::new(vault.to_path_buf(), CairnConfig::default())
}

fn body_ingest_args(body: &str, kind: &str) -> IngestArgs {
    IngestArgs {
        batch_size: None,
        body: Some(body.to_owned()),
        dry_run: None,
        exclude: None,
        file: None,
        folder: None,
        frontmatter: Some(serde_json::json!({"source": "test-suite"})),
        harness: None,
        human_review: None,
        include: None,
        jsonl: None,
        kind: kind.to_owned(),
        limit: None,
        mode: None,
        no_cache: None,
        no_diff: None,
        recording: None,
        recursive: None,
        session_id: None,
        session_id_from: None,
        tags: None,
        url: None,
    }
}

fn error_code(resp: &Response) -> Option<&str> {
    resp.error
        .as_ref()
        .and_then(|e| e.get("code"))
        .and_then(serde_json::Value::as_str)
}

fn active_record_count(vault: &Path) -> i64 {
    let conn = rusqlite::Connection::open(vault.join(".cairn/cairn.db")).expect("open cairn db");
    conn.query_row(
        "SELECT COUNT(*) FROM records WHERE active = 1 AND tombstoned = 0",
        [],
        |row| row.get(0),
    )
    .expect("count active records")
}

#[tokio::test]
async fn host_ingest_commits_record_through_signed_store() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    let host = host_for(vault.path());

    let resp = host
        .ingest(body_ingest_args(
            "Acme renewal terms land in Q3.",
            "reference",
        ))
        .await;

    assert!(
        matches!(resp.status, ResponseStatus::Committed),
        "ingest should commit; got {resp:?}"
    );
    assert!(matches!(resp.verb, ResponseVerb::Ingest));
    assert!(resp.error.is_none());
    let Some(ResponseData::Ingest(data)) = &resp.data else {
        panic!("committed ingest envelope must carry IngestData: {resp:?}");
    };
    assert_eq!(data.record_id.0.len(), 26, "record_id must be a ULID");
    assert!(
        !resp.policy_trace.is_empty(),
        "ingest must surface the filter policy trace"
    );
    assert_eq!(active_record_count(vault.path()), 1);
}

#[tokio::test]
async fn host_ingest_rejects_unknown_kind() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    let host = host_for(vault.path());

    let resp = host.ingest(body_ingest_args("some body", "email")).await;

    assert!(
        matches!(resp.status, ResponseStatus::Rejected),
        "unknown taxonomy kind must reject; got {resp:?}"
    );
    assert_eq!(active_record_count(vault.path()), 0);
}

#[tokio::test]
async fn host_ingest_rejects_session_scoped_body() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    let host = host_for(vault.path());

    let mut args = body_ingest_args("session body", "reference");
    args.session_id = Some("01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned());
    let resp = host.ingest(args).await;

    assert!(
        matches!(resp.status, ResponseStatus::Rejected),
        "session-scoped ingest is unsupported until intents carry a session dimension; got {resp:?}"
    );
    assert_eq!(error_code(&resp), Some("InvalidArgs"));
}

#[tokio::test]
async fn host_forget_record_round_trip() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    let host = host_for(vault.path());

    let ingest_resp = host
        .ingest(body_ingest_args("Temporary note to forget.", "reference"))
        .await;
    let Some(ResponseData::Ingest(data)) = &ingest_resp.data else {
        panic!("seed ingest must commit: {ingest_resp:?}");
    };
    let record_id = data.record_id.0.clone();

    let resp = host
        .forget(ForgetArgs::Record {
            dry_run: None,
            human_review: None,
            no_diff: None,
            record_id: Ulid(record_id),
        })
        .await;

    assert!(
        matches!(resp.status, ResponseStatus::Committed),
        "forget --record should commit; got {resp:?}"
    );
    assert!(matches!(resp.verb, ResponseVerb::Forget));
    let Some(ResponseData::Forget(data)) = &resp.data else {
        panic!("committed forget envelope must carry ForgetData: {resp:?}");
    };
    assert!(data.deleted_count >= 1);
    assert_eq!(active_record_count(vault.path()), 0);
}

#[tokio::test]
async fn host_forget_scope_is_capability_unavailable() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    let host = host_for(vault.path());

    let resp = host
        .forget(ForgetArgs::Scope {
            dry_run: None,
            human_review: None,
            no_diff: None,
            scope: cairn_core::generated::common::ScopeFilter {
                agent: None,
                entity: None,
                kind: None,
                record_ids: None,
                session_id: None,
                tags: None,
                tenant: Some("acme".to_owned()),
                tier: None,
                user: None,
                workspace: None,
            },
        })
        .await;

    assert!(
        matches!(resp.status, ResponseStatus::Rejected),
        "scope forget is unwired (FORGET_SCOPE_WIRED=false) and must fail closed; got {resp:?}"
    );
    assert_eq!(error_code(&resp), Some("CapabilityUnavailable"));
}

#[tokio::test]
async fn host_forget_flush_plan_modes_fail_closed() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    let host = host_for(vault.path());

    let resp = host
        .forget(ForgetArgs::Record {
            dry_run: Some(true),
            human_review: None,
            no_diff: None,
            record_id: Ulid("01HQZX9F5N0000000000000000".to_owned()),
        })
        .await;

    assert!(
        matches!(resp.status, ResponseStatus::Rejected),
        "flush-plan forget modes are CLI-only placeholders and must fail closed over MCP; got {resp:?}"
    );
}

// ── capture_trace fixture (mirrors capture_trace_verb.rs) ───────────────────

fn sha256_hex(text: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    format!("{:x}", h.finalize())
}

fn write_source(vault: &Path, filename: &str, content: &str) -> String {
    let dir = vault.join("sources").join("hook");
    std::fs::create_dir_all(&dir).expect("create sources/hook");
    std::fs::write(dir.join(filename), content).expect("write source file");
    format!("sources/hook/{filename}")
}

#[allow(clippy::too_many_arguments)]
fn make_hook_event(
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
            tool_id,
        }),
        payload_hash: PayloadHash::parse(format!("sha256:{payload_hash_hex}"))
            .expect("invariant: valid sha256"),
        payload_ref: payload_ref.to_owned(),
        captured_at: Rfc3339Timestamp::parse(timestamp).expect("invariant: valid RFC-3339"),
        payload: CapturePayload::Hook {
            hook_name: hook_name.to_owned(),
            tool_name: None,
        },
        source_family: SourceFamily::Hook,
    }
}

fn write_single_turn_fixture(vault: &Path, jsonl_path: &Path) {
    let session = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let tool = "toolu_test_01";
    let user_body = "Hello, please run ls";
    let pre_body = r#"{"tool":"bash","input":{"command":"ls"}}"#;
    let post_body = r#"{"tool":"bash","output":"file.txt"}"#;
    let stop_body = "session ended";

    let cases = [
        (
            "01ARZ3NDEKTSV4RRFFQ69G5FAA",
            "UserPromptSubmit",
            None,
            user_body,
            "2026-05-02T00:00:01Z",
        ),
        (
            "01ARZ3NDEKTSV4RRFFQ69G5FAB",
            "PreToolUse",
            Some(tool.to_owned()),
            pre_body,
            "2026-05-02T00:00:02Z",
        ),
        (
            "01ARZ3NDEKTSV4RRFFQ69G5FAC",
            "PostToolUse",
            Some(tool.to_owned()),
            post_body,
            "2026-05-02T00:00:03Z",
        ),
        (
            "01ARZ3NDEKTSV4RRFFQ69G5FAD",
            "Stop",
            None,
            stop_body,
            "2026-05-02T00:00:04Z",
        ),
    ];

    let mut f = std::fs::File::create(jsonl_path).expect("create JSONL file");
    for (id, hook_name, tool_id, body, ts) in cases {
        let payload_ref = write_source(vault, &format!("{id}.txt"), body);
        let event = make_hook_event(
            id,
            hook_name,
            session,
            "turn-1",
            ts,
            tool_id,
            &payload_ref,
            &sha256_hex(body),
        );
        let line = serde_json::to_string(&event).expect("serialize event");
        writeln!(f, "{line}").expect("write JSONL line");
    }
}

#[tokio::test]
async fn host_capture_trace_jsonl_commits() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    // Hook events pass the sensor gate only after operator opt-in; the
    // host must read the post-opt-in config, as `cairn mcp` does at boot.
    enable_hook_sensor(vault.path());
    let jsonl_path = vault.path().join("trace.jsonl");
    write_single_turn_fixture(vault.path(), &jsonl_path);
    let host = CliMutationHost::new(vault.path().to_path_buf(), vault_config(vault.path()));

    let resp = host
        .capture_trace(CaptureTraceArgs {
            blocks: None,
            from: Some(jsonl_path.to_string_lossy().into_owned()),
            session_id: None,
        })
        .await;

    assert!(
        matches!(resp.status, ResponseStatus::Committed),
        "capture_trace --from should commit; got {resp:?}"
    );
    assert!(matches!(resp.verb, ResponseVerb::CaptureTrace));
    let Some(ResponseData::CaptureTrace(data)) = &resp.data else {
        panic!("committed capture_trace envelope must carry CaptureTraceData: {resp:?}");
    };
    assert!(data.failed_turns.is_empty(), "no failed turns expected");
    assert_eq!(data.trace_id.0.len(), 26, "trace_id must be a ULID");
}

#[tokio::test]
async fn host_capture_trace_blocks_without_session_rejects() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    let host = host_for(vault.path());

    let resp = host
        .capture_trace(CaptureTraceArgs {
            blocks: Some("/tmp/blocks.json".to_owned()),
            from: None,
            session_id: None,
        })
        .await;

    assert!(
        matches!(resp.status, ResponseStatus::Rejected),
        "blocks without a session id must reject (mirrors CLI); got {resp:?}"
    );
    assert_eq!(error_code(&resp), Some("InvalidArgs"));
}
