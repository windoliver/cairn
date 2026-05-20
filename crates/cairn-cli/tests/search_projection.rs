//! Search projection CLI integration tests.

fn cli() -> std::process::Command {
    std::process::Command::new(env!("CARGO_BIN_EXE_cairn"))
}

#[test]
fn search_json_includes_ranking_signals() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join(".cairn")).expect("mkdir");
    std::fs::write(
        dir.path().join(".cairn/config.yaml"),
        "store:\n  kind: sqlite\n",
    )
    .expect("config");

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
fn search_required_bm25s_fails_closed_without_nexus() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join(".cairn")).expect("mkdir");
    std::fs::write(
        dir.path().join(".cairn/config.yaml"),
        "store:\n  kind: sqlite\n",
    )
    .expect("config");

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
