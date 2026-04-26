//! Integration: shell out to the built `cairn` binary and assert the
//! `plugins verify --json` output. This is the CI-protective wrapper:
//! it runs under `cargo nextest` regardless of any workflow-yaml drift.

use std::process::Command;

fn cairn_binary() -> std::path::PathBuf {
    // Cargo sets CARGO_BIN_EXE_<name> for every binary in the package.
    let raw = env!("CARGO_BIN_EXE_cairn");
    std::path::PathBuf::from(raw)
}

#[test]
fn plugins_verify_json_default_succeeds() {
    let output = Command::new(cairn_binary())
        .args(["plugins", "verify", "--json"])
        .output()
        .expect("spawn cairn binary");

    assert!(
        output.status.success(),
        "cairn plugins verify --json must exit 0 in default mode; got {:?}",
        output.status
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    assert_eq!(v["summary"]["failed"], 0, "no tier-1 failures expected");
    assert_eq!(
        v["plugins"].as_array().expect("plugins array").len(),
        4,
        "all four bundled plugins must be reported"
    );
}

#[test]
fn plugins_verify_strict_exits_69_with_pendings() {
    let output = Command::new(cairn_binary())
        .args(["plugins", "verify", "--strict"])
        .output()
        .expect("spawn cairn binary");

    let code = output.status.code().expect("exit code present");
    assert_eq!(
        code, 69,
        "verify --strict must exit 69 while tier-2 cases are pending"
    );
}

#[test]
fn plugins_list_emits_alphabetical_rows() {
    let output = Command::new(cairn_binary())
        .args(["plugins", "list"])
        .output()
        .expect("spawn cairn binary");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");

    let mcp_idx = stdout.find("cairn-mcp").expect("mcp present");
    let sensors_idx = stdout.find("cairn-sensors-local").expect("sensors present");
    let store_idx = stdout.find("cairn-store-sqlite").expect("store present");
    let workflows_idx = stdout.find("cairn-workflows").expect("workflows present");

    assert!(mcp_idx < sensors_idx);
    assert!(sensors_idx < store_idx);
    assert!(store_idx < workflows_idx);
}
