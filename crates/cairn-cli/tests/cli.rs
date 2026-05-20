//! End-to-end CLI smoke tests. Invokes the built `cairn` binary and asserts
//! the P0 CLI behaviour: help succeeds, usage errors return sysexits, and
//! wired verbs emit valid JSON envelopes.
//!
//! The CLI tree is generated from the IDL by `cairn-codegen`; the store is
//! Exit-code contract (spec §5.2):
//! - committed verb paths exit 0 and emit `"status":"committed"`.
//! - remaining simple verb stubs exit 1, stderr contains `Internal`, or
//!   `--json` emits `"status":"aborted"`.
//! - clap usage errors (unknown flag, unknown subcommand, missing required
//!   `ArgGroup`, bare invocation with `subcommand_required`) → 64
//!   (`EX_USAGE`).
//! - bundled `plugin.toml` parse failure → 78 (`EX_CONFIG`); registry
//!   rejection → 69 (`EX_UNAVAILABLE`).

use std::io::Write as _;
use std::path::Path;
use std::process::Command;

use cairn_core::domain::Identity;
use cairn_core::domain::session::SessionIdentity;
use sha2::{Digest, Sha256};

/// Path to the built CLI binary. Cargo sets `CARGO_BIN_EXE_<name>` for every
/// binary in the current crate at test-compile time.
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

fn enable_local_sensor(vault: &Path, sensor: &str) {
    let out = cli()
        .current_dir(vault)
        .args([
            "sensor",
            "enable",
            sensor,
            "--reason",
            "operator_on",
            "--json",
        ])
        .output()
        .unwrap_or_else(|err| panic!("cairn sensor enable {sensor}: {err}"));
    assert!(
        out.status.success(),
        "sensor enable {sensor} failed: {:?}\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn session_identity(project_root: &str) -> SessionIdentity {
    SessionIdentity::new(
        Identity::parse("hmn:cli-session-tree").expect("valid user identity"),
        Identity::parse("agt:cli:test:session-tree:v1").expect("valid agent identity"),
        Some(project_root.to_owned()),
    )
    .expect("valid session identity")
}

fn seed_consent_journal_vault(seed_sql: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let cairn_dir = dir.path().join(".cairn");
    std::fs::create_dir_all(&cairn_dir).expect("create .cairn");
    let db_path = cairn_dir.join("cairn.db");
    let mut conn = rusqlite::Connection::open(db_path).expect("open sqlite");
    cairn_store_sqlite::migrations::migrations()
        .to_version(&mut conn, 20)
        .expect("apply migrations to v20");
    conn.execute(seed_sql, [])
        .expect("seed consent_journal row");
    dir
}

fn seed_consent_journal_repair_vault() -> tempfile::TempDir {
    seed_consent_journal_vault(
        "INSERT INTO consent_journal \
          (rowid, consent_id, subject, scope, decision, granted_by, decided_at) \
         VALUES (0, 'repair-cli', 'sub', 'private', 'GRANT', 'hmn:t', 0)",
    )
}

fn seed_zero_capture_metrics_vault(body: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: dir.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    let cairn_dir = dir.path().join(".cairn");
    std::fs::write(cairn_dir.join("metrics.jsonl"), body).expect("write metrics.jsonl");
    dir
}

fn write_stop_trace_fixture(vault: &Path) -> std::path::PathBuf {
    let sources_dir = vault.join("sources").join("hook");
    std::fs::create_dir_all(&sources_dir).expect("create hook sources");
    let inputs = [
        (
            "01ARZ3NDEKTSV4RRFFQ69G5FAA",
            "UserPromptSubmit",
            None,
            "2026-05-02T00:00:01Z",
            "user asks for a summary",
        ),
        (
            "01ARZ3NDEKTSV4RRFFQ69G5FAB",
            "PreToolUse",
            Some("tool-call-1"),
            "2026-05-02T00:00:02Z",
            r#"{"tool":"Read","input":{"file_path":"README.md"}}"#,
        ),
        (
            "01ARZ3NDEKTSV4RRFFQ69G5FAC",
            "PostToolUse",
            Some("tool-call-1"),
            "2026-05-02T00:00:03Z",
            "tool returned README contents",
        ),
        (
            "01ARZ3NDEKTSV4RRFFQ69G5FAD",
            "Stop",
            None,
            "2026-05-02T00:00:04Z",
            "session ended",
        ),
    ];
    let trace_path = vault.join("trace.jsonl");
    let mut file = std::fs::File::create(&trace_path).expect("create trace jsonl");
    for (event_id, hook_name, tool_id, captured_at, body) in inputs {
        let file_name = format!("{event_id}.txt");
        let payload_path = sources_dir.join(&file_name);
        std::fs::write(&payload_path, body).expect("write hook payload");
        let hash = format!("sha256:{:x}", Sha256::digest(body.as_bytes()));
        let event = serde_json::json!({
            "event_id": event_id,
            "sensor_id": "snr:local:hook:cc-session:v1",
            "capture_mode": "auto",
            "actor_chain": [{
                "role": "author",
                "identity": "snr:local:hook:cc-session:v1",
                "at": captured_at
            }],
            "refs": {
                "session_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "turn_id": "turn-1",
                "tool_id": tool_id
            },
            "payload_hash": hash,
            "payload_ref": format!("sources/hook/{file_name}"),
            "captured_at": captured_at,
            "payload": {
                "source_family": "hook",
                "hook_name": hook_name
            },
            "source_family": "hook"
        });
        writeln!(file, "{event}").expect("write trace event");
    }
    trace_path
}

#[test]
fn prints_version_with_flag() {
    let out = cli().arg("--version").output().expect("cairn --version");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(stdout.starts_with("cairn "), "got: {stdout:?}");
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "got: {stdout:?}"
    );
}

