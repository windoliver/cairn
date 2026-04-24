//! End-to-end CLI smoke tests. Invokes the built `cairn` binary and asserts
//! the P0 stub behaviour: help/version succeed, verbs fail closed.

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
    assert!(stdout.contains(env!("CARGO_PKG_VERSION")), "got: {stdout:?}");
}

#[test]
fn default_prints_help_listing_all_eight_verbs() {
    let out = cli().output().expect("cairn");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    for verb in [
        "ingest", "search", "retrieve", "summarize",
        "assemble_hot", "capture_trace", "lint", "forget",
    ] {
        assert!(
            stdout.contains(verb),
            "help output missing verb {verb}, got:\n{stdout}",
        );
    }
}

#[test]
fn help_flag_matches_default() {
    let out = cli().arg("--help").output().expect("cairn --help");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(stdout.contains("ingest"), "got:\n{stdout}");
}

#[test]
fn known_verb_fails_closed() {
    for verb in [
        "ingest", "search", "retrieve", "summarize",
        "assemble_hot", "capture_trace", "lint", "forget",
    ] {
        let out = cli().arg(verb).output().expect("cairn <verb>");
        assert!(!out.status.success(), "verb {verb} exited OK — should fail closed");
        assert_eq!(out.status.code(), Some(2), "verb {verb} wrong exit code");
        let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
        assert!(
            stderr.contains("not yet implemented"),
            "verb {verb} stderr missing not-implemented marker: {stderr:?}",
        );
        let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
        assert!(
            stdout.is_empty(),
            "verb {verb} printed to stdout (caller might swallow stderr): {stdout:?}",
        );
    }
}

#[test]
fn unknown_argument_fails_closed() {
    let out = cli().arg("--definitely-not-a-flag").output().expect("cairn");
    assert!(!out.status.success(), "exit: {:?}", out.status);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(stderr.contains("unrecognised argv"), "got: {stderr:?}");
}

#[test]
fn trailing_arg_after_help_or_version_fails_closed() {
    // Regression: argv validation must cover the WHOLE vector, not just
    // argv[1]. `cairn --version --bad` used to exit 0.
    for (a, b) in [
        ("--version", "--definitely-not-a-flag"),
        ("-V", "--definitely-not-a-flag"),
        ("--help", "--definitely-not-a-flag"),
        ("-h", "ingest"),
        ("-V", "ingest"),
    ] {
        let out = cli().arg(a).arg(b).output().expect("cairn");
        assert!(
            !out.status.success(),
            "`cairn {a} {b}` must fail closed, got exit: {:?}",
            out.status,
        );
        assert_eq!(out.status.code(), Some(2), "`cairn {a} {b}` wrong exit");
        let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
        assert!(
            stderr.contains("unrecognised argv"),
            "`cairn {a} {b}` missing unrecognised marker: {stderr:?}",
        );
    }
}

#[test]
fn trailing_arg_after_verb_still_fails_closed() {
    // Extra tokens after a verb must not flip the scaffold into a success
    // path. The verb itself is still a not-implemented error.
    let out = cli().args(["ingest", "payload"]).output().expect("cairn");
    assert!(!out.status.success(), "exit: {:?}", out.status);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("not yet implemented"),
        "got: {stderr:?}",
    );
    assert!(
        stderr.contains("trailing"),
        "trailing-args hint should mention the extra tokens: {stderr:?}",
    );
}
