//! End-to-end CLI smoke tests. Invokes the built `cairn` binary and asserts
//! the P0 stub output.

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
