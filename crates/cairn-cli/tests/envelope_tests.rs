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
        cmd.output()
            .unwrap_or_else(|e| panic!("failed to run {verb_args:?}: {e}"))
    };
    // Aborted → exit 1 (generic failure)
    assert_eq!(
        out.status.code(),
        Some(1),
        "verb {verb_args:?} should exit 1 (Internal aborted), got {:?}",
        out.status
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("verb {verb_args:?} JSON parse failed: {e}\nstdout: {stdout:?}")
    });
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

#[test]
fn status_advertises_no_capabilities_until_runtime_lands() {
    // P0 advertises no capabilities — verb runtime returns the
    // unimplemented stub, so promising capability support would mislead
    // negotiating clients (#9 / #61 / #62). `cairn.mcp.v1.policy_trace`
    // (#95) and the store-driven search / retrieve / forget mode
    // capabilities are exercised at the type level only until runtime
    // emits traces and honors the modes.
    let out = {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
        cmd.args(["status", "--json"]);
        cmd.output().expect("status --json should run")
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
        "P0 status must advertise an empty capabilities list; got {caps:?}"
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
    };
    let json = serde_json::to_string(&data).expect("serializable");
    assert!(
        json.contains("\"excluded\""),
        "excluded must appear when set; got {json}"
    );
}
