//! CLI smoke tests: verify that the `cairn-bench` binary exposes the expected subcommands.

use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn-bench"))
}

#[test]
fn scorecard_subcommand_has_help() {
    let output = cli().args(["scorecard", "--help"]).output().expect("run");
    assert!(
        output.status.success(),
        "scorecard --help failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--fixture"),
        "expected --fixture flag in help"
    );
}

#[test]
fn top_level_help_lists_subcommands() {
    let output = cli().args(["--help"]).output().expect("run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    for sub in ["scorecard", "latency", "memory", "privacy", "all"] {
        assert!(
            stdout.contains(sub),
            "expected `{sub}` in --help output"
        );
    }
}
