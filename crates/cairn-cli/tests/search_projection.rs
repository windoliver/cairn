//! Search projection CLI integration tests.

const RECORD_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

fn cli() -> std::process::Command {
    std::process::Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn write_config(dir: &tempfile::TempDir, body: &str) {
    std::fs::create_dir(dir.path().join(".cairn")).expect("mkdir");
    std::fs::write(dir.path().join(".cairn/config.yaml"), body).expect("config");
}

fn seed_sqlite_record(dir: &tempfile::TempDir) {
    let store = cairn_store_sqlite::SqliteMemoryStore::open(&dir.path().join(".cairn/cairn.db"))
        .expect("open sqlite store");
    store
        .insert_test_record(
            RECORD_ID,
            "projection search maps sqlite ranking signals",
            1,
            "hash-1",
        )
        .expect("insert test record");
}

#[test]
fn search_json_includes_ranking_signals() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_config(&dir, "store:\n  kind: sqlite\n");

    let out = cli()
        .current_dir(dir.path())
        .args([
            "search",
            "projection",
            "--mode",
            "keyword",
            "--bm25s",
            "disabled",
            "--json",
        ])
        .output()
        .expect("cairn search");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(stdout.contains("\"ranking_signals\""), "{stdout}");
    assert!(stdout.contains("\"sqlite_fts5\""), "{stdout}");
}

#[test]
fn search_json_maps_sqlite_ranking_signal() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_config(&dir, "store:\n  kind: sqlite\n");
    seed_sqlite_record(&dir);

    let out = cli()
        .current_dir(dir.path())
        .args([
            "search",
            "projection",
            "--mode",
            "keyword",
            "--bm25s",
            "disabled",
            "--json",
        ])
        .output()
        .expect("cairn search");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).expect("search json");
    assert_eq!(value["hits"][0]["record_id"], RECORD_ID);
    assert_eq!(
        value["hits"][0]["ranking_signals"][0]["name"],
        "sqlite_fts5"
    );
    assert_eq!(value["hits"][0]["ranking_signals"][0]["used"], true);
    assert!(value["hits"][0]["ranking_signals"][0]["score"].is_number());
}

#[test]
fn search_required_bm25s_fails_closed_without_nexus() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_config(&dir, "store:\n  kind: sqlite\n");

    let out = cli()
        .current_dir(dir.path())
        .args([
            "search",
            "projection",
            "--mode",
            "keyword",
            "--bm25s",
            "required",
            "--json",
        ])
        .output()
        .expect("cairn search");

    assert!(!out.status.success(), "expected fail-closed search");
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(stderr.contains("CapabilityUnavailable"), "{stderr}");
}

#[test]
fn search_required_bm25s_fails_closed_until_nexus_ranker_is_wired() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_config(
        &dir,
        "store:\n  kind: nexus-sandbox\n  nexus:\n    endpoint: http://127.0.0.1:1\n",
    );

    let out = cli()
        .current_dir(dir.path())
        .args([
            "search",
            "projection",
            "--mode",
            "keyword",
            "--bm25s",
            "required",
            "--json",
        ])
        .output()
        .expect("cairn search");

    assert!(!out.status.success(), "expected fail-closed search");
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(stderr.contains("CapabilityUnavailable"), "{stderr}");
}
