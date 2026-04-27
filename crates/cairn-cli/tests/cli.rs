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
    for verb in [
        "search",
        "summarize",
        "assemble_hot",
        "capture_trace",
        "lint",
    ] {
        let out = cli().arg(verb).output().expect("cairn <verb>");
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
