//! Tests for §5.8 pipeable CLI modes — stdin pipe for `ingest`.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn bootstrap_vault(vault: &Path) {
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: vault.to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
}

#[test]
fn ingest_reads_body_from_stdin_when_source_is_dash() {
    let vault = tempfile::tempdir().expect("temp vault");
    bootstrap_vault(vault.path());
    let mut child = cli()
        .current_dir(vault.path())
        .args(["ingest", "--kind", "user", "-", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cairn ingest -");

    child
        .stdin
        .take()
        .expect("stdin pipe")
        .write_all(b"hello from stdin")
        .expect("write to stdin");

    let out = child.wait_with_output().expect("wait");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdin ingest should commit; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .expect("expected valid JSON envelope for committed ingest");
    assert_eq!(v["contract"], "cairn.mcp.v1");
    assert_eq!(v["status"], "committed");
    let conn = rusqlite::Connection::open(vault.path().join(".cairn/cairn.db")).expect("open db");
    let body: String = conn
        .query_row(
            "SELECT body FROM records WHERE active = 1 AND tombstoned = 0",
            [],
            |row| row.get(0),
        )
        .expect("stored body");
    assert_eq!(body, "hello from stdin");
}
