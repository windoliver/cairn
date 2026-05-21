//! Release-only lifecycle benches.
//!
//! Brief §15 SLOs covered here:
//! - `cold_rehydrate_p95`: first `assemble_hot` on a freshly-opened ≤10 MB
//!   vault subprocess. Brief §15 target: < 3 s p95.
//!
//! NOT covered here (deferred to a follow-up):
//! - 1M-record forget Phase A / Phase B latencies. Building a 1M-record
//!   vault on every CI run is prohibitively slow; these need a pre-built
//!   corpus checked into release artifacts, not generated at bench time.
//!
//! Each iteration spawns a fresh `cairn assemble_hot --json` subprocess so
//! no in-process cache is warm — that's the "cold" in cold-rehydrate.

#![allow(missing_docs)] // criterion_group!/main! generate undocumented items

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use tempfile::TempDir;

/// Path to the `cairn` binary, baked at build time via build.rs (same env
/// var the latency benches use).
fn cairn_bin() -> PathBuf {
    PathBuf::from(env!("CAIRN_BIN_PATH"))
}

/// A vault seeded with ~10 MB of body content across many records so the
/// hot-prefix assembly has real material to compose.
struct LargeSeededVault {
    _dir: TempDir,
    path: PathBuf,
}

impl LargeSeededVault {
    fn new() -> anyhow::Result<Self> {
        let bin = cairn_bin();
        let dir = tempfile::tempdir()?;
        let path = dir.path().to_path_buf();
        bootstrap(&bin, &path)?;

        // Seed identity.
        ingest(&bin, &path, "reference", "identity seed")?;

        // 200 × ~50 KB = ~10 MB total body content. The hot-prefix assembler
        // tops out at 25 KB so the bench measures selection cost over real
        // record material, not just empty walks.
        let body = "x".repeat(50_000);
        for i in 0..200_u32 {
            ingest(&bin, &path, "reference", &format!("doc-{i}: {body}"))?;
        }

        Ok(Self { _dir: dir, path })
    }
}

fn bootstrap(bin: &Path, vault: &Path) -> anyhow::Result<()> {
    let out = Command::new(bin)
        .args(["bootstrap", "--vault-path", &vault.to_string_lossy()])
        .output()?;
    anyhow::ensure!(
        out.status.success(),
        "bootstrap failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}

fn ingest(bin: &Path, vault: &Path, kind: &str, body: &str) -> anyhow::Result<()> {
    let out = Command::new(bin)
        .current_dir(vault)
        .env_remove("CAIRN_VAULT")
        .env_remove("CAIRN_REGISTRY")
        .args(["ingest", "--kind", kind, "--body", body, "--json"])
        .output()?;
    anyhow::ensure!(
        out.status.success(),
        "ingest failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}

fn bench_cold_rehydrate(c: &mut Criterion) {
    let v = LargeSeededVault::new().expect("seed large vault");
    let bin = cairn_bin();
    c.bench_function("cold_rehydrate_p95", |b| {
        b.iter(|| {
            let out = Command::new(&bin)
                .current_dir(&v.path)
                .args(["assemble_hot", "--json"])
                .output()
                .expect("spawn assemble_hot");
            assert!(
                out.status.success(),
                "assemble_hot failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        });
    });
}

criterion_group! {
    name = lifecycle;
    config = Criterion::default()
        .measurement_time(Duration::from_secs(30))
        .sample_size(10)
        .warm_up_time(Duration::from_secs(3));
    targets = bench_cold_rehydrate
}
criterion_main!(lifecycle);
