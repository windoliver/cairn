//! CLI smoke tests for `cairn share` subcommands (brief §12.a).
//!
//! These tests invoke the real `cairn` binary with `share propose`,
//! `share accept`, and `share revoke` and verify exit codes + output.
//! The vault has no identity provisioned, so all three verbs should fail
//! with a config/identity error — the point is to confirm that:
//!
//! 1. Clap parsing works (the subcommand is registered).
//! 2. The dep-wiring path executes without panic/segfault.
//! 3. Appropriate error codes are returned.

use std::path::Path;
use std::process::Command;

fn cairn_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
    cmd.current_dir(std::env::temp_dir());
    cmd.env_remove("CAIRN_VAULT");
    cmd.env_remove("CAIRN_MOCK_EMBEDDER");
    cmd
}

fn write_vault_id(root: &Path) {
    std::fs::create_dir_all(root.join(".cairn")).unwrap();
    std::fs::write(
        root.join(".cairn").join("vault.id"),
        b"01HZZ0000000000000000000AB\n",
    )
    .unwrap();
}

fn write_minimal_config(root: &Path) {
    std::fs::write(
        root.join(".cairn").join("config.yaml"),
        "vault:\n  name: test\n",
    )
    .unwrap();
}

/// `cairn share propose` without a provisioned identity should fail with
/// exit code 1 (generic failure) or 78 (EX_CONFIG). This proves clap arg
/// parsing works and the dep-wiring path executes.
#[test]
fn share_propose_without_identity_exits_with_config_error() {
    let tmp = tempfile::tempdir().unwrap();
    write_vault_id(tmp.path());
    write_minimal_config(tmp.path());

    let out = cairn_bin()
        .args([
            "--vault",
            tmp.path().to_str().unwrap(),
            "share",
            "propose",
            "--record-ids",
            "01HQZX9F5N0000000000000000",
            "--grant-tier",
            "team",
            "--expires-at",
            "2027-01-01T00:00:00Z",
            "--json",
        ])
        .output()
        .expect("spawn cairn");

    // Should fail with a config/identity error (no signing key provisioned).
    // Accept exit code 1 (generic failure) or 78 (EX_CONFIG).
    assert!(
        out.status.code() == Some(1) || out.status.code() == Some(78),
        "expected exit 1 or 78; got {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );

    // Key invariant: no panic.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("panicked at"),
        "process must not panic; stderr: {stderr}"
    );
}

/// `cairn share revoke` with an unknown link id should fail — either with
/// a config error (no identity) or a federation error (unknown link).
#[test]
fn share_revoke_unknown_link_returns_error() {
    let tmp = tempfile::tempdir().unwrap();
    write_vault_id(tmp.path());
    write_minimal_config(tmp.path());

    let out = cairn_bin()
        .args([
            "--vault",
            tmp.path().to_str().unwrap(),
            "share",
            "revoke",
            "--link-id",
            "01HQZX9F5N0000000000000000",
            "--json",
        ])
        .output()
        .expect("spawn cairn");

    // Should fail — either config error (no identity) or federation error
    // (unknown link).
    assert_ne!(
        out.status.code(),
        Some(0),
        "should not succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    // Key invariant: no panic.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("panicked at"),
        "process must not panic; stderr: {stderr}"
    );
}

/// `cairn share accept --envelope -` with invalid JSON piped to stdin
/// should fail with a parse error (exit code != 0).
#[test]
fn share_accept_from_stdin_with_invalid_json_exits_with_error() {
    let tmp = tempfile::tempdir().unwrap();
    write_vault_id(tmp.path());
    write_minimal_config(tmp.path());

    let mut child = cairn_bin()
        .args([
            "--vault",
            tmp.path().to_str().unwrap(),
            "share",
            "accept",
            "--envelope",
            "-",
            "--json",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn cairn");

    // Feed invalid JSON to stdin.
    {
        let stdin = child.stdin.as_mut().unwrap();
        use std::io::Write;
        stdin.write_all(b"not valid json\n").unwrap();
    }

    let out = child.wait_with_output().expect("wait");
    assert_ne!(
        out.status.code(),
        Some(0),
        "invalid envelope should fail\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    // Key invariant: no panic.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("panicked at"),
        "process must not panic; stderr: {stderr}"
    );
}

/// `cairn share --help` must list all three subcommands: propose, accept,
/// and revoke.
#[test]
fn share_help_lists_propose_accept_revoke() {
    let out = cairn_bin()
        .args(["share", "--help"])
        .output()
        .expect("spawn cairn");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("propose"),
        "help must mention propose: {stdout}"
    );
    assert!(
        stdout.contains("accept"),
        "help must mention accept: {stdout}"
    );
    assert!(
        stdout.contains("revoke"),
        "help must mention revoke: {stdout}"
    );

    // Key invariant: no panic.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("panicked at"),
        "process must not panic; stderr: {stderr}"
    );
}
