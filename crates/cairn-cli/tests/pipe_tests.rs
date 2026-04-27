//! Tests for §5.8 pipeable CLI modes — stdin pipe for `ingest`.

use std::io::Write;
use std::process::{Command, Stdio};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

#[test]
fn ingest_reads_body_from_stdin_when_source_is_dash() {
    let mut child = cli()
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
    // The verb returns Internal (no store wired) but must NOT crash or exit 64
    // (which would indicate a clap parse failure — stdin was not read).
    assert_ne!(
        out.status.code(),
        Some(64),
        "exit 64 means clap rejected args before stdin was read"
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    // Even in the error path, the envelope must be valid JSON in --json mode.
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .expect("expected valid JSON envelope even on Internal error");
    assert_eq!(v["contract"], "cairn.mcp.v1");
}
