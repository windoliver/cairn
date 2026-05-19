// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use std::io::Write as _;
use std::process::{Command, Stdio};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn parse_stdout_json(out: std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    serde_json::from_str(stdout.trim()).expect("expected valid JSON on stdout")
}

fn read_json_file(path: &std::path::Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read JSON file {}: {err}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|err| panic!("parse JSON file {}: {err}", path.display()))
}

fn run_hook_with_payload(
    name: &str,
    payload: &str,
    vault: &tempfile::TempDir,
) -> serde_json::Value {
    let out = cli()
        .args([
            "hook",
            name,
            "--vault-path",
            vault.path().to_str().expect("utf-8 path"),
            "--payload",
            payload,
            "--json",
        ])
        .output()
        .unwrap_or_else(|err| panic!("cairn hook {name}: {err}"));
    assert!(out.status.success(), "{name} exit: {:?}", out.status);
    parse_stdout_json(out)
}

fn run_hook_with_payload_file(
    name: &str,
    payload: &str,
    vault: &tempfile::TempDir,
) -> serde_json::Value {
    let payload_dir = vault.path().join("payloads");
    std::fs::create_dir_all(&payload_dir).expect("payload dir");
    let payload_path = payload_dir.join(format!("{name}.json"));
    std::fs::write(&payload_path, payload)
        .unwrap_or_else(|err| panic!("write payload file {}: {err}", payload_path.display()));
    let out = cli()
        .args([
            "hook",
            name,
            "--vault-path",
            vault.path().to_str().expect("utf-8 vault path"),
            "--payload-file",
            payload_path.to_str().expect("utf-8 payload path"),
            "--json",
        ])
        .output()
        .unwrap_or_else(|err| panic!("cairn hook {name} --payload-file: {err}"));
    assert!(out.status.success(), "{name} exit: {:?}", out.status);
    parse_stdout_json(out)
}

