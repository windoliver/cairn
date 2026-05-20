//! Issue #61 CLI ingest persistence coverage.

use std::path::Path;
use std::process::Command;

use rusqlite::Connection;

#[test]
fn issue_61_ingest_writes_wal_and_record_in_one_db() {
    let vault = tempfile::tempdir().expect("vault");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap");
    let out = Command::new(cargo_bin())
        .current_dir(workspace_root())
        .args([
            "run",
            "-q",
            "-p",
            "cairn-cli",
            "--bin",
            "cairn",
            "--",
            "ingest",
            "--kind",
            "reference",
            "--body",
            "hello",
            "--json",
        ])
        .env("CAIRN_VAULT", vault.path())
        .output()
        .expect("run ingest");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let conn = Connection::open(vault.path().join(".cairn/cairn.db")).expect("open db");
    let records: i64 = conn
        .query_row("SELECT COUNT(*) FROM records", [], |r| r.get(0))
        .expect("records count");
    let wal: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wal_ops WHERE kind = 'upsert'",
            [],
            |r| r.get(0),
        )
        .expect("wal count");
    let committed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM wal_ops WHERE kind = 'upsert' AND state = 'COMMITTED'",
            [],
            |r| r.get(0),
        )
        .expect("committed wal count");
    let used_challenge: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM used WHERE sequence IS NULL AND challenge IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .expect("used challenge count");
    let outstanding: i64 = conn
        .query_row("SELECT COUNT(*) FROM outstanding_challenges", [], |r| {
            r.get(0)
        })
        .expect("outstanding challenge count");
    assert_eq!(records, 1);
    assert_eq!(wal, 1);
    assert_eq!(committed, 1);
    assert_eq!(used_challenge, 1);
    assert_eq!(outstanding, 0);
}

fn cargo_bin() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned())
}

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
}
