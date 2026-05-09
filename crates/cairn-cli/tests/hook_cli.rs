// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
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