fn run_hook_with_stdin_payload(
    name: &str,
    payload: &str,
    vault: &tempfile::TempDir,
) -> serde_json::Value {
    let mut child = cli()
        .args([
            "hook",
            name,
            "--vault-path",
            vault.path().to_str().expect("utf-8 path"),
            "--json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|err| panic!("spawn cairn hook {name}: {err}"));
    child
        .stdin
        .as_mut()
        .expect("stdin pipe")
        .write_all(payload.as_bytes())
        .unwrap_or_else(|err| panic!("write cairn hook {name} stdin: {err}"));
    let out = child
        .wait_with_output()
        .unwrap_or_else(|err| panic!("wait cairn hook {name}: {err}"));
    assert!(out.status.success(), "{name} exit: {:?}", out.status);
    parse_stdout_json(out)
}

fn trace_artifact(vault: &tempfile::TempDir, result: &serde_json::Value) -> serde_json::Value {
    let trace_id = result["artifacts"]["trace_id"].as_str().expect("trace_id");
    read_json_file(
        &vault
            .path()
            .join(".cairn/hooks/traces")
            .join(format!("{trace_id}.json")),
    )
}

fn assert_session_hot_artifact(
    vault: &tempfile::TempDir,
    result: &serde_json::Value,
    session: &str,
) {
    assert_eq!(result["ok"], true);
    let hot_path = result["artifacts"]["hot_path"].as_str().expect("hot path");
    let hot = read_json_file(&vault.path().join(hot_path));
    assert_eq!(hot["operation_id"], result["operation_id"]);
    assert_eq!(hot["session_id"], session);
    assert_eq!(hot["prefix"], "");
}

fn assert_prompt_trace(vault: &tempfile::TempDir, result: &serde_json::Value, session: &str) {
    assert_eq!(result["ok"], true);
    assert_eq!(result["routing_hints"]["capture_prompt"], true);
    assert_eq!(result["routing_hints"]["memory_write_suggested"], true);
    assert_eq!(result["routing_hints"]["forget_suggested"], true);
    assert_eq!(result["routing_hints"]["search_suggested"], true);
    let trace = trace_artifact(vault, result);
    assert_eq!(trace["operation_id"], result["operation_id"]);
    assert_eq!(trace["hook"], "UserPromptSubmit");
    assert_eq!(trace["session_id"], session);
    assert_eq!(
        trace["event"]["prompt"],
        "remember to search before you forget stale notes",
    );
}

fn assert_pre_tool_trace(vault: &tempfile::TempDir, result: &serde_json::Value, session: &str) {
    assert_eq!(result["ok"], true);
    assert!(result.get("routing_hints").is_none());
    let trace = trace_artifact(vault, result);
    assert_eq!(trace["operation_id"], result["operation_id"]);
    assert_eq!(trace["hook"], "PreToolUse");
    assert_eq!(trace["session_id"], session);
    assert_eq!(trace["tool_call_id"], "call-e2e");
    assert_eq!(trace["tool_name"], "shell");
    assert_eq!(trace["event"]["input_preview"], "cargo test");
}

fn assert_post_tool_trace(vault: &tempfile::TempDir, result: &serde_json::Value, session: &str) {
    assert_eq!(result["ok"], true);
    let trace = trace_artifact(vault, result);
    assert_eq!(trace["operation_id"], result["operation_id"]);
    assert_eq!(trace["hook"], "PostToolUse");
    assert_eq!(trace["session_id"], session);
    assert_eq!(trace["tool_call_id"], "call-e2e");
    assert_eq!(trace["tool_name"], "shell");
    assert_eq!(trace["status"], "ok");
    assert_eq!(trace["event"]["exit_code"], 0);
}

fn assert_stop_artifacts(vault: &tempfile::TempDir, result: &serde_json::Value, session: &str) {
    assert_eq!(result["ok"], true);
    let trace = trace_artifact(vault, result);
    assert_eq!(trace["operation_id"], result["operation_id"]);
    assert_eq!(trace["hook"], "Stop");
    assert_eq!(trace["session_id"], session);
    let job_id = result["artifacts"]["queued_jobs"][0]
        .as_str()
        .expect("queued job id");
    let queue = read_json_file(
        &vault
            .path()
            .join(".cairn/hooks/queue")
            .join(format!("{job_id}.json")),
    );
    assert_eq!(queue["operation_id"], result["operation_id"]);
    assert_eq!(queue["job_id"], job_id);
    assert_eq!(queue["session_id"], session);
    assert_eq!(queue["trace_id"], result["artifacts"]["trace_id"]);
    assert_eq!(queue["kind"], "post_turn");
    assert_eq!(queue["status"], "pending");
}

fn assert_lifecycle_artifact_counts(vault: &tempfile::TempDir) {
    assert_eq!(
        std::fs::read_dir(vault.path().join(".cairn/hooks/traces"))
            .expect("trace dir")
            .count(),
        4,
    );
    assert_eq!(
        std::fs::read_dir(vault.path().join(".cairn/hooks/queue"))
            .expect("queue dir")
            .count(),
        1,
    );
}

#[test]
fn top_level_help_lists_hook_subcommand() {
    let out = cli().arg("--help").output().expect("cairn --help");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(
        stdout.contains("hook"),
        "top-level help missing hook: {stdout}"
    );
    assert!(
        stdout.contains("Run a Cairn harness lifecycle hook"),
        "top-level help missing hook description: {stdout}",
    );
}

#[test]
fn hook_help_lists_canonical_five_hooks() {
    let out = cli()
        .args(["hook", "--help"])
        .output()
        .expect("cairn hook --help");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    for hook in [
        "SessionStart",
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "Stop",
    ] {
        assert!(stdout.contains(hook), "hook help missing {hook}: {stdout}");
    }
}

#[test]
fn precompact_is_not_a_canonical_hook() {
    let out = cli()
        .args(["hook", "PreCompact", "--json"])
        .output()
        .expect("cairn hook PreCompact");
    assert_eq!(out.status.code(), Some(64), "exit: {:?}", out.status);
    let v = parse_stdout_json(out);
    assert_eq!(v["ok"], false);
    assert_eq!(v["hook"], "PreCompact");
    assert!(v["operation_id"].as_str().is_some());
    assert_eq!(v["error"]["code"], "InvalidArgs");
    assert!(
        v["error"]["retry_guidance"]
            .as_str()
            .is_some_and(|guidance| guidance.contains("retry")),
        "retry guidance missing retry instruction: {v}",
    );
}

#[test]
fn unknown_hook_name_exits_usage_64() {
    let out = cli()
        .args(["hook", "DefinitelyNotAHook", "--json"])
        .output()
        .expect("cairn hook unknown");
    assert_eq!(out.status.code(), Some(64), "exit: {:?}", out.status);
    let v = parse_stdout_json(out);
    assert_eq!(v["ok"], false);
    assert_eq!(v["hook"], "DefinitelyNotAHook");
    assert!(v["operation_id"].as_str().is_some());
    assert_eq!(v["error"]["code"], "InvalidArgs");
}

#[test]
fn valid_hook_json_emits_success_envelope() {
    let vault = tempfile::tempdir().expect("temp vault");
    let out = cli()
        .args([
            "hook",
            "SessionStart",
            "--vault-path",
            vault.path().to_str().expect("utf-8 path"),
            "--payload",
            r#"{"session_id":"sess-1"}"#,
            "--json",
        ])
        .output()
        .expect("cairn hook SessionStart");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let v = parse_stdout_json(out);
    assert_eq!(v["ok"], true);
    assert_eq!(v["hook"], "SessionStart");
    assert!(v["operation_id"].as_str().is_some());
    assert!(v["artifacts"].is_object());
    assert!(v.get("error").is_none());
}

#[test]
fn non_object_payload_emits_typed_invalid_args_error() {
    let out = cli()
        .args(["hook", "UserPromptSubmit", "--payload", "[]", "--json"])
        .output()
        .expect("cairn hook UserPromptSubmit");
    assert_eq!(out.status.code(), Some(1), "exit: {:?}", out.status);
    let v = parse_stdout_json(out);
    assert_eq!(v["ok"], false);
    assert_eq!(v["hook"], "UserPromptSubmit");
    assert!(v["operation_id"].as_str().is_some());
    assert_eq!(v["error"]["code"], "InvalidArgs");
    assert_eq!(v["error"]["message"], "hook payload must be a JSON object");
    assert!(
        v["error"]["retry_guidance"]
            .as_str()
            .is_some_and(|guidance| guidance.contains("retry")),
        "retry guidance missing retry instruction: {v}",
    );
}

#[test]
fn payload_and_payload_file_conflict_at_cli_boundary() {
    let vault = tempfile::tempdir().expect("temp vault");
    let payload_path = vault.path().join("payload.json");
    std::fs::write(&payload_path, r#"{"session_id":"sess-1"}"#).expect("payload file");
    let out = cli()
        .args([
            "hook",
            "SessionStart",
            "--payload",
            r#"{"session_id":"sess-1"}"#,
            "--payload-file",
            payload_path.to_str().expect("utf-8 payload path"),
            "--json",
        ])
        .output()
        .expect("cairn hook payload conflict");
    assert_eq!(out.status.code(), Some(64), "exit: {:?}", out.status);
}

#[test]
fn user_prompt_submit_writes_trace_artifact() {
    let vault = tempfile::tempdir().expect("temp vault");
    let v = run_hook_with_payload(
        "UserPromptSubmit",
        r#"{"session_id":"sess-1","prompt":"remember this"}"#,
        &vault,
    );
    assert_eq!(v["ok"], true);
    assert_eq!(v["hook"], "UserPromptSubmit");
    let trace_id = v["artifacts"]["trace_id"].as_str().expect("trace_id");
    let trace_path = vault
        .path()
        .join(".cairn/hooks/traces")
        .join(format!("{trace_id}.json"));
    assert!(
        trace_path.exists(),
        "missing trace artifact at {}",
        trace_path.display(),
    );
    let trace = read_json_file(&trace_path);
    assert_eq!(trace["operation_id"], v["operation_id"]);
    assert_eq!(trace["hook"], "UserPromptSubmit");
    assert_eq!(trace["session_id"], "sess-1");
    assert_eq!(trace["event"]["prompt"], "remember this");
    assert_eq!(v["routing_hints"]["capture_prompt"], true);
    assert_eq!(v["routing_hints"]["memory_write_suggested"], true);
}

#[test]
fn claude_code_user_prompt_payload_is_read_from_stdin() {
    let vault = tempfile::tempdir().expect("temp vault");
    let v = run_hook_with_stdin_payload(
        "UserPromptSubmit",
        r#"{"session_id":"sess-cc","transcript_path":"/tmp/claude/transcript.jsonl","cwd":"/tmp/project","permission_mode":"default","hook_event_name":"UserPromptSubmit","prompt":"remember this from stdin"}"#,
        &vault,
    );

    assert_eq!(v["ok"], true);
    assert_eq!(v["hook"], "UserPromptSubmit");
    let trace = trace_artifact(&vault, &v);
    assert_eq!(trace["session_id"], "sess-cc");
    assert_eq!(trace["event"]["hook_event_name"], "UserPromptSubmit");
    assert_eq!(
        trace["event"]["transcript_path"],
        "/tmp/claude/transcript.jsonl"
    );
    assert_eq!(trace["event"]["cwd"], "/tmp/project");
    assert_eq!(trace["event"]["prompt"], "remember this from stdin");
}

#[test]
fn session_start_returns_hot_artifact() {
    let vault = tempfile::tempdir().expect("temp vault");
    let v = run_hook_with_payload("SessionStart", r#"{"session_id":"sess-1"}"#, &vault);
    assert_eq!(v["ok"], true);
    assert_eq!(v["hook"], "SessionStart");
    let hot_path = v["artifacts"]["hot_path"].as_str().expect("hot_path");
    assert!(
        vault.path().join(hot_path).exists(),
        "missing hot artifact {hot_path}",
    );
}

#[test]
fn pre_tool_use_writes_trace_artifact() {
    let vault = tempfile::tempdir().expect("temp vault");
    let v = run_hook_with_payload(
        "PreToolUse",
        r#"{"session_id":"sess-1","tool_call_id":"call-1","tool_name":"shell"}"#,
        &vault,
    );
    assert_eq!(v["ok"], true);
    assert_eq!(v["hook"], "PreToolUse");
    let trace_id = v["artifacts"]["trace_id"].as_str().expect("trace_id");
    assert!(
        vault
            .path()
            .join(".cairn/hooks/traces")
            .join(format!("{trace_id}.json"))
            .exists()
    );
}

#[test]
fn claude_code_tool_hooks_use_tool_use_id_as_parent_link() {
    let vault = tempfile::tempdir().expect("temp vault");
    let pre = run_hook_with_stdin_payload(
        "PreToolUse",
        r#"{"session_id":"sess-cc","transcript_path":"/tmp/claude/transcript.jsonl","cwd":"/tmp/project","permission_mode":"default","hook_event_name":"PreToolUse","tool_name":"Bash","tool_use_id":"toolu_123","tool_input":{"command":"cargo test"}}"#,
        &vault,
    );
    let post = run_hook_with_stdin_payload(
        "PostToolUse",
        r#"{"session_id":"sess-cc","transcript_path":"/tmp/claude/transcript.jsonl","cwd":"/tmp/project","permission_mode":"default","hook_event_name":"PostToolUse","tool_name":"Bash","tool_use_id":"toolu_123","tool_input":{"command":"cargo test"},"tool_response":{"success":true}}"#,
        &vault,
    );

    let pre_trace = trace_artifact(&vault, &pre);
    assert_eq!(pre_trace["hook"], "PreToolUse");
    assert_eq!(pre_trace["tool_call_id"], "toolu_123");
    assert_eq!(pre_trace["tool_name"], "Bash");
    assert_eq!(pre_trace["event"]["tool_use_id"], "toolu_123");
    assert_eq!(pre_trace["event"]["tool_input"]["command"], "cargo test");

    let post_trace = trace_artifact(&vault, &post);
    assert_eq!(post_trace["hook"], "PostToolUse");
    assert_eq!(post_trace["tool_call_id"], "toolu_123");
    assert_eq!(post_trace["tool_name"], "Bash");
    assert_eq!(post_trace["status"], "ok");
    assert_eq!(post_trace["event"]["tool_use_id"], "toolu_123");
    assert_eq!(post_trace["event"]["tool_response"]["success"], true);
}

#[test]
fn post_tool_use_writes_trace_artifact() {
    let vault = tempfile::tempdir().expect("temp vault");
    let v = run_hook_with_payload(
        "PostToolUse",
        r#"{"session_id":"sess-1","tool_call_id":"call-1","tool_name":"shell","status":"ok"}"#,
        &vault,
    );
    assert_eq!(v["ok"], true);
    assert_eq!(v["hook"], "PostToolUse");
    let trace_id = v["artifacts"]["trace_id"].as_str().expect("trace_id");
    assert!(
        vault
            .path()
            .join(".cairn/hooks/traces")
            .join(format!("{trace_id}.json"))
            .exists()
    );
}

#[test]
fn missing_required_trace_field_returns_invalid_args() {
    let out = cli()
        .args([
            "hook",
            "PreToolUse",
            "--payload",
            r#"{"session_id":"sess-1","tool_call_id":"call-1"}"#,
            "--json",
        ])
        .output()
        .expect("cairn hook PreToolUse invalid");
    assert_eq!(out.status.code(), Some(1), "exit: {:?}", out.status);
    let v = parse_stdout_json(out);
    assert_eq!(v["ok"], false);
    assert_eq!(v["hook"], "PreToolUse");
    assert_eq!(v["error"]["code"], "InvalidArgs");
    assert!(
        v["error"]["retry_guidance"]
            .as_str()
            .is_some_and(|guidance| guidance.contains("retry")),
        "retry guidance missing retry instruction: {v}",
    );
}

#[test]
fn stop_writes_trace_and_queue_artifacts() {
    let vault = tempfile::tempdir().expect("temp vault");
    let out = cli()
        .args([
            "hook",
            "Stop",
            "--vault-path",
            vault.path().to_str().expect("utf-8 path"),
            "--payload",
            r#"{"session_id":"sess-1"}"#,
            "--json",
        ])
        .output()
        .expect("cairn hook Stop");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let v = parse_stdout_json(out);
    assert_eq!(v["ok"], true);
    assert_eq!(v["hook"], "Stop");
    let trace_id = v["artifacts"]["trace_id"].as_str().expect("trace_id");
    let job_id = v["artifacts"]["queued_jobs"][0]
        .as_str()
        .expect("queued job id");
    let trace_path = vault
        .path()
        .join(".cairn/hooks/traces")
        .join(format!("{trace_id}.json"));
    let queue_path = vault
        .path()
        .join(".cairn/hooks/queue")
        .join(format!("{job_id}.json"));
    assert!(trace_path.exists());
    assert!(queue_path.exists());
    let trace = read_json_file(&trace_path);
    let queue = read_json_file(&queue_path);
    assert_eq!(trace["operation_id"], v["operation_id"]);
    assert_eq!(trace["hook"], "Stop");
    assert_eq!(trace["session_id"], "sess-1");
    assert_eq!(queue["operation_id"], v["operation_id"]);
    assert_eq!(queue["job_id"], job_id);
    assert_eq!(queue["session_id"], "sess-1");
    assert_eq!(queue["trace_id"], trace_id);
    assert_eq!(queue["kind"], "post_turn");
    assert_eq!(queue["status"], "pending");
}

#[test]
fn stop_queue_failure_returns_retry_guidance() {
    let vault = tempfile::tempdir().expect("temp vault");
    let hooks_dir = vault.path().join(".cairn/hooks");
    std::fs::create_dir_all(&hooks_dir).expect("hooks dir");
    std::fs::write(hooks_dir.join("queue"), b"not a directory").expect("queue blocker");
    let out = cli()
        .args([
            "hook",
            "Stop",
            "--vault-path",
            vault.path().to_str().expect("utf-8 path"),
            "--payload",
            r#"{"session_id":"sess-1"}"#,
            "--json",
        ])
        .output()
        .expect("cairn hook Stop");
    assert_eq!(out.status.code(), Some(1), "exit: {:?}", out.status);
    let v = parse_stdout_json(out);
    assert_eq!(v["ok"], false);
    assert_eq!(v["hook"], "Stop");
    assert_eq!(v["error"]["code"], "Internal");
    assert!(v["operation_id"].is_string());
    assert!(
        v["error"]["retry_guidance"]
            .as_str()
            .unwrap_or("")
            .contains("retry cairn hook Stop")
    );
}

#[test]
fn full_hook_lifecycle_writes_expected_artifacts() {
    let vault = tempfile::tempdir().expect("temp vault");
    let session = r#""sess-1""#;
    let cases = [
        ("SessionStart", format!(r#"{{"session_id":{session}}}"#)),
        (
            "UserPromptSubmit",
            format!(r#"{{"session_id":{session},"prompt":"hello"}}"#),
        ),
        (
            "PreToolUse",
            format!(r#"{{"session_id":{session},"tool_call_id":"call-1","tool_name":"shell"}}"#),
        ),
        (
            "PostToolUse",
            format!(
                r#"{{"session_id":{session},"tool_call_id":"call-1","tool_name":"shell","status":"ok"}}"#
            ),
        ),
        ("Stop", format!(r#"{{"session_id":{session}}}"#)),
    ];
    for (hook, payload) in cases {
        let v = run_hook_with_payload(hook, &payload, &vault);
        assert_eq!(v["ok"], true, "{hook} did not succeed: {v}");
    }
    let trace_dir = vault.path().join(".cairn/hooks/traces");
    let trace_count = std::fs::read_dir(trace_dir).expect("trace dir").count();
    assert_eq!(
        trace_count, 4,
        "prompt, pre-tool, post-tool, and stop traces"
    );
    let queue_count = std::fs::read_dir(vault.path().join(".cairn/hooks/queue"))
        .expect("queue dir")
        .count();
    assert_eq!(queue_count, 1, "Stop enqueues exactly one post-turn job");
}

#[test]
fn full_hook_lifecycle_via_payload_files_round_trips_artifact_contents() {
    let vault = tempfile::tempdir().expect("temp vault");
    let session = "sess-e2e";

    let session_start = run_hook_with_payload_file(
        "SessionStart",
        &format!(r#"{{"session_id":"{session}"}}"#),
        &vault,
    );
    assert_session_hot_artifact(&vault, &session_start, session);

    let prompt = run_hook_with_payload_file(
        "UserPromptSubmit",
        &format!(
            r#"{{"session_id":"{session}","prompt":"remember to search before you forget stale notes"}}"#
        ),
        &vault,
    );
    assert_prompt_trace(&vault, &prompt, session);

    let pre = run_hook_with_payload_file(
        "PreToolUse",
        &format!(
            r#"{{"session_id":"{session}","tool_call_id":"call-e2e","tool_name":"shell","input_preview":"cargo test"}}"#
        ),
        &vault,
    );
    assert_pre_tool_trace(&vault, &pre, session);

    let post = run_hook_with_payload_file(
        "PostToolUse",
        &format!(
            r#"{{"session_id":"{session}","tool_call_id":"call-e2e","tool_name":"shell","status":"ok","exit_code":0}}"#
        ),
        &vault,
    );
    assert_post_tool_trace(&vault, &post, session);

    let stop =
        run_hook_with_payload_file("Stop", &format!(r#"{{"session_id":"{session}"}}"#), &vault);
    assert_stop_artifacts(&vault, &stop, session);
    assert_lifecycle_artifact_counts(&vault);
}

#[test]
fn full_claude_code_stdin_lifecycle_round_trips_artifact_contents() {
    let vault = tempfile::tempdir().expect("temp vault");
    let session = "sess-cc-e2e";
    let transcript = "/tmp/claude/transcript.jsonl";
    let cwd = "/tmp/project";
    let tool_use_id = "toolu_e2e";

    let session_start = run_hook_with_stdin_payload(
        "SessionStart",
        &format!(
            r#"{{"session_id":"{session}","transcript_path":"{transcript}","cwd":"{cwd}","hook_event_name":"SessionStart","source":"startup"}}"#
        ),
        &vault,
    );
    assert_session_hot_artifact(&vault, &session_start, session);

    let prompt = run_hook_with_stdin_payload(
        "UserPromptSubmit",
        &format!(
            r#"{{"session_id":"{session}","transcript_path":"{transcript}","cwd":"{cwd}","permission_mode":"default","hook_event_name":"UserPromptSubmit","prompt":"remember to search before you forget stale notes"}}"#
        ),
        &vault,
    );
    assert_prompt_trace(&vault, &prompt, session);
    let prompt_trace = trace_artifact(&vault, &prompt);
    assert_eq!(prompt_trace["event"]["hook_event_name"], "UserPromptSubmit");
    assert_eq!(prompt_trace["event"]["transcript_path"], transcript);
    assert_eq!(prompt_trace["event"]["cwd"], cwd);

    let pre = run_hook_with_stdin_payload(
        "PreToolUse",
        &format!(
            r#"{{"session_id":"{session}","transcript_path":"{transcript}","cwd":"{cwd}","permission_mode":"default","hook_event_name":"PreToolUse","tool_name":"Bash","tool_use_id":"{tool_use_id}","tool_input":{{"command":"cargo test"}}}}"#
        ),
        &vault,
    );
    let pre_trace = trace_artifact(&vault, &pre);
    assert_eq!(pre_trace["hook"], "PreToolUse");
    assert_eq!(pre_trace["session_id"], session);
    assert_eq!(pre_trace["tool_call_id"], tool_use_id);
    assert_eq!(pre_trace["tool_name"], "Bash");
    assert_eq!(pre_trace["event"]["hook_event_name"], "PreToolUse");
    assert_eq!(pre_trace["event"]["transcript_path"], transcript);
    assert_eq!(pre_trace["event"]["cwd"], cwd);
    assert_eq!(pre_trace["event"]["tool_input"]["command"], "cargo test");

    let post = run_hook_with_stdin_payload(
        "PostToolUse",
        &format!(
            r#"{{"session_id":"{session}","transcript_path":"{transcript}","cwd":"{cwd}","permission_mode":"default","hook_event_name":"PostToolUse","tool_name":"Bash","tool_use_id":"{tool_use_id}","tool_input":{{"command":"cargo test"}},"tool_response":{{"success":false,"stderr":"compile failed"}}}}"#
        ),
        &vault,
    );
    let post_trace = trace_artifact(&vault, &post);
    assert_eq!(post_trace["hook"], "PostToolUse");
    assert_eq!(post_trace["session_id"], session);
    assert_eq!(post_trace["tool_call_id"], tool_use_id);
    assert_eq!(post_trace["tool_name"], "Bash");
    assert_eq!(post_trace["status"], "error");
    assert_eq!(post_trace["event"]["hook_event_name"], "PostToolUse");
    assert_eq!(post_trace["event"]["transcript_path"], transcript);
    assert_eq!(post_trace["event"]["cwd"], cwd);
    assert_eq!(post_trace["event"]["tool_response"]["success"], false);

    let stop = run_hook_with_stdin_payload(
        "Stop",
        &format!(
            r#"{{"session_id":"{session}","transcript_path":"{transcript}","cwd":"{cwd}","hook_event_name":"Stop","stop_hook_active":false}}"#
        ),
        &vault,
    );
    assert_stop_artifacts(&vault, &stop, session);
    let stop_trace = trace_artifact(&vault, &stop);
    assert_eq!(stop_trace["event"]["hook_event_name"], "Stop");
    assert_eq!(stop_trace["event"]["transcript_path"], transcript);
    assert_eq!(stop_trace["event"]["cwd"], cwd);
    assert_eq!(stop_trace["event"]["stop_hook_active"], false);
    assert_lifecycle_artifact_counts(&vault);
}

#[test]
fn stop_returns_after_enqueue_boundary() {
    let vault = tempfile::tempdir().expect("temp vault");
    let started = std::time::Instant::now();
    let v = run_hook_with_payload("Stop", r#"{"session_id":"sess-1"}"#, &vault);
    let elapsed = started.elapsed();
    assert_eq!(v["ok"], true);
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "Stop hook should not wait on downstream workflow execution; elapsed={elapsed:?}",
    );
    assert_eq!(
        v["artifacts"]["queued_jobs"]
            .as_array()
            .expect("queued jobs")
            .len(),
        1,
    );
}

#[test]
fn trace_write_failure_returns_operation_id_and_retry_guidance() {
    let blocked_vault = tempfile::NamedTempFile::new().expect("vault blocker");
    let out = cli()
        .args([
            "hook",
            "UserPromptSubmit",
            "--vault-path",
            blocked_vault.path().to_str().expect("utf-8 path"),
            "--payload",
            r#"{"session_id":"sess-1","prompt":"hello"}"#,
            "--json",
        ])
        .output()
        .expect("cairn hook trace failure");
    assert_eq!(out.status.code(), Some(1), "exit: {:?}", out.status);
    let v = parse_stdout_json(out);
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "Internal");
    assert!(v["operation_id"].is_string());
    assert!(
        v["error"]["retry_guidance"]
            .as_str()
            .unwrap_or("")
            .contains("retry"),
    );
}
