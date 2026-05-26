//! Snapshot the `cairn serve --help` output to lock the CLI shape
//! (CLAUDE.md §6.4 — snapshot tests with insta for CLI surface).

use std::process::Command;

#[test]
fn serve_help_snapshot() {
    let bin = env!("CARGO_BIN_EXE_cairn");
    let output = Command::new(bin)
        .args(["serve", "--help"])
        .output()
        .expect("spawn cairn");
    assert!(
        output.status.success(),
        "cairn serve --help exited with {:?}, stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    insta::assert_snapshot!("serve_help", stdout);
}
