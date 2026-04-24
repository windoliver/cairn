// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use std::process::Command;

#[test]
fn crate_name_matches() {
    assert_eq!(env!("CARGO_PKG_NAME"), "cairn-idl");
}

#[test]
fn codegen_binary_fails_closed() {
    // The codegen scaffold must NOT report success — any caller shelling out
    // to it could otherwise treat missing schema generation as complete.
    let bin = env!("CARGO_BIN_EXE_cairn-codegen");
    let out = Command::new(bin).output().expect("cairn-codegen");
    assert!(!out.status.success(), "cairn-codegen exited OK — should fail closed");
    assert_eq!(out.status.code(), Some(2), "wrong exit code");
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("not yet implemented"),
        "stderr missing not-implemented marker: {stderr:?}",
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(stdout.is_empty(), "scaffold must not print to stdout: {stdout:?}");
}
