//! Verify that every verb returns a valid cairn.mcp.v1 JSON envelope.
//! These tests invoke the compiled binary and will pass after Task 7 wires dispatch.

use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn assert_aborted_internal(verb_args: &[&str]) {
    let out = {
        let mut cmd = cli();
        cmd.args(verb_args);
        cmd.output().unwrap_or_else(|e| panic!("failed to run {verb_args:?}: {e}"))
    };
    // Aborted → exit 1 (generic failure)
    assert_eq!(
        out.status.code(),
        Some(1),
        "verb {verb_args:?} should exit 1 (Internal aborted), got {:?}",
        out.status
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("verb {verb_args:?} JSON parse failed: {e}\nstdout: {stdout:?}"));
    assert_eq!(v["contract"], "cairn.mcp.v1", "verb {verb_args:?}");
    assert_eq!(v["status"], "aborted", "verb {verb_args:?}");
    assert_eq!(v["error"]["code"], "Internal", "verb {verb_args:?}");
    assert!(v["operation_id"].is_string(), "verb {verb_args:?}");
    assert!(v["policy_trace"].is_array(), "verb {verb_args:?}");
}

#[test]
fn ingest_returns_aborted_internal() {
    assert_aborted_internal(&["ingest", "--kind", "user", "--body", "hello", "--json"]);
}

#[test]
fn search_returns_aborted_internal() {
    assert_aborted_internal(&["search", "test query", "--json"]);
}

#[test]
fn retrieve_record_returns_aborted_internal() {
    assert_aborted_internal(&["retrieve", "01JXXXXXXXXXXXXXXXXXXXXXXX", "--json"]);
}

#[test]
fn summarize_returns_aborted_internal() {
    assert_aborted_internal(&["summarize", "01JXXXXXXXXXXXXXXXXXXXXXXX", "--json"]);
}

#[test]
fn assemble_hot_returns_aborted_internal() {
    assert_aborted_internal(&["assemble_hot", "--json"]);
}

#[test]
fn capture_trace_returns_aborted_internal() {
    assert_aborted_internal(&["capture_trace", "--from", "/dev/null", "--json"]);
}

#[test]
fn lint_returns_aborted_internal() {
    assert_aborted_internal(&["lint", "--json"]);
}

#[test]
fn forget_record_returns_aborted_internal() {
    assert_aborted_internal(&["forget", "--record", "01JXXXXXXXXXXXXXXXXXXXXXXX", "--json"]);
}