#[test]
fn help_flag_lists_core_commands() {
    let out = cli().arg("--help").output().expect("cairn --help");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    for verb in [
        "ingest",
        "search",
        "retrieve",
        "summarize",
        "assemble_hot",
        "capture_trace",
        "lint",
        "forget",
        "hook",
        "nexus",
    ] {
        assert!(
            stdout.contains(verb),
            "help output missing verb {verb}, got:\n{stdout}",
        );
    }
}

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

#[test]
fn ingest_recording_counts_as_source_and_validates_missing_path() {
    let out = cli()
        .args([
            "ingest",
            "--kind",
            "reference",
            "--recording",
            "meeting.mp4",
        ])
        .output()
        .expect("ingest --recording");
    assert_eq!(out.status.code(), Some(64));
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("path does not exist: meeting.mp4"),
        "recording should be counted as the selected source before path validation, stderr: {stderr}",
    );
}

#[test]
fn ingest_recording_conflicts_with_other_sources() {
    let out = cli()
        .args([
            "ingest",
            "--kind",
            "reference",
            "--body",
            "note",
            "--recording",
            "meeting.mp4",
        ])
        .output()
        .expect("ingest conflicting recording");
    assert_eq!(out.status.code(), Some(64));
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("exactly one of")
            && stderr.contains("--recording")
            && stderr.contains("got 2"),
        "recording conflict should fail source counting, stderr: {stderr}",
    );
}

#[test]
fn ingest_recording_conflicts_with_jsonl_before_jsonl_dispatch() {
    let out = cli()
        .args([
            "ingest",
            "--kind",
            "reference",
            "--jsonl",
            "missing.jsonl",
            "--recording",
            "meeting.mp4",
        ])
        .output()
        .expect("ingest jsonl recording conflict");
    assert_eq!(out.status.code(), Some(64));
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("exactly one of")
            && stderr.contains("--recording")
            && stderr.contains("got 2"),
        "recording/jsonl conflict should fail source counting before JSONL dispatch, stderr: {stderr}",
    );
}

#[test]
fn session_tree_subcommand_parses_scriptable_json_inspection() {
    let matches = cairn_cli::command::build_command()
        .try_get_matches_from([
            "cairn",
            "session",
            "tree",
            "01JTS6R4J70000000000000000",
            "--json",
        ])
        .expect("session tree command parses");

    let Some(("session", session)) = matches.subcommand() else {
        panic!("expected session subcommand");
    };
    let Some(("tree", tree)) = session.subcommand() else {
        panic!("expected session tree subcommand");
    };
    assert_eq!(
        tree.get_one::<String>("session").map(String::as_str),
        Some("01JTS6R4J70000000000000000")
    );
    assert!(tree.get_flag("json"));
}

#[test]
fn session_tree_json_inspects_seeded_vault_tree() {
    let dir = tempfile::tempdir().expect("tempdir");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: dir.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");

    let db_path = dir.path().join(".cairn/cairn.db");
    let (root_id, child_id) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(async {
            let store = cairn_store_sqlite::open(&db_path)
                .await
                .expect("open store");
            let root = store
                .create_session(
                    &session_identity("/cli-session-tree-root"),
                    cairn_store_sqlite::NewSessionMetadata::default(),
                )
                .await
                .expect("create root session");
            let child = store
                .create_session(
                    &session_identity("/cli-session-tree-child"),
                    cairn_store_sqlite::NewSessionMetadata::default(),
                )
                .await
                .expect("create child session");
            store
                .record_session_fork(&root.id, &child.id, "turn-2")
                .await
                .expect("record fork");
            (root.id, child.id)
        });

    let out = cli()
        .current_dir(dir.path())
        .args(["session", "tree", root_id.as_str(), "--json"])
        .output()
        .expect("cairn session tree --json");

    assert!(
        out.status.success(),
        "session tree failed: {:?}\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let body: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("session tree stdout is json");
    assert_eq!(body["root"], root_id.as_str());
    assert_eq!(body["nodes"].as_array().expect("nodes array").len(), 2);
    let child = body["nodes"]
        .as_array()
        .expect("nodes array")
        .iter()
        .find(|node| node["id"] == child_id.as_str())
        .expect("child node present");
    assert_eq!(child["parentId"], root_id.as_str());
    assert_eq!(child["branchKind"], "fork");
    assert_eq!(child["atTurnId"], "turn-2");
    assert!(body["merges"].as_array().expect("merges array").is_empty());
}

#[test]
fn ingest_recording_conflicts_with_folder_before_folder_dispatch() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = cli()
        .args([
            "ingest",
            "--kind",
            "reference",
            "--folder",
            dir.path().to_str().expect("utf-8 path"),
            "--recording",
            "meeting.mp4",
        ])
        .output()
        .expect("ingest folder recording conflict");
    assert_eq!(out.status.code(), Some(64));
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("exactly one of")
            && stderr.contains("--recording")
            && stderr.contains("got 2"),
        "recording/folder conflict should fail source counting before folder dispatch, stderr: {stderr}",
    );
}

