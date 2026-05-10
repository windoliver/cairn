//! E2E for hot-prefix cache: metrics jsonl + cache hit on the second call.
//! See issue #83.

use std::path::Path;
use std::process::Command;

use cairn_cli::vault::{BootstrapOpts, bootstrap};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn opts(dir: &Path) -> BootstrapOpts {
    BootstrapOpts {
        vault_path: dir.to_path_buf(),
        force: true,
    }
}

fn seed_default_identity(vault: &Path) {
    let output = cli()
        .current_dir(vault)
        .args([
            "ingest",
            "--kind",
            "reference",
            "--body",
            "identity seed",
            "--json",
        ])
        .output()
        .expect("ingest seed");
    assert!(
        output.status.success(),
        "ingest seed failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assemble_hot(vault: &Path) {
    let output = cli()
        .current_dir(vault)
        .args(["assemble_hot", "--json"])
        .output()
        .expect("assemble_hot");
    assert!(
        output.status.success(),
        "assemble_hot failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn assemble_hot_writes_one_metrics_line_per_call() {
    let dir = tempfile::tempdir().expect("tempdir");
    bootstrap(&opts(dir.path())).expect("bootstrap");
    seed_default_identity(dir.path());

    let metrics_path = dir.path().join(".cairn/metrics.jsonl");

    // First call → cache miss → 1 line.
    assemble_hot(dir.path());
    let content1 = std::fs::read_to_string(&metrics_path).expect("read metrics #1");
    assert_eq!(content1.lines().count(), 1, "after 1 call");
    let line1: serde_json::Value =
        serde_json::from_str(content1.lines().next().unwrap()).expect("parse #1");
    assert_eq!(line1["event"], "hot_prefix_assembled");
    assert_eq!(
        line1["cache_hit"], false,
        "first call must be a cache miss; got line1={line1}"
    );

    // Second call → cache hit → 2 lines, last line cache_hit: true.
    assemble_hot(dir.path());
    let content2 = std::fs::read_to_string(&metrics_path).expect("read metrics #2");
    let all_lines: Vec<&str> = content2.lines().collect();
    assert_eq!(all_lines.len(), 2, "after 2 calls");
    let line2: serde_json::Value = serde_json::from_str(all_lines[1]).expect("parse #2");
    assert_eq!(line2["event"], "hot_prefix_assembled");
    assert_eq!(
        line2["cache_hit"], true,
        "second call must hit; got line2={line2}"
    );
}

#[test]
fn watermark_bump_invalidates_cache_across_cli_calls() {
    use cairn_core::contract::hot_prefix_cache::HotPrefixCache;
    use cairn_core::domain::hot_prefix::SourceClass;

    let dir = tempfile::tempdir().expect("tempdir");
    bootstrap(&opts(dir.path())).expect("bootstrap");
    seed_default_identity(dir.path());

    let metrics_path = dir.path().join(".cairn/metrics.jsonl");

    // Warm the cache: first call (miss), second call (hit).
    assemble_hot(dir.path());
    assemble_hot(dir.path());

    // Simulate what `forget` will do once wired: bump a source-class
    // watermark via the cache lib API. Bypasses the unwired CLI
    // `forget` verb (capability gated; see envelope_tests.rs).
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(async {
        let cache = cairn_store_sqlite::SqliteHotPrefixCache::open(dir.path())
            .await
            .expect("open cache");
        cache
            .bump(&[SourceClass::ProfileEvidence])
            .await
            .expect("bump");
    });

    // Third call: must be a cache MISS because the watermark moved.
    assemble_hot(dir.path());

    let content = std::fs::read_to_string(&metrics_path).expect("read metrics");
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(
        lines.len(),
        3,
        "expected 3 metric lines, got {}: {content}",
        lines.len()
    );

    let last: serde_json::Value = serde_json::from_str(lines[2]).expect("parse last");
    assert_eq!(
        last["cache_hit"], false,
        "watermark bump must invalidate the cache; last metric line: {last}"
    );
}
