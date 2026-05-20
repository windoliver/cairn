//! End-to-end CLI regression for hybrid search semantic-provider degradation.
//!
//! The test drives the real `cairn` binary against a fixture vault. The vault
//! is indexed with the normal mock embedder, then the CLI subprocess is run
//! with a failing mock query embedder so hybrid must fall back to the keyword
//! leg and surface the wire-level `semantic_degraded=true` flag.

#![allow(missing_docs)]

use assert_cmd::Command;
use cairn_test_fixtures::{RecordSpec, build_hybrid_test_vault};

#[tokio::test(flavor = "multi_thread")]
async fn hybrid_search_surfaces_semantic_degraded_on_provider_outage() {
    let vault = build_hybrid_test_vault(&[
        RecordSpec::from_body("issue106 degraded alpha keyword survivor"),
        RecordSpec::from_body("issue106 degraded beta keyword survivor"),
    ])
    .await;

    let root = vault.root.clone();
    let dir = vault.dir;
    drop(vault.store);
    drop(vault.embedder);

    let output = Command::cargo_bin("cairn")
        .expect("locate cairn binary")
        .env("CAIRN_VAULT", &root)
        .env("CAIRN_MOCK_EMBEDDER", "1")
        .env("CAIRN_MOCK_EMBEDDER_FAIL", "network")
        .args(["search", "issue106", "--mode", "hybrid", "--json"])
        .output()
        .expect("run cli");
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "hybrid search must commit keyword fallback results. stderr: {stderr}\nstdout: {stdout}"
    );

    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("invalid json: {e}\nstdout: {stdout}"));
    assert_eq!(parsed["status"], "committed");
    assert_eq!(parsed["data"]["semantic_degraded"], true, "{parsed}");
    let hits = parsed["data"]["hits"].as_array().expect("hits array");
    assert!(
        !hits.is_empty(),
        "keyword fallback must return hits: {parsed}"
    );

    let degraded_legs = parsed["data"]["degraded_legs"]
        .as_array()
        .expect("degraded_legs array");
    assert!(
        degraded_legs
            .iter()
            .any(|leg| leg["leg"] == "semantic" && leg["reason"] == "timeout"),
        "transient semantic outage must surface as semantic timeout leg: {parsed}"
    );

    drop(dir);
}
