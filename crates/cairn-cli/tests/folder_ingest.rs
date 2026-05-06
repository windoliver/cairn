//! End-to-end CLI tests for `cairn ingest --folder`.

use std::path::Path;
use std::process::Command;

use serde_json::Value;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().expect("fixture path has parent")).unwrap();
    std::fs::write(path, body).unwrap();
}

fn json_stdout(out: &std::process::Output) -> Value {
    let stdout = String::from_utf8(out.stdout.clone()).expect("utf-8 stdout");
    serde_json::from_str(stdout.trim()).expect("stdout is valid json")
}

#[test]
fn folder_ingest_keyword_dry_run_json_succeeds_without_writes() {
    let dir = tempfile::tempdir().unwrap();
    write(
        &dir.path().join("docs/guide.md"),
        "# Cairn Guide\nSee [[Memory Store]].\n",
    );

    let out = cli()
        .current_dir(dir.path())
        .args([
            "ingest",
            "--folder",
            "docs",
            "--mode",
            "keyword",
            "--dry-run",
            "--json",
        ])
        .output()
        .expect("cairn ingest --folder");

    assert_eq!(out.status.code(), Some(0), "exit: {:?}", out.status);
    assert!(!dir.path().join(".cairn/cache").exists());
    let v = json_stdout(&out);
    for field in [
        "cached",
        "processed",
        "entities_new",
        "entities_merged",
        "edges_new",
        "contradictions_resolved",
        "elapsed_ms",
    ] {
        assert!(v.get(field).is_some(), "missing field {field}: {v}");
    }
    assert_eq!(v["processed"], 1);
    assert!(v["entities_new"].as_u64().unwrap() > 0);
}

#[test]
fn folder_conflicts_with_body_file_url_and_source() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("docs")).unwrap();

    for args in [
        vec!["ingest", "--folder", "docs", "--body", "hi"],
        vec!["ingest", "--folder", "docs", "--file", "a.md"],
        vec!["ingest", "--folder", "docs", "--url", "https://example.com"],
        vec!["ingest", "--folder", "docs", "a.md"],
    ] {
        let out = cli()
            .current_dir(dir.path())
            .args(args)
            .output()
            .expect("cairn ingest conflicting sources");

        assert_eq!(out.status.code(), Some(64), "exit: {:?}", out.status);
    }
}

#[test]
fn missing_folder_exits_usage() {
    let dir = tempfile::tempdir().unwrap();

    let out = cli()
        .current_dir(dir.path())
        .args(["ingest", "--folder", "missing"])
        .output()
        .expect("cairn ingest missing folder");

    assert_eq!(out.status.code(), Some(64), "exit: {:?}", out.status);
}

#[test]
fn semantic_and_full_modes_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("docs")).unwrap();

    for mode in ["semantic", "full"] {
        let out = cli()
            .current_dir(dir.path())
            .args(["ingest", "--folder", "docs", "--mode", mode, "--json"])
            .output()
            .expect("cairn ingest unavailable mode");

        assert_eq!(out.status.code(), Some(78), "exit: {:?}", out.status);
        let v = json_stdout(&out);
        assert_eq!(v["status"], "aborted");
        assert_eq!(v["error"]["code"], "CapabilityUnavailable");
    }
}

#[test]
fn second_non_dry_run_uses_cache() {
    let dir = tempfile::tempdir().unwrap();
    write(
        &dir.path().join("docs/guide.md"),
        "# Cairn Guide\nSee [[Memory Store]].\n",
    );

    let first = cli()
        .current_dir(dir.path())
        .args(["ingest", "--folder", "docs", "--mode", "keyword", "--json"])
        .output()
        .expect("first cairn ingest --folder");
    assert_eq!(first.status.code(), Some(0), "exit: {:?}", first.status);
    assert!(dir.path().join(".cairn/cache").exists());
    let first_json = json_stdout(&first);
    assert_eq!(first_json["records_written"], 0);

    let second = cli()
        .current_dir(dir.path())
        .args(["ingest", "--folder", "docs", "--mode", "keyword", "--json"])
        .output()
        .expect("second cairn ingest --folder");
    assert_eq!(second.status.code(), Some(0), "exit: {:?}", second.status);
    let v = json_stdout(&second);
    assert_eq!(v["cached"], 1);
    assert_eq!(v["processed"], 0);
    assert!(v["elapsed_ms"].as_u64().unwrap() < 100);
}

#[test]
fn docs_folder_keyword_dry_run_extracts_entities() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root");

    let out = cli()
        .current_dir(repo)
        .args([
            "ingest",
            "--folder",
            "docs",
            "--mode",
            "keyword",
            "--dry-run",
            "--json",
        ])
        .output()
        .expect("cairn ingest repo docs");

    assert_eq!(out.status.code(), Some(0), "exit: {:?}", out.status);
    let v = json_stdout(&out);
    assert!(v["entities_new"].as_u64().unwrap() > 0);
}
