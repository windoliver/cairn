//! Release-scorecard integration guardrails.
//!
//! These tests keep the manual `bench-full` workflow and docs reference home
//! aligned with the `cairn-bench scorecard` subcommand contract.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("cairn-bench crate is two levels below repo root")
        .to_path_buf()
}

#[test]
fn bench_full_workflow_invokes_scorecard_subcommand() {
    let workflow = std::fs::read_to_string(repo_root().join(".github/workflows/ci.yml"))
        .expect("read ci workflow");

    assert!(
        workflow.contains("cargo run --release -p cairn-bench --features openai --locked -- \\\n              scorecard \\\n              --fixture fixtures/v0/brainbench-world-v1"),
        "OpenAI bench-full path must invoke the scorecard subcommand"
    );
    assert!(
        workflow.contains("cargo run --release -p cairn-bench --locked -- \\\n              scorecard \\\n              --fixture fixtures/v0/brainbench-world-v1"),
        "offline bench-full path must invoke the scorecard subcommand"
    );
    assert!(
        workflow.contains("BENCH_FETCH_MODELS: \"1\""),
        "manual full-corpus workflow must opt in to BGE model fetches"
    );
}

#[test]
fn bench_reference_home_is_committed_to_docs_site() {
    let root = repo_root();

    assert!(
        root.join("docs/site/src/reference/bench/index.md")
            .is_file(),
        "release scorecards need a committed docs reference home"
    );

    let summary = std::fs::read_to_string(root.join("docs/site/src/SUMMARY.md"))
        .expect("read mdbook summary");
    assert!(
        summary.contains("reference/bench/index.md"),
        "mdBook summary should link the bench reference home"
    );
}

#[test]
fn scorecard_default_run_skips_uncached_bge_without_network_opt_in() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fixture = dir.path().join("fixture");
    let pages = fixture.join("pages");
    std::fs::create_dir_all(&pages).expect("create pages dir");
    std::fs::write(
        pages.join("alpha.json"),
        r#"{"slug":"alpha","title":"Alpha","compiled_truth":"Alpha keeps the bench tiny.","_facts":{}}"#,
    )
    .expect("write page");
    std::fs::write(
        fixture.join("queries.json"),
        r#"[{"id":"q1","query":"Which page mentions Alpha?","relevant":["alpha"],"grades":{"alpha":1}}]"#,
    )
    .expect("write queries");

    let out_dir = dir.path().join("out");
    let output = Command::new(env!("CARGO_BIN_EXE_cairn-bench"))
        .current_dir(dir.path())
        .env_remove("BENCH_FETCH_MODELS")
        .env_remove("OPENAI_API_KEY")
        .args([
            "scorecard",
            "--fixture",
            fixture.to_str().expect("utf-8 fixture"),
            "--out-dir",
            out_dir.to_str().expect("utf-8 out_dir"),
            "--skip-openai",
        ])
        .output()
        .expect("run cairn-bench scorecard");

    assert!(
        output.status.success(),
        "scorecard should succeed without network opt-in; stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("skipping adapter `vector-bge`"),
        "uncached BGE adapter should be skipped, not fetched: {stderr}"
    );

    let report = std::fs::read_to_string(out_dir.join("report.md")).expect("read report");
    assert!(report.contains("`bm25-only`"));
    assert!(report.contains("`vector-bge` | 0.000"));
    assert!(report.contains("N/A | `hybrid-bge-rrf` P@5"));
}
