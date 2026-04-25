//! End-to-end CLI smoke tests. Invokes the built `cairn` binary and asserts
//! the P0 stub behaviour: help succeeds, verbs without dispatch fail closed.
//!
//! The CLI tree itself is generated from the IDL by `cairn-codegen`; verb
//! dispatch lands in #59 / #9. Until then, simple verbs exit 2 with a
//! not-implemented message and tagged-union verbs (retrieve / forget) exit 2
//! at clap argument parsing because their target groups are required.

use std::process::Command;

/// Path to the built CLI binary. Cargo sets `CARGO_BIN_EXE_<name>` for every
/// binary in the current crate at test-compile time.
fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
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
    // arg_required_else_help(true), so a bare `cairn` invocation is treated
    // as a clap usage error: help to stderr, exit 2.
    let out = cli().output().expect("cairn");
    assert!(!out.status.success(), "bare cairn exited OK");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("ingest"),
        "help text missing verb listing: {stderr:?}",
    );
}

#[test]
fn simple_verb_fails_closed_with_not_implemented_marker() {
    // Verbs whose Args are not a tagged union accept a bare invocation and
    // reach the scaffold dispatch in main.rs, which exits 2 with a marker.
    for verb in [
        "ingest",
        "search",
        "summarize",
        "assemble_hot",
        "capture_trace",
        "lint",
    ] {
        let out = cli().arg(verb).output().expect("cairn <verb>");
        assert!(
            !out.status.success(),
            "verb {verb} exited OK — should fail closed"
        );
        assert_eq!(out.status.code(), Some(2), "verb {verb} wrong exit code");
        let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
        assert!(
            stderr.contains("not yet implemented"),
            "verb {verb} stderr missing not-implemented marker: {stderr:?}",
        );
    }
}

#[test]
fn tagged_union_verb_requires_target_flag() {
    // `retrieve` and `forget` carry a discriminator-keyed ArgGroup with
    // `.required(true)`; clap rejects a bare invocation before our dispatch
    // runs.
    for verb in ["retrieve", "forget"] {
        let out = cli().arg(verb).output().expect("cairn <verb>");
        assert!(!out.status.success(), "verb {verb} exited OK");
        assert_eq!(out.status.code(), Some(2), "verb {verb} wrong exit code");
        let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
        assert!(
            stderr.contains("required"),
            "verb {verb} stderr missing required-args message: {stderr:?}",
        );
    }
}

#[test]
fn unknown_argument_fails_closed() {
    let out = cli()
        .arg("--definitely-not-a-flag")
        .output()
        .expect("cairn");
    assert!(!out.status.success(), "exit: {:?}", out.status);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("unexpected argument"),
        "stderr missing clap usage marker: {stderr:?}",
    );
}
