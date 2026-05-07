//! Tests for `cairn ingest --folder` extraction cache behaviour.

use std::fs;
use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn write_note(vault: &std::path::Path, body: &str) {
    let docs = vault.join("docs");
    fs::create_dir_all(&docs).expect("create docs dir");
    fs::write(docs.join("note.md"), body).expect("write markdown note");
}

fn run_folder_ingest(vault: &std::path::Path, no_cache: bool) -> serde_json::Value {
    let mut cmd = cli();
    cmd.current_dir(vault).args([
        "ingest",
        "--kind",
        "reference",
        "--folder",
        "docs",
        "--json",
    ]);
    if no_cache {
        cmd.arg("--no-cache");
    }
    let out = cmd.output().expect("run cairn ingest --folder");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("stdout is JSON response")
}

fn run_positional_folder_ingest(vault: &std::path::Path) -> serde_json::Value {
    let mut cmd = cli();
    cmd.current_dir(vault)
        .args(["ingest", "--kind", "reference", "docs", "--json"]);
    let out = cmd.output().expect("run cairn ingest positional folder");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("stdout is JSON response")
}

#[test]
fn folder_ingest_writes_and_reuses_extraction_cache_entry() {
    let vault = tempfile::tempdir().expect("temp vault");
    write_note(vault.path(), "---\ntitle: First\n---\nbody");

    let first = run_folder_ingest(vault.path(), false);
    assert_eq!(first["status"], "committed");
    assert_eq!(first["data"]["files_processed"], 1);
    assert_eq!(first["data"]["cache_hits"], 0);
    assert_eq!(first["data"]["cache_misses"], 1);
    assert_eq!(first["data"]["cache_writes"], 1);

    let cache_files = fs::read_dir(vault.path().join(".cairn/cache"))
        .expect("cache dir exists")
        .collect::<Result<Vec<_>, _>>()
        .expect("read cache dir");
    assert_eq!(cache_files.len(), 1);
    let cache_entry: serde_json::Value =
        serde_json::from_slice(&fs::read(cache_files[0].path()).expect("read cache entry"))
            .expect("cache entry is JSON");
    assert_eq!(cache_entry["entity_count"], 1);
    assert_eq!(cache_entry["edge_count"], 0);
    assert_eq!(cache_entry["nodes"][0]["kind"], "source_document");
    assert_eq!(cache_entry["nodes"][0]["source_path"], "docs/note.md");

    let second = run_folder_ingest(vault.path(), false);
    assert_eq!(second["status"], "committed");
    assert_eq!(second["data"]["files_processed"], 1);
    assert_eq!(second["data"]["cache_hits"], 1);
    assert_eq!(second["data"]["cache_misses"], 0);
    assert_eq!(second["data"]["cache_writes"], 0);
}

#[test]
fn positional_folder_source_runs_folder_cache_ingest() {
    let vault = tempfile::tempdir().expect("temp vault");
    write_note(vault.path(), "body");

    let resp = run_positional_folder_ingest(vault.path());
    assert_eq!(resp["status"], "committed");
    assert_eq!(resp["data"]["files_processed"], 1);
    assert_eq!(resp["data"]["cache_misses"], 1);
    assert_eq!(resp["data"]["cache_writes"], 1);
}

#[test]
fn folder_ingest_no_cache_bypasses_lookup_but_writes_entry() {
    let vault = tempfile::tempdir().expect("temp vault");
    write_note(vault.path(), "---\ntitle: First\n---\nbody");

    let first = run_folder_ingest(vault.path(), false);
    assert_eq!(first["data"]["cache_writes"], 1);

    write_note(vault.path(), "---\ntitle: Second\n---\nbody");
    let second = run_folder_ingest(vault.path(), true);
    assert_eq!(second["status"], "committed");
    assert_eq!(second["data"]["files_processed"], 1);
    assert_eq!(second["data"]["cache_hits"], 0);
    assert_eq!(second["data"]["cache_misses"], 1);
    assert_eq!(second["data"]["cache_writes"], 1);

    let cache_files = fs::read_dir(vault.path().join(".cairn/cache"))
        .expect("cache dir exists")
        .collect::<Result<Vec<_>, _>>()
        .expect("read cache dir");
    assert_eq!(
        cache_files.len(),
        1,
        "frontmatter-only edits must keep the same cache key"
    );
}

#[test]
fn folder_ingest_processes_binary_sidecars() {
    let vault = tempfile::tempdir().expect("temp vault");
    write_note(vault.path(), "body");
    fs::write(vault.path().join("docs/blob.bin"), [0xff, 0x00, 0xfe, 0x41])
        .expect("write binary sidecar");

    let resp = run_folder_ingest(vault.path(), false);
    assert_eq!(resp["status"], "committed");
    assert_eq!(resp["data"]["files_processed"], 2);
    assert_eq!(resp["data"]["cache_misses"], 2);
    assert_eq!(resp["data"]["cache_writes"], 2);
}

#[cfg(unix)]
#[test]
fn folder_ingest_skips_symlinked_directory_loops() {
    use std::os::unix::fs::symlink;

    let vault = tempfile::tempdir().expect("temp vault");
    write_note(vault.path(), "body");
    symlink("..", vault.path().join("docs/loop")).expect("create directory symlink loop");

    let resp = run_folder_ingest(vault.path(), false);
    assert_eq!(resp["status"], "committed");
    assert_eq!(resp["data"]["files_processed"], 1);
    assert_eq!(resp["data"]["cache_misses"], 1);
    assert_eq!(resp["data"]["cache_writes"], 1);
}
