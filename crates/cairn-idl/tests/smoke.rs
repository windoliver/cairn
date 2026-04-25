// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use std::process::Command;

#[test]
fn crate_name_matches() {
    assert_eq!(env!("CARGO_PKG_NAME"), "cairn-idl");
}

#[test]
fn codegen_binary_help_exits_zero() {
    // The binary must have a deterministic, well-formed CLI.
    // `--help` should print usage and exit 0.
    let bin = env!("CARGO_BIN_EXE_cairn-codegen");
    let out = Command::new(bin).arg("--help").output().expect("cairn-codegen --help");
    assert!(
        out.status.success(),
        "cairn-codegen --help should exit 0, got {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(
        stdout.contains("cairn-codegen"),
        "--help output should contain binary name: {stdout:?}",
    );
}

#[test]
fn schema_dir_constant_points_at_crate_schema_dir() {
    let dir = std::path::Path::new(cairn_idl::SCHEMA_DIR);
    let expected = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("schema");
    assert_eq!(
        dir, expected,
        "SCHEMA_DIR should resolve to <crate>/schema, got {dir:?}"
    );
    assert!(dir.is_dir(), "SCHEMA_DIR must exist on disk: {dir:?}");
}
