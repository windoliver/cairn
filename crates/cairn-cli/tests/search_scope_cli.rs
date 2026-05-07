//! End-to-end CLI smoke test for `cairn search --scope-tenant …` (issue #191).
//!
//! Boots a real fixture vault with two-tenant seed records, invokes the
//! CLI binary with `--scope-tenant`, and asserts the JSON envelope only
//! contains rows from the requested tenant.

#![allow(missing_docs)]

use assert_cmd::Command;
use cairn_core::domain::ScopeTuple;
use cairn_test_fixtures::{RecordSpec, build_hybrid_test_vault};

/// Helper: invoke `cairn search` with the given args and decode the JSON
/// data block.
fn run_cli_search(vault_root: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let output = Command::cargo_bin("cairn")
        .expect("locate cairn binary")
        .env("CAIRN_VAULT", vault_root)
        .env("CAIRN_MOCK_EMBEDDER", "1")
        .args(args)
        .output()
        .expect("run cli");
    assert!(
        output.status.success(),
        "cli exit non-zero.\n  args: {args:?}\n  stderr: {}\n  stdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let envelope: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("envelope is JSON");
    envelope["data"].clone()
}

#[tokio::test]
async fn search_scope_tenant_flag_narrows_results() {
    let vault = build_hybrid_test_vault(&[
        RecordSpec::from_body("alpha shared keyword acme record one").with_scope(ScopeTuple {
            tenant: Some("acme".into()),
            user: Some("hmn:tafeng".into()),
            ..Default::default()
        }),
        RecordSpec::from_body("alpha shared keyword globex record one").with_scope(ScopeTuple {
            tenant: Some("globex".into()),
            user: Some("hmn:tafeng".into()),
            ..Default::default()
        }),
    ])
    .await;

    // Baseline: no scope flag, both tenants should surface.
    let baseline = run_cli_search(
        &vault.root,
        &["search", "alpha", "--mode", "keyword", "--json"],
    );
    let hits = baseline["hits"].as_array().expect("hits is an array");
    assert_eq!(
        hits.len(),
        2,
        "baseline must return both tenants; got {hits:?}"
    );

    // With `--scope-tenant acme`, only the acme record returns.
    let scoped = run_cli_search(
        &vault.root,
        &[
            "search",
            "alpha",
            "--mode",
            "keyword",
            "--scope-tenant",
            "acme",
            "--json",
        ],
    );
    let hits = scoped["hits"].as_array().expect("hits is an array");
    assert_eq!(
        hits.len(),
        1,
        "--scope-tenant acme must narrow to one record; got {hits:?}"
    );
    let id = hits[0]["record_id"].as_str().expect("record_id");
    // Records are seeded in array order with suffix `00`, `01`, etc., so
    // the acme one (index 0) ends in `00`.
    assert!(
        id.ends_with("00"),
        "scoped result must be the acme record; got {id}",
    );
}

#[tokio::test]
async fn search_scope_tenant_unmatched_returns_empty() {
    let vault = build_hybrid_test_vault(&[RecordSpec::from_body("alpha solo record").with_scope(
        ScopeTuple {
            tenant: Some("acme".into()),
            user: Some("hmn:tafeng".into()),
            ..Default::default()
        },
    )])
    .await;
    let data = run_cli_search(
        &vault.root,
        &[
            "search",
            "alpha",
            "--mode",
            "keyword",
            "--scope-tenant",
            "nonexistent",
            "--json",
        ],
    );
    let hits = data["hits"].as_array().expect("hits");
    assert!(
        hits.is_empty(),
        "unmatched scope must return zero hits, not error; got {hits:?}",
    );
}

#[test]
fn search_help_lists_scope_flags() {
    let out = Command::cargo_bin("cairn")
        .expect("locate cairn binary")
        .args(["search", "--help"])
        .output()
        .expect("cairn search --help");
    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    for flag in [
        "--scope-tenant",
        "--scope-workspace",
        "--scope-user",
        "--scope-agent",
    ] {
        assert!(
            stdout.contains(flag),
            "search --help missing {flag} flag.\nfull help:\n{stdout}",
        );
    }
}
