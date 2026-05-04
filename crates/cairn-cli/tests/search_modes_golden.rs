//! Golden-query CLI snapshot tests — one per search mode.
//!
//! Builds a fixture vault via `cairn_test_fixtures::build_hybrid_test_vault`,
//! runs `cairn search … --json` for each mode, and snapshots the output with
//! `insta`. Edit accepted snapshots with `cargo insta review` after intentional
//! changes.
//!
//! `CAIRN_MOCK_EMBEDDER=1` is set so the CLI subprocess uses `MockEmbedder`
//! (matching the vectors already in the DB) rather than requiring real model
//! weights on disk.

use assert_cmd::Command;
use cairn_test_fixtures::{RecordSpec, build_hybrid_test_vault};

const QUERY: &str = "memory safety in rust";

async fn fixture_vault() -> cairn_test_fixtures::HybridTestVault {
    build_hybrid_test_vault(&[
        RecordSpec::from_body("rust offers memory safety without garbage collection"),
        RecordSpec::from_body("the quick brown fox jumps over the lazy dog"),
        RecordSpec::from_body("ownership and borrowing prevent memory bugs at compile time"),
        RecordSpec::from_body("python is dynamically typed"),
    ])
    .await
}

fn run_cli_search(vault_root: &std::path::Path, mode: &str) -> String {
    let output = Command::cargo_bin("cairn")
        .expect("locate cairn binary")
        .env("CAIRN_VAULT", vault_root)
        .env("CAIRN_MOCK_EMBEDDER", "1")
        .args(["search", QUERY, "--mode", mode, "--json"])
        .output()
        .expect("run cli");
    assert!(
        output.status.success(),
        "cli exit non-zero for mode={mode}.\n  stderr: {}\n  stdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );
    redact_operation_id(&String::from_utf8(output.stdout).expect("utf8 stdout"))
}

/// Replace the per-call `operation_id` ULID in the IDL response envelope
/// with a fixed placeholder so the golden snapshot stays deterministic
/// across runs. The envelope shape is the unit under test; the ULID
/// value is intentionally unique per call (round-8 review #1).
fn redact_operation_id(json: &str) -> String {
    const KEY: &str = "\"operation_id\":\"";
    const ULID_LEN: usize = 26;
    const PLACEHOLDER: &str = "01XXXXXXXXXXXXXXXXXXXXXXXX";
    let mut out = String::with_capacity(json.len());
    let mut rest = json;
    while let Some(idx) = rest.find(KEY) {
        out.push_str(&rest[..idx + KEY.len()]);
        let value_start = idx + KEY.len();
        if rest[value_start..].len() >= ULID_LEN {
            out.push_str(PLACEHOLDER);
            rest = &rest[value_start + ULID_LEN..];
        } else {
            // Shouldn't happen with valid envelopes — fall through.
            rest = &rest[value_start..];
        }
    }
    out.push_str(rest);
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn keyword_mode_snapshot() {
    let vault = fixture_vault().await;
    let root = vault.root.clone();
    let dir = vault.dir;
    // Drop store + embedder so the CLI subprocess can open the DB without
    // lock contention. TempDir is kept alive via `dir` until test end.
    drop(vault.store);
    drop(vault.embedder);
    let stdout = run_cli_search(&root, "keyword");
    insta::assert_snapshot!("search_keyword_json", stdout);
    drop(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn semantic_mode_snapshot() {
    let vault = fixture_vault().await;
    let root = vault.root.clone();
    let dir = vault.dir;
    drop(vault.store);
    drop(vault.embedder);
    let stdout = run_cli_search(&root, "semantic");
    insta::assert_snapshot!("search_semantic_json", stdout);
    drop(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn hybrid_mode_snapshot() {
    let vault = fixture_vault().await;
    let root = vault.root.clone();
    let dir = vault.dir;
    drop(vault.store);
    drop(vault.embedder);
    let stdout = run_cli_search(&root, "hybrid");
    insta::assert_snapshot!("search_hybrid_json", stdout);
    drop(dir);
}