#[test]
fn ingest_recording_conflicts_with_positional_folder_before_folder_dispatch() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = cli()
        .args([
            "ingest",
            "--kind",
            "reference",
            dir.path().to_str().expect("utf-8 path"),
            "--recording",
            "meeting.mp4",
        ])
        .output()
        .expect("ingest positional folder recording conflict");
    assert_eq!(out.status.code(), Some(64));
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("exactly one of")
            && stderr.contains("--recording")
            && stderr.contains("got 2"),
        "recording/positional-folder conflict should fail source counting before folder dispatch, stderr: {stderr}",
    );
}

#[test]
fn ingest_recording_dry_run_does_not_succeed() {
    let out = cli()
        .args([
            "ingest",
            "--kind",
            "reference",
            "--recording",
            "meeting.mp4",
            "--dry-run",
        ])
        .output()
        .expect("ingest recording dry-run");
    assert_eq!(out.status.code(), Some(64));
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("path does not exist: meeting.mp4"),
        "recording dry-run should route through recording validation, stderr: {stderr}",
    );
}

#[test]
fn mcp_subcommand_help_exits_zero() {
    let out = cli()
        .args(["mcp", "--help"])
        .output()
        .expect("cairn mcp --help");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(
        stdout.contains("stdio"),
        "mcp help should describe stdio transport: {stdout:?}",
    );
}

#[test]
fn repair_consent_journal_help_lists_delete_options() {
    let out = cli()
        .args(["repair", "consent-journal", "--help"])
        .output()
        .expect("cairn repair consent-journal --help");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    for needle in ["--delete-rowid", "--reason", "--yes", "--json"] {
        assert!(
            stdout.contains(needle),
            "repair help missing {needle}: {stdout}",
        );
    }
}

#[test]
fn search_and_retrieve_help_list_include_reasoning_flag() {
    for command in [["search", "--help"], ["retrieve", "--help"]] {
        let out = cli().args(command).output().expect("verb --help");
        assert!(out.status.success(), "exit: {:?}", out.status);
        let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
        assert!(
            stdout.contains("--include-reasoning"),
            "help output missing --include-reasoning for {command:?}: {stdout}",
        );
    }
}

#[test]
fn capture_trace_help_lists_blocks_flag() {
    let out = cli()
        .args(["capture_trace", "--help"])
        .output()
        .expect("capture_trace --help");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    for needle in ["--blocks", "--from", "--session"] {
        assert!(
            stdout.contains(needle),
            "capture_trace help missing {needle}: {stdout}",
        );
    }
}

#[test]
fn repair_delete_requires_reason_and_yes() {
    let missing_yes = cli()
        .args([
            "repair",
            "consent-journal",
            "--delete-rowid",
            "0",
            "--reason",
            "operator approved",
        ])
        .output()
        .expect("cairn repair delete without yes");
    assert_eq!(missing_yes.status.code(), Some(64));

    let missing_reason = cli()
        .args(["repair", "consent-journal", "--delete-rowid", "0", "--yes"])
        .output()
        .expect("cairn repair delete without reason");
    assert_eq!(missing_reason.status.code(), Some(64));
}

#[test]
fn repair_consent_journal_json_lists_blockers() {
    let dir = seed_consent_journal_repair_vault();
    let out = cli()
        .args([
            "--vault",
            dir.path().to_str().expect("utf-8 tempdir"),
            "repair",
            "consent-journal",
            "--json",
        ])
        .output()
        .expect("cairn repair consent-journal --json");
    assert!(
        out.status.success(),
        "exit: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("repair list must emit JSON");
    let blockers = parsed["blockers"]
        .as_array()
        .expect("blockers should be an array");
    assert_eq!(blockers.len(), 1);
    assert_eq!(blockers[0]["rowid"], 0);
    assert_eq!(blockers[0]["blocker_codes"][0], "non_positive_rowid");
}

#[test]
fn repair_consent_journal_delete_succeeds_and_reports_receipt() {
    let dir = seed_consent_journal_repair_vault();
    let out = cli()
        .args([
            "--vault",
            dir.path().to_str().expect("utf-8 tempdir"),
            "repair",
            "consent-journal",
            "--delete-rowid",
            "0",
            "--reason",
            "operator approved",
            "--yes",
            "--json",
        ])
        .output()
        .expect("cairn repair consent-journal delete");
    assert!(
        out.status.success(),
        "exit: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("repair delete must emit JSON");
    assert_eq!(parsed["deleted"]["target_rowid"], 0);
    assert_eq!(parsed["deleted"]["reason"], "operator approved");

    let conn =
        rusqlite::Connection::open(dir.path().join(".cairn").join("cairn.db")).expect("open db");
    let remaining: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM consent_journal WHERE consent_id = 'repair-cli'",
            [],
            |row| row.get(0),
        )
        .expect("count remaining");
    assert_eq!(remaining, 0);
    let audit_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM consent_journal_repair_audit",
            [],
            |row| row.get(0),
        )
        .expect("count audit rows");
    assert_eq!(audit_count, 1);
}

