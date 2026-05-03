//! End-to-end CLI smoke tests. Invokes the built `cairn` binary and asserts
//! the P0 stub behaviour: help succeeds, verbs fail closed with `Internal`.
//!
//! The CLI tree is generated from the IDL by `cairn-codegen`; the store is
//! not wired yet (lands in #9), so every verb returns an `aborted` envelope
//! with `code: "Internal"`. Exit-code contract (spec §5.2):
//! - simple verb stubs (`ingest`, `search`, …) → exit 1, stderr contains
//!   `Internal`, or `--json` → stdout contains `"status":"aborted"`.
//! - clap usage errors (unknown flag, unknown subcommand, missing required
//!   `ArgGroup`, bare invocation with `subcommand_required`) → 64
//!   (`EX_USAGE`).
//! - bundled `plugin.toml` parse failure → 78 (`EX_CONFIG`); registry
//!   rejection → 69 (`EX_UNAVAILABLE`).

use std::process::Command;

/// Path to the built CLI binary. Cargo sets `CARGO_BIN_EXE_<name>` for every
/// binary in the current crate at test-compile time.
fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
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
fn help_flag_lists_all_eight_verbs() {
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
    ] {
        assert!(
            stdout.contains(verb),
            "help output missing verb {verb}, got:\n{stdout}",
        );
    }
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
fn simple_verb_human_mode_exits_one_with_internal() {
    // After dispatch wiring: verbs with no store adapter exit 1 (generic failure)
    // and print "Internal" to stderr in human mode.
    // `ingest` is excluded: bare `cairn ingest` has no source → exit 64 (usage error).
    // `retrieve` and `forget` are excluded: required ArgGroup → exit 64 (usage error).
    // `search` is excluded: the dispatcher now rejects an empty query with
    //   `InvalidArgs` → exit 64 (EX_USAGE); see `search_empty_query_exits_64` below.
    for args in [
        &["summarize", "01ARYZ6S41TSV4RRFFQ69G5FAV"][..],
        &["assemble_hot"],
        &["capture_trace"],
        &["lint"],
    ] {
        let verb = args[0];
        let out = cli().args(args).output().expect("cairn <verb>");
        assert!(
            !out.status.success(),
            "verb {verb} exited OK — should fail with Internal"
        );
        assert_eq!(
            out.status.code(),
            Some(1),
            "verb {verb} wrong exit code (want 1)"
        );
        let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
        assert!(
            stderr.contains("Internal"),
            "verb {verb} stderr missing Internal error code: {stderr:?}",
        );
    }
}

#[test]
fn search_empty_query_exits_64() {
    // `cairn search` with no query (empty string default) is now wired to the
    // dispatcher, which rejects an empty query with `InvalidArgs` → EX_USAGE (64).
    // This replaced the old stub behaviour (exit 1 + "Internal").
    let out = cli().arg("search").output().expect("cairn search");
    assert_eq!(
        out.status.code(),
        Some(64),
        "search with empty query must exit 64 (EX_USAGE / InvalidArgs); got {:?}",
        out.status
    );
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("InvalidArgs") || stderr.contains("invalid args"),
        "stderr must surface InvalidArgs: {stderr:?}",
    );
}

#[test]
fn simple_verb_json_mode_emits_aborted_internal_envelope() {
    let out = cli()
        .args(["ingest", "--kind", "user", "--body", "hi", "--json"])
        .output()
        .expect("cairn ingest --json");
    assert_eq!(out.status.code(), Some(1), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("expected valid JSON on stdout");
    assert_eq!(v["contract"], "cairn.mcp.v1");
    assert_eq!(v["status"], "aborted");
    assert_eq!(v["error"]["code"], "Internal");
}

#[test]
fn ingest_with_no_source_exits_64() {
    // Bare `cairn ingest` (no body/file/url/source) must fail with usage error, not Internal.
    let out = cli().arg("ingest").output().expect("cairn ingest");
    assert_eq!(out.status.code(), Some(64), "exit: {:?}", out.status);
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
        .args(["search", "--explain", "test"])
        .output()
        .expect("cairn");
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        !stderr.contains("unexpected argument"),
        "search must accept --explain; got: {stderr:?}",
    );
}

#[test]
fn search_explain_fails_closed_with_capability_unavailable() {
    // P0 does not advertise cairn.mcp.v1.policy_trace, so `--explain`
    // must fail-closed with EX_UNAVAILABLE (69) and a
    // CapabilityUnavailable error envelope before any verb dispatch.
    // Mirrors the SDK fail-closed path; together they enforce the
    // x-cairn-capability-when-true annotation in search.json
    // (PR #237 round 4).
    let out = cli()
        .args(["search", "--explain", "test"])
        .output()
        .expect("cairn");
    assert_eq!(
        out.status.code(),
        Some(69),
        "search --explain without policy_trace must exit 69 (EX_UNAVAILABLE); got status {:?}, stderr {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("CapabilityUnavailable"),
        "stderr must surface CapabilityUnavailable: {stderr:?}",
    );
    assert!(
        stderr.contains("cairn.mcp.v1.policy_trace"),
        "stderr must name the missing capability: {stderr:?}",
    );
}

#[test]
fn search_explain_json_emits_capability_unavailable_envelope() {
    // The JSON form of the same fail-closed path: error.code is
    // CapabilityUnavailable and error.data.capability is
    // cairn.mcp.v1.policy_trace.
    let out = cli()
        .args(["search", "--explain", "test", "--json"])
        .output()
        .expect("cairn");
    assert_eq!(out.status.code(), Some(69));
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("not valid JSON: {e}\nstdout: {stdout:?}"));
    assert_eq!(v["status"].as_str(), Some("aborted"));
    assert_eq!(v["error"]["code"].as_str(), Some("CapabilityUnavailable"));
    assert_eq!(
        v["error"]["data"]["capability"].as_str(),
        Some("cairn.mcp.v1.policy_trace")
    );
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
