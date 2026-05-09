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
    let out = cli()
        .args([
            "hook",
            "SessionStart",
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
    assert!(v.get("artifacts").is_none());
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
