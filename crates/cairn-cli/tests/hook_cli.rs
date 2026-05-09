// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use std::process::Command;

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
}

#[test]
fn unknown_hook_name_exits_usage_64() {
    let out = cli()
        .args(["hook", "DefinitelyNotAHook", "--json"])
        .output()
        .expect("cairn hook unknown");
    assert_eq!(out.status.code(), Some(64), "exit: {:?}", out.status);
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
fn stop_returns_after_enqueue_boundary() {
    let vault = tempfile::tempdir().expect("temp vault");
    let started = std::time::Instant::now();
    let v = run_hook_with_payload("Stop", r#"{"session_id":"sess-1"}"#, &vault);
    assert_eq!(v["ok"], true);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "Stop hook should not wait on downstream workflow execution",
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
