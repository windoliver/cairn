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
fn folder_help_exposes_live_flags() {
    let out = cli()
        .args(["ingest", "--help"])
        .output()
        .expect("cairn ingest --help");

    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    for flag in [
        "--folder",
        "--include",
        "--exclude",
        "--mode",
        "--batch-size",
        "--dry-run",
    ] {
        assert!(stdout.contains(flag), "help missing {flag}:\n{stdout}");
    }
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
    assert!(
        !dir.path().join(".cairn").exists(),
        "dry-run must not create vault state"
    );
    let v = json_stdout(&out);
    for field in [
        "cached",
        "processed",
        "plans",
        "batch_size",
        "operation_ids",
        "entities_new",
        "entities_merged",
        "edges_new",
        "contradictions_resolved",
        "elapsed_ms",
    ] {
        assert!(v.get(field).is_some(), "missing field {field}: {v}");
    }
    assert_eq!(v["processed"], 1);
    assert_eq!(v["plans"], 1);
    assert_eq!(v["batch_size"], 64);
    assert_eq!(v["operation_ids"].as_array().unwrap().len(), 1);
    assert!(v["entities_new"].as_u64().unwrap() > 0);
}

#[test]
fn folder_operation_ids_are_stable_for_relative_and_absolute_paths() {
    let dir = tempfile::tempdir().unwrap();
    write(
        &dir.path().join("docs/guide.md"),
        "# Cairn Guide\nSee [[Memory Store]].\n",
    );
    let absolute_docs = dir.path().join("docs");

    let relative = cli()
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
        .expect("relative folder ingest");
    assert_eq!(
        relative.status.code(),
        Some(0),
        "exit: {:?}",
        relative.status
    );

    let absolute = cli()
        .current_dir(dir.path())
        .arg("ingest")
        .arg("--folder")
        .arg(&absolute_docs)
        .args(["--mode", "keyword", "--dry-run", "--json"])
        .output()
        .expect("absolute folder ingest");
    assert_eq!(
        absolute.status.code(),
        Some(0),
        "exit: {:?}",
        absolute.status
    );

    let relative_json = json_stdout(&relative);
    let absolute_json = json_stdout(&absolute);
    assert_eq!(
        relative_json["operation_ids"], absolute_json["operation_ids"],
        "same folder must plan the same operation ids"
    );
}

#[test]
fn batch_size_zero_exits_usage() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("docs/guide.md"), "# Cairn Guide\n");

    let out = cli()
        .current_dir(dir.path())
        .args([
            "ingest",
            "--folder",
            "docs",
            "--mode",
            "keyword",
            "--batch-size",
            "0",
            "--json",
        ])
        .output()
        .expect("cairn ingest --folder --batch-size 0");

    assert_eq!(out.status.code(), Some(64), "exit: {:?}", out.status);
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("batch-size") || stderr.contains("batch_size"),
        "stderr missing batch-size diagnostic: {stderr:?}"
    );
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
    assert!(dir.path().join(".cairn/cairn.db").exists());
    let first_json = json_stdout(&first);
    assert_eq!(first_json["processed"], 1);
    assert_eq!(first_json["records_written"], 1);
    assert_eq!(first_json["plans"], 1);

    let second = cli()
        .current_dir(dir.path())
        .args(["ingest", "--folder", "docs", "--mode", "keyword", "--json"])
        .output()
        .expect("second cairn ingest --folder");
    assert_eq!(second.status.code(), Some(0), "exit: {:?}", second.status);
    let v = json_stdout(&second);
    assert_eq!(v["cached"], 1);
    assert_eq!(v["processed"], 0);
    assert_eq!(v["records_written"], 0);
    assert_eq!(v["plans"], 0);
    assert!(v["elapsed_ms"].as_u64().is_some());
}

#[test]
fn changed_second_batch_resumes_without_rewriting_first_batch() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["a.md", "b.md", "c.md"] {
        write(&dir.path().join(name), &format!("# {name}\n[[Entity]]\n"));
    }

    let first = cli()
        .current_dir(dir.path())
        .args([
            "ingest",
            "--folder",
            ".",
            "--mode",
            "keyword",
            "--batch-size",
            "2",
            "--json",
        ])
        .output()
        .expect("first folder ingest");
    assert_eq!(first.status.code(), Some(0), "exit: {:?}", first.status);
    let first_json = json_stdout(&first);
    assert_eq!(first_json["plans"], 2);
    assert_eq!(first_json["records_written"], 3);
    let first_ops = first_json["operation_ids"].as_array().unwrap().clone();

    write(
        &dir.path().join("c.md"),
        "# c changed\n[[Entity]]\n[[Other]]\n",
    );

    let second = cli()
        .current_dir(dir.path())
        .args([
            "ingest",
            "--folder",
            ".",
            "--mode",
            "keyword",
            "--batch-size",
            "2",
            "--json",
        ])
        .output()
        .expect("retry folder ingest");
    assert_eq!(second.status.code(), Some(0), "exit: {:?}", second.status);
    let second_json = json_stdout(&second);
    assert_eq!(second_json["cached"], 2);
    assert_eq!(second_json["processed"], 1);
    assert_eq!(second_json["plans"], 1);
    assert_eq!(second_json["records_written"], 1);
    assert_ne!(second_json["operation_ids"][0], first_ops[1]);
}

#[test]
fn explicitly_included_unsupported_files_are_warned_and_not_cached() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("docs/image.png"), "not keyword text");

    let out = cli()
        .current_dir(dir.path())
        .args(["ingest", "--folder", "docs", "--include", "*.png", "--json"])
        .output()
        .expect("cairn ingest unsupported include");

    assert_eq!(out.status.code(), Some(0), "exit: {:?}", out.status);
    assert!(!dir.path().join(".cairn/cache").exists());
    let v = json_stdout(&out);
    assert_eq!(v["processed"], 0);
    assert_eq!(v["skipped"], 1);
    assert_eq!(v["warnings"], 1);
    assert_eq!(v["entities_new"], 0);
}

#[test]
fn docs_folder_keyword_dry_run_extracts_entities_and_plans() {
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
            "--batch-size",
            "64",
            "--dry-run",
            "--json",
        ])
        .output()
        .expect("cairn ingest repo docs");

    assert_eq!(out.status.code(), Some(0), "exit: {:?}", out.status);
    let v = json_stdout(&out);
    assert!(v["entities_new"].as_u64().unwrap() > 0);
    assert!(v["plans"].as_u64().unwrap() > 0);
    assert_eq!(v["records_written"], 0);
}
