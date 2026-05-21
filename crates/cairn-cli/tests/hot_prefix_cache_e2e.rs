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

fn hot_prefix_metrics(content: &str) -> Vec<serde_json::Value> {
    content
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|metric| metric["event"] == "hot_prefix_assembled")
        .collect()
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
    let metrics1 = hot_prefix_metrics(&content1);
    assert_eq!(metrics1.len(), 1, "after 1 call: {content1}");
    let line1 = &metrics1[0];
    assert_eq!(line1["event"], "hot_prefix_assembled");
    assert_eq!(
        line1["cache_hit"], false,
        "first call must be a cache miss; got line1={line1}"
    );

    // Second call → cache hit → 2 lines, last line cache_hit: true.
    assemble_hot(dir.path());
    let content2 = std::fs::read_to_string(&metrics_path).expect("read metrics #2");
    let metrics2 = hot_prefix_metrics(&content2);
    assert_eq!(metrics2.len(), 2, "after 2 calls: {content2}");
    let line2 = &metrics2[1];
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
    let metrics = hot_prefix_metrics(&content);
    assert_eq!(
        metrics.len(),
        3,
        "expected 3 hot-prefix metric lines, got {}: {content}",
        metrics.len()
    );

    let last = &metrics[2];
    assert_eq!(
        last["cache_hit"], false,
        "watermark bump must invalidate the cache; last metric line: {last}"
    );
}
