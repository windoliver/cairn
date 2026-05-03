//! CLI snapshot test for `cairn search --explain --json`.
//!
//! Verifies that `score_explain` is present in the JSON output and that its
//! length matches the `hits` array. Snapshots the full output for regression
//! detection.
//!
//! `CAIRN_MOCK_EMBEDDER=1` is set so the CLI subprocess uses `MockEmbedder`
//! (matching the vectors already in the DB) rather than requiring real model
//! weights on disk.

use assert_cmd::Command;
use cairn_test_fixtures::{RecordSpec, build_hybrid_test_vault};

#[tokio::test(flavor = "multi_thread")]
async fn hybrid_explain_block_snapshot() {
    let vault = build_hybrid_test_vault(&[
        RecordSpec::from_body("rust offers memory safety without garbage collection"),
        RecordSpec::from_body("ownership and borrowing prevent memory bugs at compile time"),
        RecordSpec::from_body("python is dynamically typed"),
    ])
    .await;

    let root = vault.root.clone();
    let dir = vault.dir;
    // Drop store + embedder so the CLI subprocess opens the DB without contention.
    drop(vault.store);
    drop(vault.embedder);

    let output = Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", &root)
        .env("CAIRN_MOCK_EMBEDDER", "1")
        .args([
            "search",
            "memory safety",
            "--mode",
            "hybrid",
            "--explain",
            "--json",
        ])
        .output()
        .expect("run cli");
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "exit non-zero. stderr: {stderr}\nstdout: {stdout}"
    );

    // Structural sanity before snapshot.
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("invalid json: {e}\nstdout: {stdout}"));
    assert!(
        parsed.get("score_explain").is_some(),
        "score_explain absent from JSON output: {stdout}"
    );
    let hits = parsed["hits"].as_array().expect("hits must be an array");
    let exps = parsed["score_explain"]
        .as_array()
        .expect("score_explain must be an array");
    assert_eq!(
        hits.len(),
        exps.len(),
        "score_explain length ({}) must match hits length ({})",
        exps.len(),
        hits.len()
    );

    insta::assert_snapshot!("search_explain_json", stdout);
    drop(dir);
}