#[test]
fn repair_consent_journal_delete_non_blocker_exits_65() {
    let dir = seed_consent_journal_vault(
        "INSERT INTO consent_journal \
          (consent_id, subject, scope, decision, granted_by, decided_at) \
         VALUES ('not-a-blocker', 'sub', 'private', 'GRANT', 'hmn:t', 0)",
    );
    let out = cli()
        .args([
            "--vault",
            dir.path().to_str().expect("utf-8 tempdir"),
            "repair",
            "consent-journal",
            "--delete-rowid",
            "1",
            "--reason",
            "operator approved",
            "--yes",
        ])
        .output()
        .expect("cairn repair consent-journal delete non-blocker");
    assert_eq!(out.status.code(), Some(65));
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("not repair-eligible"),
        "stderr missing repair eligibility error: {stderr:?}",
    );
}

#[test]
fn admin_zero_capture_report_renders_markdown() {
    let vault = seed_zero_capture_metrics_vault(
        r#"{"event":"zero_capture_audit","session_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV","activity_count":3,"successful_ingest_writes":0,"successful_capture_trace_writes":0,"successful_write_count":0,"decision":"emit_nudge"}
"#,
    );
    let out = cli()
        .args([
            "--vault",
            vault.path().to_str().expect("utf-8 vault path"),
            "admin",
            "zero-capture-report",
        ])
        .output()
        .expect("cairn admin zero-capture-report");
    assert!(
        out.status.success(),
        "exit: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(stdout.contains("# Zero-capture report"));
    assert!(stdout.contains("- emit_nudge: 1"));
    assert!(stdout.contains("decision: emit_nudge"));
}

#[test]
fn admin_zero_capture_report_json_emits_summary_and_reports() {
    let vault = seed_zero_capture_metrics_vault(
        r#"{"event":"accepted","record_id":"01ARZ3NDEKTSV4RRFFQ69G5FAA","kind":"user","class":"semantic","visibility":"private","scope":{"session_id":"ignored"},"source_family":"cli","capture_mode":"explicit","rank":1}
{"event":"zero_capture_audit","session_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV","activity_count":3,"successful_ingest_writes":0,"successful_capture_trace_writes":0,"successful_write_count":0,"decision":"emit_nudge"}
{"event":"zero_capture_audit","session_id":"01ARZ3NDEKTSV4RRFFQ69G5FAW","activity_count":0,"successful_ingest_writes":0,"successful_capture_trace_writes":0,"successful_write_count":0,"decision":"no_meaningful_activity"}
"#,
    );
    let out = cli()
        .args([
            "--vault",
            vault.path().to_str().expect("utf-8 vault path"),
            "admin",
            "zero-capture-report",
            "--json",
        ])
        .output()
        .expect("cairn admin zero-capture-report --json");
    assert!(
        out.status.success(),
        "exit: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("report json should parse");
    assert_eq!(parsed["summary"]["total"], 2);
    assert_eq!(parsed["summary"]["emit_nudge"], 1);
    assert_eq!(parsed["summary"]["no_meaningful_activity"], 1);
    assert_eq!(parsed["reports"][0]["decision"], "emit_nudge");
}

#[test]
fn admin_zero_capture_report_skips_malformed_unrelated_metrics_rows() {
    let vault = seed_zero_capture_metrics_vault(
        r#"not json
{"event":"zero_capture_audit","session_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV","activity_count":3,"successful_ingest_writes":0,"successful_capture_trace_writes":0,"successful_write_count":0,"decision":"emit_nudge"}
"#,
    );
    let out = cli()
        .args([
            "--vault",
            vault.path().to_str().expect("utf-8 vault path"),
            "admin",
            "zero-capture-report",
            "--json",
        ])
        .output()
        .expect("cairn admin zero-capture-report --json");
    assert!(
        out.status.success(),
        "exit: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("report json should parse");
    assert_eq!(parsed["summary"]["total"], 1);
    assert_eq!(parsed["summary"]["emit_nudge"], 1);
}

#[test]
fn admin_zero_capture_report_skips_malformed_diagnostic_that_mentions_audit() {
    let vault = seed_zero_capture_metrics_vault(
        r#"{"event":"diagnostic","message":"zero_capture_audit row truncated"
{"event":"zero_capture_audit","session_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV","activity_count":3,"successful_ingest_writes":0,"successful_capture_trace_writes":0,"successful_write_count":0,"decision":"emit_nudge"}
"#,
    );
    let out = cli()
        .args([
            "--vault",
            vault.path().to_str().expect("utf-8 vault path"),
            "admin",
            "zero-capture-report",
            "--json",
        ])
        .output()
        .expect("cairn admin zero-capture-report --json");
    assert!(
        out.status.success(),
        "exit: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("report json should parse");
    assert_eq!(parsed["summary"]["total"], 1);
}

#[test]
fn admin_zero_capture_report_skips_malformed_prefix_event_names() {
    let vault = seed_zero_capture_metrics_vault(
        r#"{"event":"zero_capture_audit_debug"
{"event":"zero_capture_audit","session_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV","activity_count":3,"successful_ingest_writes":0,"successful_capture_trace_writes":0,"successful_write_count":0,"decision":"emit_nudge"}
"#,
    );
    let out = cli()
        .args([
            "--vault",
            vault.path().to_str().expect("utf-8 vault path"),
            "admin",
            "zero-capture-report",
            "--json",
        ])
        .output()
        .expect("cairn admin zero-capture-report --json");
    assert!(
        out.status.success(),
        "exit: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("report json should parse");
    assert_eq!(parsed["summary"]["total"], 1);
}

#[test]
fn admin_zero_capture_report_rejects_malformed_audit_metrics_rows() {
    let vault = seed_zero_capture_metrics_vault(r#"{"event":"zero_capture_audit""#);
    let out = cli()
        .args([
            "--vault",
            vault.path().to_str().expect("utf-8 vault path"),
            "admin",
            "zero-capture-report",
            "--json",
        ])
        .output()
        .expect("cairn admin zero-capture-report --json");
    assert_eq!(
        out.status.code(),
        Some(65),
        "malformed zero_capture_audit rows must fail; stdout: {} stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("InvalidInput"),
        "stderr should include InvalidInput JSON: {stderr}"
    );
}

#[test]
fn no_args_prints_help_and_fails_closed() {
    // Generated `command()` sets subcommand_required(true) and
    // arg_required_else_help(true), so a bare `cairn` invocation is a clap
    // usage error → exit 64 (EX_USAGE) per spec §5.2.
    let out = cli().output().expect("cairn");
    assert!(!out.status.success(), "bare cairn exited OK");
    assert_eq!(out.status.code(), Some(64));
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("ingest"),
        "help text missing verb listing: {stderr:?}",
    );
}

#[test]
fn capture_trace_empty_file_exits_zero() {
    let vault = tempfile::tempdir().expect("temp vault");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    let trace_path = vault.path().join("trace.jsonl");
    std::fs::write(&trace_path, "").expect("write empty trace");

    let out = cli()
        .args([
            "--vault",
            vault.path().to_str().expect("utf-8 vault"),
            "capture_trace",
            "--from",
            trace_path.to_str().expect("utf-8 trace path"),
        ])
        .output()
        .expect("cairn capture_trace --from <empty>");
    assert!(
        out.status.success(),
        "capture_trace should succeed for empty import; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn capture_trace_unbound_vault_exits_ex_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    let trace_path = dir.path().join("trace.jsonl");
    std::fs::write(&trace_path, "").expect("write empty trace");

    let out = cli()
        .current_dir(dir.path())
        .args([
            "capture_trace",
            "--from",
            trace_path.to_str().expect("utf-8 trace path"),
        ])
        .output()
        .expect("cairn capture_trace --from <empty>");
    assert_eq!(
        out.status.code(),
        Some(78),
        "capture_trace must fail closed outside a bound vault; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn capture_trace_unbound_vault_json_emits_envelope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let trace_path = dir.path().join("trace.jsonl");
    std::fs::write(&trace_path, "").expect("write empty trace");

    let out = cli()
        .current_dir(dir.path())
        .args([
            "capture_trace",
            "--from",
            trace_path.to_str().expect("utf-8 trace path"),
            "--json",
        ])
        .output()
        .expect("cairn capture_trace --json --from <empty>");
    assert_eq!(
        out.status.code(),
        Some(78),
        "capture_trace must fail closed outside a bound vault; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let response: cairn_core::generated::envelope::Response = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("capture_trace error envelope parse failed: {e}\n{stdout}"));
    assert!(matches!(
        response.status,
        cairn_core::generated::envelope::ResponseStatus::Aborted
    ));
    assert_eq!(
        response
            .error
            .as_ref()
            .and_then(|error| error.get("code"))
            .and_then(serde_json::Value::as_str),
        Some("NotFound")
    );
}

#[test]
fn capture_trace_missing_named_vault_json_emits_envelope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let trace_path = dir.path().join("trace.jsonl");
    let registry_path = dir.path().join("vaults.toml");
    std::fs::write(&trace_path, "").expect("write empty trace");
    cairn_cli::vault::VaultRegistryStore::new(registry_path.clone())
        .save(&cairn_core::config::VaultRegistry::default())
        .expect("seed empty registry");

    let out = cli()
        .current_dir(dir.path())
        .env("CAIRN_REGISTRY", &registry_path)
        .args([
            "--vault",
            "missing-vault",
            "capture_trace",
            "--from",
            trace_path.to_str().expect("utf-8 trace path"),
            "--json",
        ])
        .output()
        .expect("cairn --vault missing-vault capture_trace --json");
    assert_eq!(
        out.status.code(),
        Some(78),
        "capture_trace must fail closed for missing named vault; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let response: cairn_core::generated::envelope::Response = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("capture_trace error envelope parse failed: {e}\n{stdout}"));
    assert!(matches!(
        response.verb,
        cairn_core::generated::envelope::ResponseVerb::CaptureTrace
    ));
    assert_eq!(
        response
            .error
            .as_ref()
            .and_then(|error| error.get("code"))
            .and_then(serde_json::Value::as_str),
        Some("NotFound")
    );
}

#[test]
fn capture_trace_malformed_registry_json_emits_envelope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let trace_path = dir.path().join("trace.jsonl");
    let registry_path = dir.path().join("vaults.toml");
    std::fs::write(&trace_path, "").expect("write empty trace");
    std::fs::write(&registry_path, "not valid toml").expect("write malformed registry");

    let out = cli()
        .current_dir(dir.path())
        .env("CAIRN_REGISTRY", &registry_path)
        .args([
            "--vault",
            "missing-vault",
            "capture_trace",
            "--from",
            trace_path.to_str().expect("utf-8 trace path"),
            "--json",
        ])
        .output()
        .expect("cairn --vault missing-vault capture_trace --json");
    assert_eq!(
        out.status.code(),
        Some(78),
        "capture_trace must fail closed for malformed registry; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let response: cairn_core::generated::envelope::Response = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("capture_trace error envelope parse failed: {e}\n{stdout}"));
    assert!(matches!(
        response.verb,
        cairn_core::generated::envelope::ResponseVerb::CaptureTrace
    ));
    assert_eq!(
        response
            .error
            .as_ref()
            .and_then(|error| error.get("code"))
            .and_then(serde_json::Value::as_str),
        Some("Internal")
    );
}

#[test]
fn capture_trace_registry_path_error_json_emits_envelope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let trace_path = dir.path().join("trace.jsonl");
    std::fs::write(&trace_path, "").expect("write empty trace");

    let out = cli()
        .current_dir(dir.path())
        .env_remove("CAIRN_REGISTRY")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("HOME")
        .args([
            "--vault",
            "missing-vault",
            "capture_trace",
            "--from",
            trace_path.to_str().expect("utf-8 trace path"),
            "--json",
        ])
        .output()
        .expect("cairn --vault missing-vault capture_trace --json without registry env");
    assert_eq!(
        out.status.code(),
        Some(78),
        "capture_trace must fail closed for registry path errors; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let response: cairn_core::generated::envelope::Response = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("capture_trace error envelope parse failed: {e}\n{stdout}"));
    assert!(matches!(
        response.verb,
        cairn_core::generated::envelope::ResponseVerb::CaptureTrace
    ));
    assert_eq!(
        response
            .error
            .as_ref()
            .and_then(|error| error.get("code"))
            .and_then(serde_json::Value::as_str),
        Some("Internal")
    );
}

#[test]
fn capture_trace_e2e_skips_malformed_prefix_metric_event() {
    let vault = tempfile::tempdir().expect("temp vault");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    enable_local_sensor(vault.path(), "hook");
    let trace_path = write_stop_trace_fixture(vault.path());
    std::fs::write(
        vault.path().join(".cairn").join("metrics.jsonl"),
        r#"{"event":"accepted_debug""#,
    )
    .expect("write malformed prefix metric row");

    let out = cli()
        .args([
            "--vault",
            vault.path().to_str().expect("utf-8 vault"),
            "capture_trace",
            "--from",
            trace_path.to_str().expect("utf-8 trace path"),
            "--json",
        ])
        .output()
        .expect("cairn capture_trace --json");
    assert!(
        out.status.success(),
        "capture_trace should succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("capture_trace envelope should parse");
    assert_eq!(parsed["status"], "committed");
    assert_eq!(
        parsed["data"]["failed_turns"]
            .as_array()
            .expect("failed_turns array")
            .len(),
        0
    );
    let metrics = std::fs::read_to_string(vault.path().join(".cairn").join("metrics.jsonl"))
        .expect("read metrics");
    assert!(
        metrics.contains(r#""event":"zero_capture_audit""#),
        "audit row should be appended despite malformed prefix row: {metrics}"
    );
}

#[test]
fn capture_trace_e2e_reports_malformed_exact_accepted_metric() {
    let vault = tempfile::tempdir().expect("temp vault");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    enable_local_sensor(vault.path(), "hook");
    let trace_path = write_stop_trace_fixture(vault.path());
    std::fs::write(
        vault.path().join(".cairn").join("metrics.jsonl"),
        r#"{"event":"accepted""#,
    )
    .expect("write malformed accepted metric row");

    let out = cli()
        .args([
            "--vault",
            vault.path().to_str().expect("utf-8 vault"),
            "capture_trace",
            "--from",
            trace_path.to_str().expect("utf-8 trace path"),
            "--json",
        ])
        .output()
        .expect("cairn capture_trace --json");
    assert!(
        out.status.success(),
        "capture_trace should surface per-turn failures in committed response; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("capture_trace envelope should parse");
    let failed_turns = parsed["data"]["failed_turns"]
        .as_array()
        .expect("failed_turns array");
    assert_eq!(failed_turns.len(), 1);
    assert!(
        failed_turns[0]["reason"]
            .as_str()
            .expect("failure reason")
            .contains("turn_failed"),
        "unexpected failed_turns: {failed_turns:?}"
    );
    let metrics = std::fs::read_to_string(vault.path().join(".cairn").join("metrics.jsonl"))
        .expect("read metrics");
    assert!(
        !metrics.contains(r#""event":"zero_capture_audit""#),
        "audit row should not be appended when accepted metric parsing fails: {metrics}"
    );
}

#[test]
fn assemble_hot_exits_zero_and_emits_committed_envelope() {
    // `assemble_hot` is wired to real hot-memory sources and returns a
    // committed Response with six segments for the default recipe. Exit 0.
    // The verb fails closed on a non-vault directory, so bootstrap a
    // tempdir vault and run from inside it.
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
        .expect("cairn assemble_hot --json");
    assert!(
        out.status.success(),
        "assemble_hot exited non-zero: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("expected valid JSON on stdout");
    assert_eq!(v["contract"], "cairn.mcp.v1");
    assert_eq!(v["status"], "committed");
    assert_eq!(v["verb"], "assemble_hot");
    assert!(v["data"]["segments"].is_array(), "segments must be present");
    assert_eq!(
        v["data"]["segments"].as_array().map(Vec::len),
        Some(6),
        "default recipe has 6 steps"
    );
}

#[test]
fn search_missing_query_exits_64() {
    // `query` is required by the IDL schema and generated clap surface, so
    // clap rejects the invocation before dispatch.
    let out = cli()
        .args(["search", "--mode", "keyword"])
        .output()
        .expect("cairn search --mode keyword");
    assert_eq!(
        out.status.code(),
        Some(64),
        "search without query must exit 64 (EX_USAGE); got {:?}",
        out.status
    );
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("required"),
        "stderr must surface required query: {stderr:?}",
    );
}

#[test]
fn simple_verb_json_mode_emits_committed_ingest_envelope() {
    let vault = tempfile::tempdir().expect("vault");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap");
    let out = cli()
        .current_dir(vault.path())
        .args(["ingest", "--kind", "user", "--body", "hi", "--json"])
        .output()
        .expect("cairn ingest --json");
    assert_eq!(
        out.status.code(),
        Some(0),
        "exit: {:?}; stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("expected valid JSON on stdout");
    assert_eq!(v["contract"], "cairn.mcp.v1");
    assert_eq!(v["status"], "committed");
    assert!(v["data"]["record_id"].is_string());
    assert!(v["policy_trace"].is_array());
}

#[test]
fn ingest_with_no_source_exits_64() {
    // Bare `cairn ingest` (no body/file/url/source) must fail with usage error, not Internal.
    let out = cli().arg("ingest").output().expect("cairn ingest");
    assert_eq!(out.status.code(), Some(64), "exit: {:?}", out.status);
}

#[test]
fn lint_accepts_fix_flag() {
    let out = cli()
        .args(["lint", "--fix", "--json"])
        .output()
        .expect("cairn lint --fix --json");
    assert_ne!(
        out.status.code(),
        Some(64),
        "lint --fix should parse as a verb flag; stderr: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn ingest_with_conflicting_sources_exits_64() {
    // Providing both --body and --file violates the IDL exactly-one-of constraint.
    let out = cli()
        .args([
            "ingest",
            "--kind",
            "user",
            "--body",
            "a",
            "--file",
            "/dev/null",
        ])
        .output()
        .expect("cairn ingest --body --file");
    assert_eq!(out.status.code(), Some(64), "exit: {:?}", out.status);
    let stderr = String::from_utf8(out.stderr).expect("utf-8");
    assert!(
        stderr.contains("exactly one"),
        "stderr missing constraint message: {stderr:?}"
    );
}

#[test]
fn tagged_union_verb_requires_target_flag() {
    // `retrieve` and `forget` carry a discriminator-keyed ArgGroup with
    // `.required(true)`; clap rejects a bare invocation before our dispatch
    // runs → exit 64 (EX_USAGE).
    for verb in ["retrieve", "forget"] {
        let out = cli().arg(verb).output().expect("cairn <verb>");
        assert!(!out.status.success(), "verb {verb} exited OK");
        assert_eq!(out.status.code(), Some(64), "verb {verb} wrong exit code");
        let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
        assert!(
            stderr.contains("required"),
            "verb {verb} stderr missing required-args message: {stderr:?}",
        );
    }
}

#[test]
fn search_accepts_explain_flag() {
    // Clap accepts `--explain` (a SetTrue boolean flag generated from
    // search.json's x-cairn-cli flags). Without the policy_trace
    // capability advertised, the handler fails-closed with sysexit 69
    // (covered by the next test) — but it must not be `UnknownArgument`.
    let out = cli()
        .args(["search", "--mode", "keyword", "--explain", "test"])
        .output()
        .expect("cairn");
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        !stderr.contains("unexpected argument"),
        "search must accept --explain; got: {stderr:?}",
    );
}

#[test]
fn search_explain_is_gated_by_policy_trace_capability() {
    // P0 always advertises cairn.mcp.v1.policy_trace (CairnConfig::capabilities
    // unconditionally sets policy_trace = true; see config tests). Therefore
    // `--explain` must NOT be rejected with CapabilityUnavailable — the gate
    // passes and the request proceeds to verb dispatch.
    // Without a live vault, the search opens an empty store and returns 0 results
    // (exit 0); stderr must not mention CapabilityUnavailable.
    let out = cli()
        .args(["search", "--mode", "keyword", "--explain", "test"])
        .output()
        .expect("cairn");
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        !stderr.contains("CapabilityUnavailable"),
        "search --explain must not be rejected with CapabilityUnavailable \
         because policy_trace is always advertised at P0; got: {stderr:?}",
    );
    // Exit 0 (success, 0 results) or 1 (Internal store error); never 69.
    assert_ne!(
        out.status.code(),
        Some(69),
        "exit 69 (EX_UNAVAILABLE) must not occur when policy_trace is advertised; \
         got status {:?}, stderr {stderr}",
        out.status,
    );
}

#[test]
fn search_explain_json_does_not_emit_capability_unavailable() {
    // P0 always advertises policy_trace, so `--explain --json` must NOT
    // emit a CapabilityUnavailable error envelope. The exit code is 0
    // (success, 0 results) or 1 (Internal if the temp store can't open) —
    // never 69 (EX_UNAVAILABLE).
    let out = cli()
        .args(["search", "--mode", "keyword", "--explain", "test", "--json"])
        .output()
        .expect("cairn");
    assert_ne!(
        out.status.code(),
        Some(69),
        "exit 69 must not occur since policy_trace is always advertised at P0"
    );
    // Verify the stdout (if any) is valid JSON and not a CapabilityUnavailable envelope.
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    if !stdout.trim().is_empty() {
        let v: serde_json::Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("not valid JSON: {e}\nstdout: {stdout:?}"));
        assert_ne!(
            v["error"]["code"].as_str(),
            Some("CapabilityUnavailable"),
            "must not emit CapabilityUnavailable since policy_trace is advertised"
        );
    }
}

#[test]
fn search_help_lists_explain_flag() {
    // The generated help screen must surface --explain so callers can
    // discover it. Regression guard against the IDL/x-cairn-cli drift
    // codex flagged in PR #237.
    let out = cli().args(["search", "--help"]).output().expect("cairn");
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(
        stdout.contains("--explain"),
        "search --help must list --explain flag; got: {stdout}",
    );
}

#[test]
fn unknown_argument_fails_closed() {
    // Clap UnknownArgument → exit 64 (EX_USAGE) per spec §5.2.
    let out = cli()
        .arg("--definitely-not-a-flag")
        .output()
        .expect("cairn");
    assert!(!out.status.success(), "exit: {:?}", out.status);
    assert_eq!(out.status.code(), Some(64));
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("unexpected argument"),
        "stderr missing clap usage marker: {stderr:?}",
    );
}

#[test]
fn screen_capture_help_lists_output_flag() {
    let out = cli()
        .args(["screen", "capture", "--help"])
        .output()
        .expect("cairn screen capture --help");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(
        stdout.contains("--output"),
        "screen capture --help must list --output flag; got: {stdout}",
    );
}

#[test]
fn bootstrap_emits_json_with_flag() {
    let dir = tempfile::tempdir().unwrap();
    let out = cli()
        .args([
            "bootstrap",
            "--vault-path",
            dir.path().to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("cairn bootstrap --json");
    assert!(
        out.status.success(),
        "exit: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("--json must emit valid JSON");
    assert!(
        parsed.get("vault_path").is_some(),
        "JSON missing vault_path"
    );
    assert!(
        parsed.get("dirs_created").is_some(),
        "JSON missing dirs_created"
    );
}

#[test]
fn bootstrap_force_flag_accepted() {
    let dir = tempfile::tempdir().unwrap();
    // first run
    cli()
        .args(["bootstrap", "--vault-path", dir.path().to_str().unwrap()])
        .output()
        .unwrap();
    // second run with --force must succeed
    let out = cli()
        .args([
            "bootstrap",
            "--vault-path",
            dir.path().to_str().unwrap(),
            "--force",
        ])
        .output()
        .expect("cairn bootstrap --force");
    assert!(out.status.success(), "exit: {:?}", out.status);
}

#[test]
fn bootstrap_io_error_exits_74() {
    // Point at a path we cannot write to — use a file as the vault path so
    // create_dir_all fails.
    let file = tempfile::NamedTempFile::new().unwrap();
    let out = cli()
        .args(["bootstrap", "--vault-path", file.path().to_str().unwrap()])
        .output()
        .expect("cairn bootstrap <file-as-vault>");
    assert_eq!(
        out.status.code(),
        Some(74),
        "expected EX_IOERR(74), got: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}
