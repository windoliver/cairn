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
use serde_json::Value;

/// Normalize volatile fields so the snapshot asserts wire shape, ordering, and
/// explain content without pinning platform-specific float noise.
fn normalize_search_snapshot(json: &str) -> String {
    const PLACEHOLDER: &str = "01XXXXXXXXXXXXXXXXXXXXXXXX";

    let mut value: Value = serde_json::from_str(json).expect("search output must be json");
    value["operation_id"] = Value::String(PLACEHOLDER.to_owned());

    if let Some(hits) = value["data"]["hits"].as_array_mut() {
        for hit in hits {
            if let Some(signals) = hit["ranking_signals"].as_array_mut() {
                for signal in signals {
                    if let Some(score) = signal["score"].as_f64() {
                        let rounded = (score * 1_000_000.0).round() / 1_000_000.0;
                        signal["score"] = Value::Number(
                            serde_json::Number::from_f64(rounded)
                                .expect("rounded score must be finite"),
                        );
                    }
                }
            }
        }
    }

    serde_json::to_string(&value).expect("normalized search output must serialize")
}

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

    // Structural sanity before snapshot. The committed wire shape is
    // the IDL `Response` envelope (round-8 review #1): `data` carries
    // the `SearchData` payload, so `hits` and `score_explain` live one
    // level deeper than the legacy bespoke shape.
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("invalid json: {e}\nstdout: {stdout}"));
    assert_eq!(
        parsed["contract"], "cairn.mcp.v1",
        "search must emit the cairn.mcp.v1 envelope; got {stdout}"
    );
    assert_eq!(
        parsed["status"], "committed",
        "successful search must be status=committed; got {stdout}"
    );
    assert_eq!(
        parsed["verb"], "search",
        "verb must be search; got {stdout}"
    );
    let data = parsed.get("data").expect("envelope.data is required");
    assert!(
        data.get("score_explain").is_some(),
        "score_explain absent from data payload: {stdout}"
    );
    let hits = data["hits"].as_array().expect("hits must be an array");
    let exps = data["score_explain"]
        .as_array()
        .expect("score_explain must be an array");
    assert_eq!(
        hits.len(),
        exps.len(),
        "score_explain length ({}) must match hits length ({})",
        exps.len(),
        hits.len()
    );

    insta::assert_snapshot!("search_explain_json", normalize_search_snapshot(&stdout));
    drop(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn keyword_explain_includes_policy_trace_and_dedup_exclusions() {
    let duplicate = "issue62explaindedup duplicate body";
    let vault = build_hybrid_test_vault(&[
        RecordSpec::from_body(duplicate),
        RecordSpec::from_body(duplicate),
        RecordSpec::from_body("issue62explaindedup unrelated survivor"),
    ])
    .await;

    let root = vault.root.clone();
    let dir = vault.dir;
    drop(vault.store);
    drop(vault.embedder);

    let output = Command::cargo_bin("cairn")
        .expect("cairn binary")
        .env("CAIRN_VAULT", &root)
        .env("CAIRN_MOCK_EMBEDDER", "1")
        .args([
            "search",
            "issue62explaindedup",
            "--mode",
            "keyword",
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

    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("invalid json: {e}\nstdout: {stdout}"));
    assert_eq!(parsed["status"], "committed");
    let trace = parsed["policy_trace"]
        .as_array()
        .expect("policy_trace array");
    assert!(
        trace
            .iter()
            .any(|entry| { entry["gate"] == "search.scope" && entry["result"] == "pass" }),
        "search --explain must trace the scope gate: {parsed}"
    );
    assert!(
        trace
            .iter()
            .any(|entry| { entry["gate"] == "search.capability" && entry["result"] == "pass" }),
        "search --explain must trace the capability gate: {parsed}"
    );
    assert!(
        trace
            .iter()
            .any(|entry| { entry["gate"] == "search.read_filter" && entry["result"] == "deny" }),
        "dedup exclusion must aggregate into search.read_filter=deny: {parsed}"
    );

    let excluded = parsed["data"]["excluded"]
        .as_array()
        .expect("explain search must include data.excluded");
    assert_eq!(
        excluded.len(),
        1,
        "duplicate visible candidate should produce one exclusion: {parsed}"
    );
    assert_eq!(excluded[0]["gate"], "read_filter_dedup");
    assert_eq!(
        excluded[0]["detail"], "",
        "exclusion detail must be the body-free PolicyDetail wire string"
    );
    assert!(
        excluded[0]["target_id"].as_str().is_some(),
        "exclusion carries a target id"
    );
    drop(dir);
}
