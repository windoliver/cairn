//! End-to-end coverage for `cairn ingest --jsonl <path>` (issue #311).

use std::io::Write as _;
use std::path::PathBuf;
use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("fixtures/transcripts/claude-code-mixed.jsonl")
}

fn fresh_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    cairn_cli::vault::bootstrap(&cairn_cli::vault::BootstrapOpts {
        vault_path: dir.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap vault");
    dir
}

#[test]
fn ingest_help_lists_transcript_flags() {
    let out = cli()
        .args(["ingest", "--help"])
        .output()
        .expect("cairn ingest --help");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    for needle in [
        "--jsonl",
        "--harness",
        "--session-id-from",
        "--recursive",
        "--limit",
    ] {
        assert!(
            stdout.contains(needle),
            "missing {needle} in --help output:\n{stdout}"
        );
    }
}

#[test]
fn ingest_jsonl_dry_run_reports_sessions_without_writes() {
    let vault = fresh_vault();
    let transcript = fixture();
    assert!(
        transcript.exists(),
        "fixture missing: {}",
        transcript.display()
    );

    let out = cli()
        .current_dir(vault.path())
        .args([
            "ingest",
            "--kind",
            "trace",
            "--jsonl",
            transcript.to_str().expect("utf-8 path"),
            "--harness",
            "claude-code",
            "--dry-run",
            "--json",
        ])
        .output()
        .expect("cairn ingest --jsonl --dry-run");
    assert!(
        out.status.success(),
        "exit: {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let envelope: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("single-document envelope");
    let summary = &envelope["data"]["jsonl_summary"];
    assert_eq!(summary["sessions_parsed"], 1, "envelope: {envelope}");
    assert_eq!(summary["records_written"], 0, "dry-run must not write");
    assert_eq!(summary["files_scanned"], 1);
    assert_eq!(summary["turns_parsed"], 10);

    // Vault DB must not have been touched.
    let db = vault.path().join(".cairn").join("cairn.db");
    assert!(!db.exists(), "dry-run must not create cairn.db");
}

#[test]
fn ingest_jsonl_second_run_is_idempotent() {
    let vault = fresh_vault();
    let transcript = fixture();

    let run = || {
        cli()
            .current_dir(vault.path())
            .args([
                "ingest",
                "--kind",
                "trace",
                "--jsonl",
                transcript.to_str().expect("utf-8 path"),
                "--harness",
                "claude-code",
                "--json",
            ])
            .output()
            .expect("cairn ingest --jsonl")
    };

    let first = run();
    assert!(
        first.status.success(),
        "first run failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_envelope: serde_json::Value =
        serde_json::from_str(String::from_utf8(first.stdout).expect("utf-8").trim()).expect("json");
    let first_summary = &first_envelope["data"]["jsonl_summary"];
    // 2 records per session-with-reasoning: scrubbed session + reasoning
    // addendum (the latter is hidden by the search reasoning filter).
    assert_eq!(first_summary["records_written"], 2);
    assert_eq!(first_summary["sessions_skipped"], 0);

    let second = run();
    assert!(
        second.status.success(),
        "second run failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second_envelope: serde_json::Value =
        serde_json::from_str(String::from_utf8(second.stdout).expect("utf-8").trim())
            .expect("json");
    let second_summary = &second_envelope["data"]["jsonl_summary"];
    assert_eq!(second_summary["records_written"], 0, "idempotency");
    assert_eq!(second_summary["sessions_skipped"], 1);
}

#[test]
fn ingest_jsonl_malformed_row_reports_parser_and_line() {
    let vault = fresh_vault();
    let bad = vault.path().join("bad.jsonl");
    let mut f = std::fs::File::create(&bad).expect("create bad.jsonl");
    f.write_all(
        br#"{"sessionId":"s1","message":{"role":"user","content":[{"type":"text","text":"a"}]}}
"#,
    )
    .expect("line 1");
    f.write_all(
        br#"{"sessionId":"s1","message":{"role":"assistant","content":[{"type":"text","text":"b"}]}}
"#,
    )
    .expect("line 2");
    writeln!(f, "{{not valid json").expect("line 3");
    drop(f);

    let out = cli()
        .current_dir(vault.path())
        .args([
            "ingest",
            "--kind",
            "trace",
            "--jsonl",
            bad.to_str().expect("utf-8"),
            "--harness",
            "claude-code",
        ])
        .output()
        .expect("cairn ingest --jsonl bad");
    assert!(
        !out.status.success(),
        "malformed input should fail; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("claude-code"),
        "stderr must name parser: {stderr}"
    );
    assert!(
        stderr.contains("line 3"),
        "stderr must surface line number: {stderr}"
    );
}
