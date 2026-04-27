//! Integration: `cairn mcp --help` exits 0.
#![allow(missing_docs)]

use std::process::Command;

#[test]
fn mcp_help_exits_zero() {
    let bin = env!("CARGO_BIN_EXE_cairn");
    let status = Command::new(bin)
        .args(["mcp", "--help"])
        .status()
        .expect("cairn binary must be runnable");
    assert!(
        status.success(),
        "cairn mcp --help must exit 0; got {status}"
    );
}

#[test]
fn mcp_appears_in_help() {
    let bin = env!("CARGO_BIN_EXE_cairn");
    let output = Command::new(bin)
        .arg("--help")
        .output()
        .expect("cairn binary must be runnable");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("mcp"),
        "cairn --help must mention 'mcp' subcommand; got:\n{stdout}"
    );
}
