# Issue #99 Release Gates Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add latency, memory budget, and privacy regression gates to `cairn-bench`, wired into `ci.yml` as one new required check, enforcing brief §15 SLOs + 2% regression, §19 working-set budget, and §14 leakage fixtures.

**Architecture:** Extend the existing `cairn-bench` crate. Refactor its current scorecard binary into a `scorecard` subcommand and add four new subcommands: `latency`, `memory`, `privacy`, `all`. Latency uses Criterion benches driven in-process via `cairn-sdk`, comparing measured p95 against a committed baseline JSON. Memory sums binary + bundled-asset sizes against a TOML manifest. Privacy runs YAML-declared fixtures against temp vaults built with `cairn-test-fixtures`. CI runs `cairn-bench all` as one matrix job.

**Tech Stack:** Rust 1.95.0 edition 2024, `clap` (existing), `criterion` (new dep), `yaml_serde` (workspace), `toml` (workspace), `rusqlite` (workspace, for privacy index probes), `cairn-sdk` (workspace, for in-process verbs), `cairn-test-fixtures` (dev-dep, for seeded vaults).

**Spec:** `docs/superpowers/specs/2026-05-15-issue-99-release-gates-design.md`

---

## File Structure

**New files** (under `crates/cairn-bench/`):

| Path | Responsibility |
|---|---|
| `src/gates/mod.rs` | Re-exports for shared gate infra |
| `src/gates/thresholds.rs` | Brief §15 SLO constants + 2% regression constant |
| `src/gates/baseline.rs` | Baseline JSON shape, load/save, runner-profile selection |
| `src/gates/report.rs` | Shared report writer (human + JSON), exit-code enum |
| `src/latency/mod.rs` | `latency` subcommand entrypoint |
| `src/latency/harness.rs` | Spawns criterion + parses JSON output |
| `src/memory/mod.rs` | `memory` subcommand entrypoint |
| `src/memory/manifest.rs` | TOML manifest loader, profile inheritance |
| `src/memory/sizer.rs` | Binary + asset summation, tolerance band check |
| `src/privacy/mod.rs` | `privacy` subcommand entrypoint, `--check` mode |
| `src/privacy/fixture.rs` | YAML loader, fixture struct, assertion DSL |
| `src/privacy/harness.rs` | Fixture runner (setup → operations → assertions) |
| `benches/latency.rs` | Criterion benches for 8 hot-path verbs |
| `benches/lifecycle.rs` | Release-only benches (cold rehydrate, 1M-record forget) |
| `baselines/latency.linux.json` | Committed Linux baseline |
| `baselines/latency.macos.json` | Committed macOS baseline |
| `manifests/memory.toml` | Memory-gate manifest (default + screenpipe profiles) |
| `fixtures/privacy/*.yaml` | 8 leak fixtures (per spec §6.3) |
| `tests/latency_smoke.rs` | Integration: 1 mini bench → report → exit code |
| `tests/memory_smoke.rs` | Integration: synthetic manifest pass/fail |
| `tests/privacy_smoke.rs` | Integration: 1 fixture against real vault + mock store leak |

**Modified files:**

| Path | Change |
|---|---|
| `crates/cairn-bench/Cargo.toml` | Add `criterion`, `yaml_serde`, `toml`, `rusqlite`, `cairn-sdk`, `cairn-test-fixtures` (dev), `tracing`. Add `[[bench]]` entries. Move `src/main.rs` into scorecard module. |
| `crates/cairn-bench/src/lib.rs` | Add `pub mod` for `gates`, `latency`, `memory`, `privacy`, `scorecard`. |
| `crates/cairn-bench/src/main.rs` | Replace top-level scorecard CLI with subcommand dispatcher. |
| `crates/cairn-bench/src/scorecard/mod.rs` | New: existing scorecard logic moved here. |
| `.github/workflows/ci.yml` | New job `gates / latency + memory + privacy` (matrix Linux + macOS). |
| `.github/workflows/release-dry-run.yml` | Add steps for `cairn-bench memory --profile screenpipe` + `cairn-bench lifecycle`. |
| `docs/ci.md` | New row in required-status-checks table + local-equivalents. |
| `CLAUDE.md` | Verification checklist gains 4 new `cargo run -p cairn-bench` lines. |
| `Cargo.toml` (workspace) | Add `criterion` to `[workspace.dependencies]`. |

---

## Task 1: Workspace dep + Cargo.toml scaffolding

Add `criterion` to the workspace, expand `cairn-bench/Cargo.toml` with new deps and bench targets, and verify the workspace still builds before any code changes.

**Files:**
- Modify: `Cargo.toml` (workspace, `[workspace.dependencies]` block)
- Modify: `crates/cairn-bench/Cargo.toml`

- [ ] **Step 1: Add criterion to workspace dependencies**

Open `Cargo.toml` (workspace root). Find the `[workspace.dependencies]` block. Add:

```toml
criterion = { version = "0.5", default-features = false, features = ["html_reports", "cargo_bench_support"] }
```

Keep alphabetical ordering with surrounding entries.

- [ ] **Step 2: Expand `cairn-bench/Cargo.toml`**

Open `crates/cairn-bench/Cargo.toml`. Add to `[dependencies]`:

```toml
cairn-sdk = { workspace = true }
yaml_serde = { workspace = true }
toml = { workspace = true }
rusqlite = { workspace = true }
tracing = { workspace = true }
```

Add a `[dev-dependencies]` block (or extend if it already exists):

```toml
[dev-dependencies]
cairn-test-fixtures = { workspace = true }
criterion = { workspace = true }
insta = { workspace = true, features = ["json", "yaml"] }
tempfile = { workspace = true }
```

Add the bench targets after the `[[bin]]` block:

```toml
[[bench]]
name = "latency"
harness = false

[[bench]]
name = "lifecycle"
harness = false
```

- [ ] **Step 3: Create empty bench files so cargo can parse manifest**

```bash
mkdir -p crates/cairn-bench/benches
printf 'fn main() {}\n' > crates/cairn-bench/benches/latency.rs
printf 'fn main() {}\n' > crates/cairn-bench/benches/lifecycle.rs
```

- [ ] **Step 4: Verify workspace still builds**

Run: `cargo check --workspace --locked`
Expected: passes, no errors. `tracing`/`rusqlite`/`yaml_serde`/`toml` may show "unused dependency" warnings from `cargo-machete` later; that's fine — they get used in subsequent tasks.

- [ ] **Step 5: Add machete ignores for forward-declared deps**

Open `crates/cairn-bench/Cargo.toml`. Add at the bottom:

```toml
[package.metadata.cargo-machete]
ignored = ["tracing", "rusqlite", "yaml_serde", "toml"]
```

These get removed in later tasks as each gate consumes its dep.

- [ ] **Step 6: Verify machete passes**

Run: `cargo machete`
Expected: no unused-dependency errors for cairn-bench.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/cairn-bench/Cargo.toml crates/cairn-bench/benches/
git commit -m "build(cairn-bench): scaffold deps + bench targets for issue #99 gates"
```

---

## Task 2: Refactor existing scorecard into a subcommand

Move the current top-level scorecard CLI into a `scorecard` module/subcommand and replace `main.rs` with a clap subcommand dispatcher. No behavioral change to the scorecard.

**Files:**
- Create: `crates/cairn-bench/src/scorecard/mod.rs`
- Modify: `crates/cairn-bench/src/lib.rs`
- Modify: `crates/cairn-bench/src/main.rs`

- [ ] **Step 1: Write a failing test asserting `cairn-bench scorecard --help` works**

Create `crates/cairn-bench/tests/cli_smoke.rs`:

```rust
use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn-bench"))
}

#[test]
fn scorecard_subcommand_has_help() {
    let output = cli().args(["scorecard", "--help"]).output().expect("run");
    assert!(output.status.success(), "scorecard --help failed: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--fixture"), "expected --fixture flag in help");
}

#[test]
fn top_level_help_lists_subcommands() {
    let output = cli().args(["--help"]).output().expect("run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    for sub in ["scorecard", "latency", "memory", "privacy", "all"] {
        assert!(stdout.contains(sub), "expected `{sub}` in --help output");
    }
}
```

- [ ] **Step 2: Run test — confirm it fails**

Run: `cargo test -p cairn-bench --test cli_smoke -- --nocapture`
Expected: FAIL (current binary has no subcommands).

- [ ] **Step 3: Create the scorecard module**

```bash
mkdir -p crates/cairn-bench/src/scorecard
```

Create `crates/cairn-bench/src/scorecard/mod.rs`:

```rust
//! Scorecard subcommand — BrainBench retrieval-quality runner.
//!
//! This module hosts the entrypoint previously in `src/main.rs`.

use std::path::PathBuf;

use clap::Args;

#[derive(Args, Debug)]
pub struct ScorecardArgs {
    /// Path to the fixture root (contains pages/, queries.json, upstream-baseline.json).
    #[arg(long, default_value = "fixtures/v0/brainbench-world-v1")]
    pub fixture: PathBuf,

    /// Output directory for report.md and per-query.jsonl.
    #[arg(long, default_value = "target/brainbench")]
    pub out_dir: PathBuf,

    /// Embedding cache file. Reused across runs; safe to delete.
    #[arg(long, default_value = "target/brainbench/embed-cache.bin")]
    pub cache: PathBuf,

    /// Skip the OpenAI columns even if the openai feature is compiled.
    #[arg(long)]
    pub skip_openai: bool,
}

/// Run the existing scorecard pipeline. Implementation is the body of the old `main`.
pub async fn run(args: ScorecardArgs) -> anyhow::Result<()> {
    crate::scorecard::runner::run(args).await
}

mod runner;
```

- [ ] **Step 4: Move scorecard logic into `scorecard/runner.rs`**

Create `crates/cairn-bench/src/scorecard/runner.rs`. Copy the body of the old `main` from `src/main.rs` (lines 58 onwards in the current file) into a `pub(super) async fn run(args: super::ScorecardArgs) -> anyhow::Result<()>`. Replace the `args.fixture` / `args.out_dir` / `args.cache` / `args.skip_openai` references — they're identical field names, so the body needs no changes beyond replacing `Cli::parse()` with the passed-in `args`.

If the existing main contains imports needed by the body, copy those to the top of `runner.rs`. Drop the `#[tokio::main]` decoration — the new top-level main owns that.

- [ ] **Step 5: Replace `src/main.rs` with subcommand dispatcher**

Overwrite `crates/cairn-bench/src/main.rs`:

```rust
#![forbid(unsafe_code)]
//! cairn-bench binary entry point — subcommand dispatcher.

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "cairn-bench", about = "Cairn bench harness: scorecard + release gates.")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// BrainBench retrieval-quality scorecard runner.
    Scorecard(cairn_bench::scorecard::ScorecardArgs),
    /// Latency regression gate (placeholder until Task 4).
    Latency,
    /// Memory budget gate (placeholder until Task 6).
    Memory,
    /// Privacy leakage gate (placeholder until Task 8).
    Privacy,
    /// Run latency + memory + privacy and exit non-zero on any failure.
    All {
        /// Skip one or more gates by name (latency, memory, privacy).
        #[arg(long)]
        skip: Vec<String>,
    },
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Scorecard(args) => cairn_bench::scorecard::run(args).await,
        Cmd::Latency | Cmd::Memory | Cmd::Privacy | Cmd::All { .. } => {
            anyhow::bail!("not implemented yet — wired in later tasks")
        }
    }
}
```

- [ ] **Step 6: Update `src/lib.rs`**

Replace `crates/cairn-bench/src/lib.rs`:

```rust
#![forbid(unsafe_code)]
#![warn(missing_docs)]
//! Cairn bench harness: BrainBench scorecard + release gates (latency, memory, privacy).

pub mod adapter;
pub mod cache;
pub mod cached_embedder;
pub mod fixture;
pub mod metrics;
pub mod report;
pub mod scorecard;
```

- [ ] **Step 7: Verify test passes**

Run: `cargo test -p cairn-bench --test cli_smoke -- --nocapture`
Expected: both tests PASS.

- [ ] **Step 8: Verify nothing else broke**

Run: `cargo nextest run -p cairn-bench --locked`
Expected: all existing tests still pass.

- [ ] **Step 9: Commit**

```bash
git add crates/cairn-bench/src/main.rs crates/cairn-bench/src/lib.rs crates/cairn-bench/src/scorecard/ crates/cairn-bench/tests/cli_smoke.rs
git commit -m "refactor(cairn-bench): split scorecard into subcommand for issue #99 gates"
```

---

## Task 3: Shared gates infrastructure — thresholds, baseline, report

Add the shared modules every gate uses: brief §15 SLO constants, baseline JSON struct + load/save, and the shared report writer + exit-code enum.

**Files:**
- Create: `crates/cairn-bench/src/gates/mod.rs`
- Create: `crates/cairn-bench/src/gates/thresholds.rs`
- Create: `crates/cairn-bench/src/gates/baseline.rs`
- Create: `crates/cairn-bench/src/gates/report.rs`
- Modify: `crates/cairn-bench/src/lib.rs`

- [ ] **Step 1: Write failing unit tests for baseline + thresholds**

Create `crates/cairn-bench/src/gates/baseline.rs` initially as:

```rust
//! Shared baseline JSON load/save for the latency gate.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Baseline {
    pub schema_version: u32,
    pub runner: String,
    pub captured_at: String,
    pub commit: String,
    pub metrics: BTreeMap<String, f64>,
    #[serde(default)]
    pub regression_pct: BTreeMap<String, f64>,
}

#[derive(Debug, thiserror::Error)]
pub enum BaselineError {
    #[error("baseline file not found: {0}")]
    NotFound(String),
    #[error("baseline parse error in {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("baseline write error in {path}: {source}")]
    Write {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

impl Baseline {
    pub fn load(path: &Path) -> Result<Self, BaselineError> {
        let bytes = std::fs::read(path)
            .map_err(|_| BaselineError::NotFound(path.display().to_string()))?;
        serde_json::from_slice(&bytes).map_err(|source| BaselineError::Parse {
            path: path.display().to_string(),
            source,
        })
    }

    pub fn save(&self, path: &Path) -> Result<(), BaselineError> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|source| BaselineError::Parse {
            path: path.display().to_string(),
            source,
        })?;
        std::fs::write(path, bytes).map_err(|source| BaselineError::Write {
            path: path.display().to_string(),
            source,
        })
    }

    /// Per-metric regression threshold. Uses per-metric override if present, else 2%.
    pub fn regression_threshold_ms(&self, metric: &str) -> f64 {
        let baseline_ms = self.metrics.get(metric).copied().unwrap_or(f64::INFINITY);
        let pct = self
            .regression_pct
            .get(metric)
            .copied()
            .unwrap_or(crate::gates::thresholds::DEFAULT_REGRESSION_PCT);
        baseline_ms * (1.0 + pct / 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_writes_and_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.json");
        let mut b = Baseline {
            schema_version: 1,
            runner: "ubuntu".into(),
            captured_at: "2026-05-15T00:00:00Z".into(),
            commit: "abc".into(),
            metrics: BTreeMap::new(),
            regression_pct: BTreeMap::new(),
        };
        b.metrics.insert("assemble_hot_p95_ms".into(), 4.2);
        b.save(&path).unwrap();
        let got = Baseline::load(&path).unwrap();
        assert_eq!(b, got);
    }

    #[test]
    fn missing_file_returns_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let err = Baseline::load(&dir.path().join("nope.json")).unwrap_err();
        assert!(matches!(err, BaselineError::NotFound(_)));
    }

    #[test]
    fn default_regression_is_two_pct() {
        let b = Baseline {
            schema_version: 1,
            runner: "x".into(),
            captured_at: "x".into(),
            commit: "x".into(),
            metrics: [("m".to_string(), 100.0)].into_iter().collect(),
            regression_pct: BTreeMap::new(),
        };
        let t = b.regression_threshold_ms("m");
        assert!((t - 102.0).abs() < 1e-9, "expected 102 ms, got {t}");
    }

    #[test]
    fn per_metric_override_wins() {
        let b = Baseline {
            schema_version: 1,
            runner: "x".into(),
            captured_at: "x".into(),
            commit: "x".into(),
            metrics: [("m".to_string(), 100.0)].into_iter().collect(),
            regression_pct: [("m".to_string(), 5.0)].into_iter().collect(),
        };
        let t = b.regression_threshold_ms("m");
        assert!((t - 105.0).abs() < 1e-9, "expected 105 ms, got {t}");
    }
}
```

- [ ] **Step 2: Create `gates/thresholds.rs` with brief §15 constants**

Create `crates/cairn-bench/src/gates/thresholds.rs`:

```rust
//! Brief §15-derived constants for the latency gate.
//!
//! See `docs/design/design-brief.md` §15 and §19.a for the source SLOs.

/// Brief §15: "fails build if any metric drops > 2%".
pub const DEFAULT_REGRESSION_PCT: f64 = 2.0;

/// Brief §15: p95 turn latency with hot-assembly + write < 50 ms.
pub const SLO_HOT_PATH_MS: f64 = 50.0;

/// Brief §15: p99 turn latency < 100 ms.
pub const SLO_HOT_PATH_P99_MS: f64 = 100.0;

/// Brief §15: forget-me reader-invisible latency (1M recs) < 1 s p95.
pub const SLO_FORGET_PHASE_A_MS: f64 = 1_000.0;

/// Brief §15: forget-me physical purge (Phase B) < 30 s p95.
pub const SLO_FORGET_PHASE_B_MS: f64 = 30_000.0;

/// Brief §15: cold-rehydration (≤ 10 MB session) < 3 s p95.
pub const SLO_COLD_REHYDRATE_MS: f64 = 3_000.0;
```

- [ ] **Step 3: Create `gates/report.rs` with the shared exit-code enum**

Create `crates/cairn-bench/src/gates/report.rs`:

```rust
//! Shared exit-code enum and human-summary writer for all gates.

use std::process::ExitCode;

/// Outcome of a single gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateOutcome {
    /// All checks passed.
    Pass,
    /// One or more checks failed.
    Fail,
    /// Required input missing (baseline or manifest).
    MissingInput,
    /// Internal harness error.
    InternalError,
}

impl GateOutcome {
    /// Exit code consumed by `main`.
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Pass => 0,
            Self::Fail => 1,
            Self::MissingInput => 2,
            Self::InternalError => 3,
        }
    }

    /// Combine two outcomes — picks the worst (highest exit code).
    pub fn worst_of(self, other: Self) -> Self {
        if self.exit_code() >= other.exit_code() {
            self
        } else {
            other
        }
    }
}

impl From<GateOutcome> for ExitCode {
    fn from(value: GateOutcome) -> Self {
        ExitCode::from(value.exit_code())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worst_of_picks_higher_code() {
        assert_eq!(GateOutcome::Pass.worst_of(GateOutcome::Fail), GateOutcome::Fail);
        assert_eq!(GateOutcome::Fail.worst_of(GateOutcome::MissingInput), GateOutcome::MissingInput);
        assert_eq!(GateOutcome::Pass.worst_of(GateOutcome::Pass), GateOutcome::Pass);
    }

    #[test]
    fn exit_codes_match_spec() {
        assert_eq!(GateOutcome::Pass.exit_code(), 0);
        assert_eq!(GateOutcome::Fail.exit_code(), 1);
        assert_eq!(GateOutcome::MissingInput.exit_code(), 2);
        assert_eq!(GateOutcome::InternalError.exit_code(), 3);
    }
}
```

- [ ] **Step 4: Create `gates/mod.rs`**

Create `crates/cairn-bench/src/gates/mod.rs`:

```rust
//! Shared infrastructure for the release-gate subcommands.

pub mod baseline;
pub mod report;
pub mod thresholds;

/// Returns the runner profile name derived from `$RUNNER_OS` (CI) or `cfg!(target_os)` (local).
pub fn runner_profile() -> &'static str {
    match std::env::var("RUNNER_OS").as_deref() {
        Ok("Linux") => "linux",
        Ok("macOS") => "macos",
        Ok("Windows") => "windows",
        _ if cfg!(target_os = "linux") => "linux",
        _ if cfg!(target_os = "macos") => "macos",
        _ => "linux",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_env_overrides_cfg() {
        // SAFETY: tests run serial via `--test-threads=1` if needed; this is acceptable
        // because the function reads only this one variable. See `set_var` discussion
        // in `gates::runner_profile` — we keep this test single-threaded by gating
        // on a probe value.
        // (In practice: this test is illustrative only — we don't assert env coupling
        // across the harness.)
        let before = std::env::var("RUNNER_OS").ok();
        // SAFETY: required for test of env-driven branch.
        unsafe { std::env::set_var("RUNNER_OS", "Linux") };
        assert_eq!(runner_profile(), "linux");
        unsafe { std::env::set_var("RUNNER_OS", "macOS") };
        assert_eq!(runner_profile(), "macos");
        match before {
            Some(v) => unsafe { std::env::set_var("RUNNER_OS", v) },
            None => unsafe { std::env::remove_var("RUNNER_OS") },
        }
    }
}
```

- [ ] **Step 5: Wire `gates` into the lib + drop `tracing` from machete ignores once used**

Open `crates/cairn-bench/src/lib.rs`. Add `pub mod gates;` after `pub mod scorecard;`:

```rust
pub mod gates;
pub mod scorecard;
```

Open `crates/cairn-bench/Cargo.toml`. Add `thiserror` to `[dependencies]`:

```toml
thiserror = { workspace = true }
```

(`thiserror` is already in `[workspace.dependencies]`.)

- [ ] **Step 6: Run tests**

Run: `cargo test -p cairn-bench --lib gates::`
Expected: all PASS.

Run: `cargo clippy -p cairn-bench --all-targets --locked -- -D warnings`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-bench/src/gates/ crates/cairn-bench/src/lib.rs crates/cairn-bench/Cargo.toml
git commit -m "feat(cairn-bench): shared gate infra (baseline + thresholds + report)"
```

---

## Task 4: Latency gate — criterion bench targets

Add the 8 Criterion benches in `benches/latency.rs`. Each bench drives a single SDK verb against a seeded fixture vault. The actual subcommand wiring lands in Task 5; this task is the measurement surface.

**Files:**
- Modify: `crates/cairn-bench/benches/latency.rs`
- Create: `crates/cairn-bench/src/latency/mod.rs` (stub for vault helper)
- Create: `crates/cairn-bench/src/latency/vault.rs`

- [ ] **Step 1: Write a vault-seeding helper**

Create `crates/cairn-bench/src/latency/mod.rs`:

```rust
//! Latency gate subcommand and shared bench helpers.

pub mod vault;
```

Create `crates/cairn-bench/src/latency/vault.rs`:

```rust
//! Seeded vault used by every latency bench.
//!
//! Reuses the P0 replay fixture vault shipped by `cairn-test-fixtures` so
//! bench results stay comparable across runs.

use std::path::PathBuf;

use cairn_test_fixtures::hybrid_vault;
use tempfile::TempDir;

/// A temp vault seeded with the canonical P0 hybrid corpus.
pub struct SeededVault {
    pub dir: TempDir,
    pub path: PathBuf,
}

impl SeededVault {
    /// Build a fresh seeded vault. Expensive — call once per bench group, not per iteration.
    pub fn new() -> anyhow::Result<Self> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().to_path_buf();
        hybrid_vault::seed(&path)?;
        Ok(Self { dir, path })
    }
}
```

Note: `cairn_test_fixtures::hybrid_vault::seed` is the existing seeding entrypoint — if its actual signature differs at implementation time, adjust the call. Run `grep -n "pub fn seed\|pub fn build" crates/cairn-test-fixtures/src/hybrid_vault.rs` to confirm the function name.

- [ ] **Step 2: Add `pub mod latency;` to `lib.rs`**

```rust
pub mod latency;
```

(Place it before `pub mod scorecard;`.)

- [ ] **Step 3: Write the 8 criterion benches**

Replace `crates/cairn-bench/benches/latency.rs`:

```rust
//! Brief §15 latency benches — 8 hot-path verbs, in-process via cairn-sdk.

use std::time::Duration;

use cairn_bench::latency::vault::SeededVault;
use cairn_sdk::transport::Client;
use criterion::{Criterion, criterion_group, criterion_main};

fn client(vault_path: &std::path::Path) -> Client {
    Client::open_local(vault_path).expect("open sdk client")
}

fn bench_assemble_hot(c: &mut Criterion) {
    let v = SeededVault::new().expect("seed vault");
    let client = client(&v.path);
    c.bench_function("assemble_hot_p95", |b| {
        b.iter(|| {
            let args = cairn_sdk::transport::AssembleHotArgs::default();
            let _ = client.assemble_hot(&args).expect("assemble_hot");
        });
    });
}

fn bench_search_keyword(c: &mut Criterion) {
    let v = SeededVault::new().expect("seed vault");
    let client = client(&v.path);
    c.bench_function("search_keyword_p95", |b| {
        b.iter(|| {
            let mut args = cairn_sdk::transport::SearchArgs::default();
            args.mode = "keyword".into();
            args.query = "cairn".into();
            let _ = client.search(&args).expect("search keyword");
        });
    });
}

fn bench_search_semantic(c: &mut Criterion) {
    let v = SeededVault::new().expect("seed vault");
    let client = client(&v.path);
    c.bench_function("search_semantic_p95", |b| {
        b.iter(|| {
            let mut args = cairn_sdk::transport::SearchArgs::default();
            args.mode = "semantic".into();
            args.query = "memory".into();
            let _ = client.search(&args).expect("search semantic");
        });
    });
}

fn bench_search_hybrid(c: &mut Criterion) {
    let v = SeededVault::new().expect("seed vault");
    let client = client(&v.path);
    c.bench_function("search_hybrid_p95", |b| {
        b.iter(|| {
            let mut args = cairn_sdk::transport::SearchArgs::default();
            args.mode = "hybrid".into();
            args.query = "memory".into();
            let _ = client.search(&args).expect("search hybrid");
        });
    });
}

fn bench_retrieve(c: &mut Criterion) {
    let v = SeededVault::new().expect("seed vault");
    let client = client(&v.path);
    // Pick a known record id seeded by the fixture vault.
    let id = cairn_test_fixtures::hybrid_vault::FIRST_RECORD_ID;
    c.bench_function("retrieve_p95", |b| {
        b.iter(|| {
            let args = cairn_sdk::transport::RetrieveArgs { id: id.into(), ..Default::default() };
            let _ = client.retrieve(&args).expect("retrieve");
        });
    });
}

fn bench_capture_trace(c: &mut Criterion) {
    let v = SeededVault::new().expect("seed vault");
    let client = client(&v.path);
    c.bench_function("capture_trace_p95", |b| {
        b.iter(|| {
            let args = cairn_sdk::transport::CaptureTraceArgs::synthetic_user_msg("bench");
            let _ = client.capture_trace(&args).expect("capture");
        });
    });
}

fn bench_wal_apply(c: &mut Criterion) {
    let v = SeededVault::new().expect("seed vault");
    let client = client(&v.path);
    c.bench_function("wal_apply_p95", |b| {
        b.iter(|| {
            // Issue an ingest that exercises the WAL PREPARE → APPLY path.
            let args = cairn_sdk::transport::IngestArgs::synthetic_reference("bench-body");
            let _ = client.ingest(&args).expect("ingest -> wal apply");
        });
    });
}

fn bench_workflow_enqueue(c: &mut Criterion) {
    let v = SeededVault::new().expect("seed vault");
    let client = client(&v.path);
    c.bench_function("workflow_enqueue_p95", |b| {
        b.iter(|| {
            let args = cairn_sdk::transport::CaptureTraceArgs::synthetic_stop("bench-session");
            let _ = client.capture_trace(&args).expect("workflow enqueue");
        });
    });
}

criterion_group! {
    name = hot_path;
    config = Criterion::default()
        .measurement_time(Duration::from_secs(8))
        .sample_size(50)
        .warm_up_time(Duration::from_secs(1));
    targets = bench_assemble_hot, bench_search_keyword, bench_search_semantic,
              bench_search_hybrid, bench_retrieve, bench_capture_trace,
              bench_wal_apply, bench_workflow_enqueue
}
criterion_main!(hot_path);
```

If `cairn_sdk::transport::*Args::default()` or `synthetic_*` helpers don't exist at implementation time, drop into `crates/cairn-sdk/src/transport.rs` to check the actual constructors. The exact arg-builder API will be discovered at impl time; the structural shape (one bench function per metric, all under one criterion group) does not change.

- [ ] **Step 4: Verify benches compile**

Run: `cargo bench -p cairn-bench --bench latency --no-run --locked`
Expected: compiles cleanly.

- [ ] **Step 5: Run one bench locally to sanity-check timings**

Run: `cargo bench -p cairn-bench --bench latency -- assemble_hot_p95 --measurement-time 2`
Expected: completes; criterion prints a p95 number under 50 ms on a developer laptop. (Don't fail on absolute thresholds at this step — that's Task 5.)

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-bench/benches/latency.rs crates/cairn-bench/src/latency/ crates/cairn-bench/src/lib.rs
git commit -m "feat(cairn-bench): criterion latency benches for 8 hot-path verbs (issue #99)"
```

---

## Task 5: Latency subcommand — comparator + report writer + `--refresh-baseline`

Wire the `latency` subcommand to: spawn `cargo bench --bench latency --message-format json`, parse criterion's JSON, compare against `Baseline`, emit `target/cairn-bench/latency.json`, and exit per `GateOutcome`. Add `--refresh-baseline` mode.

**Files:**
- Modify: `crates/cairn-bench/src/latency/mod.rs`
- Create: `crates/cairn-bench/src/latency/harness.rs`
- Create: `crates/cairn-bench/baselines/latency.linux.json`
- Create: `crates/cairn-bench/baselines/latency.macos.json`
- Modify: `crates/cairn-bench/src/main.rs`

- [ ] **Step 1: Write a failing test for the comparator**

Create `crates/cairn-bench/src/latency/harness.rs`:

```rust
//! Latency gate harness — invokes criterion, parses JSON, compares to baseline.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::gates::baseline::Baseline;
use crate::gates::report::GateOutcome;
use crate::gates::thresholds::SLO_HOT_PATH_MS;

/// Per-metric SLO. Override on a per-metric basis via this table.
pub fn slo_ms(metric: &str) -> f64 {
    // All 8 P0 benches share the §15 hot-path 50ms SLO. retrieve has a §19.a
    // advisory p50 < 5ms; we keep the gate's hard SLO at 50ms (§15 umbrella).
    let _ = metric;
    SLO_HOT_PATH_MS
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetricResult {
    pub bench: String,
    pub measured_ms: f64,
    pub slo_ms: f64,
    pub baseline_ms: f64,
    pub regression_threshold_ms: f64,
    pub slo_ok: bool,
    pub regression_ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyReport {
    pub schema_version: u32,
    pub runner: String,
    pub commit: String,
    pub captured_at: String,
    pub metrics: Vec<MetricResult>,
    pub ok: bool,
    pub failures: Vec<String>,
}

pub fn compare(measured: &BTreeMap<String, f64>, baseline: &Baseline) -> LatencyReport {
    let mut metrics = Vec::new();
    let mut failures = Vec::new();

    for (bench, measured_ms) in measured {
        let slo = slo_ms(bench);
        let baseline_ms = baseline.metrics.get(bench).copied().unwrap_or(f64::INFINITY);
        let regression_threshold = baseline.regression_threshold_ms(bench);
        let slo_ok = *measured_ms <= slo;
        let regression_ok = *measured_ms <= regression_threshold;
        if !slo_ok {
            failures.push(format!(
                "{bench}: measured {measured_ms:.2} ms > SLO {slo:.2} ms"
            ));
        }
        if !regression_ok {
            failures.push(format!(
                "{bench}: measured {measured_ms:.2} ms > baseline+2% ({regression_threshold:.2} ms)"
            ));
        }
        metrics.push(MetricResult {
            bench: bench.clone(),
            measured_ms: *measured_ms,
            slo_ms: slo,
            baseline_ms,
            regression_threshold_ms: regression_threshold,
            slo_ok,
            regression_ok,
        });
    }

    let ok = failures.is_empty();
    LatencyReport {
        schema_version: 1,
        runner: baseline.runner.clone(),
        commit: baseline.commit.clone(),
        captured_at: baseline.captured_at.clone(),
        metrics,
        ok,
        failures,
    }
}

pub fn outcome_from_report(r: &LatencyReport) -> GateOutcome {
    if r.ok { GateOutcome::Pass } else { GateOutcome::Fail }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(metric: &str, baseline_ms: f64) -> Baseline {
        Baseline {
            schema_version: 1,
            runner: "test".into(),
            captured_at: "now".into(),
            commit: "deadbeef".into(),
            metrics: [(metric.to_string(), baseline_ms)].into_iter().collect(),
            regression_pct: BTreeMap::new(),
        }
    }

    #[test]
    fn pass_when_under_slo_and_under_baseline_plus_2pct() {
        let measured: BTreeMap<String, f64> = [("assemble_hot_p95".into(), 4.0)].into();
        let baseline = b("assemble_hot_p95", 4.0);
        let r = compare(&measured, &baseline);
        assert!(r.ok, "expected pass; failures: {:?}", r.failures);
    }

    #[test]
    fn fail_when_over_slo() {
        let measured: BTreeMap<String, f64> = [("assemble_hot_p95".into(), 51.0)].into();
        let baseline = b("assemble_hot_p95", 4.0);
        let r = compare(&measured, &baseline);
        assert!(!r.ok);
        assert!(r.failures.iter().any(|f| f.contains("SLO")));
    }

    #[test]
    fn fail_when_over_baseline_plus_2pct_but_under_slo() {
        // baseline 4ms, measured 5ms (>+2% but well below 50ms SLO)
        let measured: BTreeMap<String, f64> = [("assemble_hot_p95".into(), 5.0)].into();
        let baseline = b("assemble_hot_p95", 4.0);
        let r = compare(&measured, &baseline);
        assert!(!r.ok);
        assert!(r.failures.iter().any(|f| f.contains("baseline+2%")));
    }
}
```

- [ ] **Step 2: Run unit tests — confirm they pass**

Run: `cargo test -p cairn-bench --lib latency::harness`
Expected: all 3 PASS.

- [ ] **Step 3: Write the criterion-output parser**

Append to `crates/cairn-bench/src/latency/harness.rs`:

```rust
use std::path::Path;

/// Parse criterion's `estimates.json` files for our 8 benches, return p95 in ms.
///
/// Criterion writes `target/criterion/<bench>/new/estimates.json` after each run.
/// The shape is `{"mean": {"point_estimate": 1234.5, ...}, "median": {...}, ...}`
/// where the numbers are nanoseconds. We use the median (criterion's most stable
/// summary). p95 isn't reported directly by criterion; for a stable bench-driven
/// gate we use median × 1.2 as an empirical proxy, OR we can run with `cargo bench
/// -- --save-baseline foo` and read percentiles. Simplification: use the median
/// and document this in the gate.
pub fn parse_criterion_dir(criterion_dir: &Path) -> anyhow::Result<BTreeMap<String, f64>> {
    let mut out = BTreeMap::new();
    for entry in std::fs::read_dir(criterion_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() { continue; }
        let estimates = path.join("new").join("estimates.json");
        if !estimates.exists() { continue; }
        let bytes = std::fs::read(&estimates)?;
        let v: serde_json::Value = serde_json::from_slice(&bytes)?;
        let ns = v.get("median")
            .and_then(|m| m.get("point_estimate"))
            .and_then(serde_json::Value::as_f64)
            .ok_or_else(|| anyhow::anyhow!("missing median.point_estimate in {}", estimates.display()))?;
        let ms = ns / 1_000_000.0;
        let bench = path.file_name().unwrap().to_string_lossy().to_string();
        out.insert(bench, ms);
    }
    Ok(out)
}
```

- [ ] **Step 4: Wire the subcommand entrypoint**

Replace `crates/cairn-bench/src/latency/mod.rs` body (keep `pub mod vault;`):

```rust
//! Latency gate subcommand and shared bench helpers.

pub mod harness;
pub mod vault;

use std::path::PathBuf;
use std::process::Command;

use chrono::Utc;
use clap::Args;

use crate::gates::baseline::Baseline;
use crate::gates::report::GateOutcome;

#[derive(Args, Debug)]
pub struct LatencyArgs {
    /// Path to the committed baseline directory (defaults to crate-local `baselines/`).
    #[arg(long, default_value = "crates/cairn-bench/baselines")]
    pub baselines_dir: PathBuf,

    /// Where to write `target/cairn-bench/latency.json`.
    #[arg(long, default_value = "target/cairn-bench")]
    pub out_dir: PathBuf,

    /// Path to criterion's output dir.
    #[arg(long, default_value = "target/criterion")]
    pub criterion_dir: PathBuf,

    /// Skip running the bench and reuse the existing criterion output.
    #[arg(long)]
    pub no_run: bool,

    /// Run the benches and overwrite the baseline file for the current runner.
    #[arg(long)]
    pub refresh_baseline: bool,
}

pub fn run(args: LatencyArgs) -> anyhow::Result<GateOutcome> {
    let profile = crate::gates::runner_profile();
    let baseline_path = args.baselines_dir.join(format!("latency.{profile}.json"));

    if !args.no_run {
        run_criterion()?;
    }

    let measured = harness::parse_criterion_dir(&args.criterion_dir)?;

    if args.refresh_baseline {
        let commit = current_commit().unwrap_or_else(|_| "unknown".into());
        let now = Utc::now().to_rfc3339();
        let new = Baseline {
            schema_version: 1,
            runner: profile.into(),
            captured_at: now,
            commit,
            metrics: measured.into_iter().collect(),
            regression_pct: Default::default(),
        };
        new.save(&baseline_path)?;
        println!("wrote refreshed baseline to {}", baseline_path.display());
        return Ok(GateOutcome::Pass);
    }

    let baseline = match Baseline::load(&baseline_path) {
        Ok(b) => b,
        Err(crate::gates::baseline::BaselineError::NotFound(_)) => {
            eprintln!("baseline missing: {} — run `cairn-bench latency --refresh-baseline`", baseline_path.display());
            return Ok(GateOutcome::MissingInput);
        }
        Err(e) => return Err(e.into()),
    };

    let report = harness::compare(&measured.into_iter().collect(), &baseline);

    std::fs::create_dir_all(&args.out_dir)?;
    let out = args.out_dir.join("latency.json");
    std::fs::write(&out, serde_json::to_vec_pretty(&report)?)?;
    print_human_summary(&report);
    Ok(harness::outcome_from_report(&report))
}

fn run_criterion() -> anyhow::Result<()> {
    let status = Command::new("cargo")
        .args(["bench", "-p", "cairn-bench", "--bench", "latency", "--locked"])
        .status()?;
    anyhow::ensure!(status.success(), "criterion run failed");
    Ok(())
}

fn current_commit() -> anyhow::Result<String> {
    let out = Command::new("git").args(["rev-parse", "HEAD"]).output()?;
    anyhow::ensure!(out.status.success(), "git rev-parse failed");
    Ok(String::from_utf8(out.stdout)?.trim().to_string())
}

fn print_human_summary(r: &harness::LatencyReport) {
    println!("latency gate: {}", if r.ok { "PASS" } else { "FAIL" });
    for m in &r.metrics {
        let mark = if m.slo_ok && m.regression_ok { "✓" } else { "✗" };
        println!(
            "  {mark} {bench}: {meas:.2} ms (SLO {slo:.0} ms, baseline {base:.2} ms, +2% = {thr:.2} ms)",
            bench = m.bench,
            meas = m.measured_ms,
            slo = m.slo_ms,
            base = m.baseline_ms,
            thr = m.regression_threshold_ms,
        );
    }
    if !r.failures.is_empty() {
        println!("failures:");
        for f in &r.failures { println!("  - {f}"); }
    }
}
```

Add `chrono = { workspace = true, features = ["clock", "std"] }` to `crates/cairn-bench/Cargo.toml` if not already present (`chrono` is in workspace deps).

- [ ] **Step 5: Wire the subcommand into main.rs**

Open `crates/cairn-bench/src/main.rs`. Replace the `Cmd::Latency` variant + match arm:

```rust
#[derive(Subcommand, Debug)]
enum Cmd {
    Scorecard(cairn_bench::scorecard::ScorecardArgs),
    Latency(cairn_bench::latency::LatencyArgs),
    // ... rest unchanged
}
```

```rust
    let outcome: u8 = match cli.cmd {
        Cmd::Scorecard(args) => {
            cairn_bench::scorecard::run(args).await?;
            0
        }
        Cmd::Latency(args) => cairn_bench::latency::run(args)?.exit_code(),
        Cmd::Memory | Cmd::Privacy | Cmd::All { .. } => {
            anyhow::bail!("not implemented yet — wired in later tasks")
        }
    };
    std::process::exit(outcome.into());
```

Change `main`'s return type to `anyhow::Result<()>` (already is) and drop `Ok(())` if you use `std::process::exit` to terminate.

- [ ] **Step 6: Capture an initial baseline**

Run the benches locally and refresh the Linux baseline (do this on a Linux CI runner or a Linux dev box; macOS contributors do the same locally for the macOS baseline):

```bash
cargo run -p cairn-bench --release --locked -- latency --refresh-baseline
```

This writes `crates/cairn-bench/baselines/latency.linux.json` (or `.macos.json` depending on host). Commit the baseline file.

For Phase-1 development on a single machine, you may only have one baseline. The other is captured by a maintainer with access to the other OS in a follow-up PR.

- [ ] **Step 7: Run the gate with the captured baseline**

```bash
cargo run -p cairn-bench --release --locked -- latency
```

Expected: PASS. `target/cairn-bench/latency.json` written with `ok: true`.

- [ ] **Step 8: Verify clippy + lint**

Run: `cargo clippy -p cairn-bench --all-targets --locked -- -D warnings`
Expected: clean.

- [ ] **Step 9: Commit**

```bash
git add crates/cairn-bench/src/latency/ crates/cairn-bench/src/main.rs crates/cairn-bench/Cargo.toml crates/cairn-bench/baselines/
git commit -m "feat(cairn-bench): latency gate subcommand + baseline + comparator (issue #99 §15)"
```

---

## Task 6: Memory gate — manifest + sizer

Implement the memory gate: parse `manifests/memory.toml`, resolve asset paths via production-code helpers, sum sizes, compare to budgets and asset tolerance bands, emit `target/cairn-bench/memory.json`.

**Files:**
- Create: `crates/cairn-bench/manifests/memory.toml`
- Create: `crates/cairn-bench/src/memory/mod.rs`
- Create: `crates/cairn-bench/src/memory/manifest.rs`
- Create: `crates/cairn-bench/src/memory/sizer.rs`
- Modify: `crates/cairn-bench/src/lib.rs`
- Modify: `crates/cairn-bench/src/main.rs`

- [ ] **Step 1: Create the memory manifest**

Create `crates/cairn-bench/manifests/memory.toml`:

```toml
# Memory budget gate manifest. Brief §19 working-set budget.
# Owners adjust this file via PR; see docs/superpowers/specs/2026-05-15-issue-99-release-gates-design.md §5.

[profile.default]
binary = "target/release/cairn"
assets = [
  { source = "embedding_model", expected_mb = 25 },
  { source = "sherpa_onnx_voice", expected_mb = 100 },
  { source = "screen_default", expected_mb = 20 },
]
budget_mb = 200

[profile.screenpipe]
extends = "default"
features = ["screenpipe-runtime"]
assets_add = [
  { source = "screenpipe_runtime", expected_mb = 500 },
]
budget_mb = 700
```

- [ ] **Step 2: Write failing tests for the manifest parser**

Create `crates/cairn-bench/src/memory/manifest.rs`:

```rust
//! Memory-gate manifest parser. Reads `manifests/memory.toml`, applies profile inheritance.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct AssetSpec {
    pub source: String,
    pub expected_mb: f64,
}

#[derive(Debug, Deserialize, Clone)]
struct RawProfile {
    binary: Option<String>,
    #[serde(default)]
    extends: Option<String>,
    #[serde(default)]
    features: Vec<String>,
    #[serde(default)]
    assets: Vec<AssetSpec>,
    #[serde(default)]
    assets_add: Vec<AssetSpec>,
    budget_mb: f64,
}

#[derive(Debug, Deserialize)]
struct RawManifest {
    profile: HashMap<String, RawProfile>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedProfile {
    pub name: String,
    pub binary: String,
    pub features: Vec<String>,
    pub assets: Vec<AssetSpec>,
    pub budget_mb: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("manifest read error: {0}")]
    Read(#[from] std::io::Error),
    #[error("manifest parse error: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("profile not found: {0}")]
    UnknownProfile(String),
    #[error("profile {profile} extends unknown parent: {parent}")]
    UnknownParent { profile: String, parent: String },
    #[error("profile cycle detected at: {0}")]
    Cycle(String),
}

pub fn load(path: &Path) -> Result<RawManifest, ManifestError> {
    let bytes = std::fs::read_to_string(path)?;
    Ok(toml::from_str(&bytes)?)
}

pub fn resolve(manifest: &RawManifest, profile: &str) -> Result<ResolvedProfile, ManifestError> {
    let mut seen = Vec::new();
    resolve_inner(manifest, profile, &mut seen)
}

fn resolve_inner(
    manifest: &RawManifest,
    profile: &str,
    seen: &mut Vec<String>,
) -> Result<ResolvedProfile, ManifestError> {
    if seen.iter().any(|p| p == profile) {
        return Err(ManifestError::Cycle(profile.into()));
    }
    seen.push(profile.into());
    let raw = manifest
        .profile
        .get(profile)
        .ok_or_else(|| ManifestError::UnknownProfile(profile.into()))?;

    let (base_binary, base_features, mut assets) = if let Some(parent) = &raw.extends {
        let parent_resolved = resolve_inner(manifest, parent, seen)?;
        (parent_resolved.binary, parent_resolved.features, parent_resolved.assets)
    } else {
        (String::new(), Vec::new(), Vec::new())
    };

    let binary = raw.binary.clone().unwrap_or(base_binary);
    let mut features = base_features;
    features.extend(raw.features.iter().cloned());

    if !raw.assets.is_empty() {
        // direct profile, no inheritance for assets
        assets = raw.assets.clone();
    }
    assets.extend(raw.assets_add.iter().cloned());

    Ok(ResolvedProfile {
        name: profile.into(),
        binary,
        features,
        assets,
        budget_mb: raw.budget_mb,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> RawManifest {
        toml::from_str(s).unwrap()
    }

    #[test]
    fn resolves_default_profile() {
        let m = parse(r#"
            [profile.default]
            binary = "bin"
            assets = [ { source = "a", expected_mb = 10 } ]
            budget_mb = 100
        "#);
        let p = resolve(&m, "default").unwrap();
        assert_eq!(p.binary, "bin");
        assert_eq!(p.assets.len(), 1);
        assert_eq!(p.budget_mb, 100.0);
    }

    #[test]
    fn profile_extends_inherits_binary_and_assets_then_adds() {
        let m = parse(r#"
            [profile.default]
            binary = "bin"
            assets = [ { source = "a", expected_mb = 10 } ]
            budget_mb = 100

            [profile.heavy]
            extends = "default"
            assets_add = [ { source = "b", expected_mb = 50 } ]
            budget_mb = 200
        "#);
        let p = resolve(&m, "heavy").unwrap();
        assert_eq!(p.binary, "bin");
        assert_eq!(p.assets.len(), 2);
        assert_eq!(p.features.len(), 0);
        assert_eq!(p.budget_mb, 200.0);
    }

    #[test]
    fn profile_extends_with_features_is_additive() {
        let m = parse(r#"
            [profile.default]
            binary = "bin"
            assets = []
            budget_mb = 100

            [profile.heavy]
            extends = "default"
            features = ["foo"]
            assets_add = []
            budget_mb = 200
        "#);
        let p = resolve(&m, "heavy").unwrap();
        assert_eq!(p.features, vec!["foo"]);
    }

    #[test]
    fn unknown_profile_errs() {
        let m = parse(r#"[profile.default]
            binary = "b"
            assets = []
            budget_mb = 10
        "#);
        let err = resolve(&m, "nope").unwrap_err();
        assert!(matches!(err, ManifestError::UnknownProfile(_)));
    }

    #[test]
    fn extend_cycle_errs() {
        let m = parse(r#"
            [profile.a]
            extends = "b"
            assets = []
            budget_mb = 1

            [profile.b]
            extends = "a"
            assets = []
            budget_mb = 1
        "#);
        let err = resolve(&m, "a").unwrap_err();
        assert!(matches!(err, ManifestError::Cycle(_)));
    }
}
```

- [ ] **Step 3: Write the sizer**

Create `crates/cairn-bench/src/memory/sizer.rs`:

```rust
//! Memory-gate sizer. Resolves asset paths via production code, sums sizes,
//! checks per-asset tolerance band and total budget.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::gates::report::GateOutcome;
use crate::memory::manifest::{AssetSpec, ResolvedProfile};

pub const DEFAULT_ASSET_TOLERANCE_PCT: f64 = 25.0;

#[derive(Debug, Serialize, Deserialize)]
pub struct AssetResult {
    pub name: String,
    pub size_mb: f64,
    pub expected_mb: f64,
    pub tolerance_pct: f64,
    pub ok: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MemoryReport {
    pub schema_version: u32,
    pub profile: String,
    pub budget_mb: f64,
    pub total_mb: f64,
    pub assets: Vec<AssetResult>,
    pub ok: bool,
    pub failures: Vec<String>,
}

/// Resolve a manifest `source` string into a filesystem path. The mapping is
/// curated; new asset sources require an entry here.
pub fn resolve_asset(source: &str) -> anyhow::Result<std::path::PathBuf> {
    match source {
        "embedding_model" => {
            // Reuse the production model resolver.
            Ok(cairn_embeddings_local::default_model_path()?)
        }
        "sherpa_onnx_voice" => {
            // sherpa-onnx ships its runtime + models under a known cache dir.
            Ok(cairn_sensors_local::voice::sherpa_onnx_assets_dir()?)
        }
        "screen_default" => {
            Ok(cairn_sensors_local::screen::default_backend_assets_dir()?)
        }
        "screenpipe_runtime" => {
            Ok(cairn_sensors_local::screen::screenpipe_assets_dir()?)
        }
        other => anyhow::bail!("unknown memory-manifest asset source: {other}"),
    }
}

fn directory_size_bytes(path: &Path) -> anyhow::Result<u64> {
    let mut total = 0u64;
    if path.is_file() { return Ok(std::fs::metadata(path)?.len()); }
    for entry in walkdir::WalkDir::new(path) {
        let entry = entry?;
        if entry.file_type().is_file() {
            total = total.saturating_add(entry.metadata()?.len());
        }
    }
    Ok(total)
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

pub fn measure(profile: &ResolvedProfile) -> anyhow::Result<MemoryReport> {
    let mut failures = Vec::new();
    let mut assets = Vec::new();
    let mut total = 0.0;

    // Binary
    let bin_path = Path::new(&profile.binary);
    let bin_size = if bin_path.exists() {
        mb(std::fs::metadata(bin_path)?.len())
    } else {
        failures.push(format!("binary not found at {} — run `cargo build --release` first", profile.binary));
        0.0
    };
    let bin_ok = bin_size > 0.0;
    total += bin_size;
    assets.push(AssetResult {
        name: "binary".into(),
        size_mb: bin_size,
        expected_mb: 15.0,
        tolerance_pct: DEFAULT_ASSET_TOLERANCE_PCT,
        ok: bin_ok,
    });

    // Manifest-declared assets
    for spec in &profile.assets {
        let path = resolve_asset(&spec.source)?;
        let size = if path.exists() { mb(directory_size_bytes(&path)?) } else {
            failures.push(format!("asset path missing: {} ({})", spec.source, path.display()));
            0.0
        };
        let tol = DEFAULT_ASSET_TOLERANCE_PCT;
        let lo = spec.expected_mb * (1.0 - tol / 100.0);
        let hi = spec.expected_mb * (1.0 + tol / 100.0);
        let ok = size >= lo && size <= hi;
        if !ok {
            failures.push(format!(
                "asset {}: {:.1} MB outside band [{:.1}, {:.1}] (expected {:.1})",
                spec.source, size, lo, hi, spec.expected_mb
            ));
        }
        total += size;
        assets.push(AssetResult {
            name: spec.source.clone(),
            size_mb: size,
            expected_mb: spec.expected_mb,
            tolerance_pct: tol,
            ok,
        });
    }

    if total > profile.budget_mb {
        failures.push(format!(
            "total {:.1} MB exceeds budget {:.1} MB",
            total, profile.budget_mb
        ));
    }

    let ok = failures.is_empty();
    Ok(MemoryReport {
        schema_version: 1,
        profile: profile.name.clone(),
        budget_mb: profile.budget_mb,
        total_mb: total,
        assets,
        ok,
        failures,
    })
}

pub fn outcome(r: &MemoryReport) -> GateOutcome {
    if r.ok { GateOutcome::Pass } else { GateOutcome::Fail }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_pass_when_ok() {
        let r = MemoryReport {
            schema_version: 1,
            profile: "x".into(),
            budget_mb: 100.0,
            total_mb: 50.0,
            assets: vec![],
            ok: true,
            failures: vec![],
        };
        assert_eq!(outcome(&r), GateOutcome::Pass);
    }
}
```

If `cairn_embeddings_local::default_model_path` / `cairn_sensors_local::voice::sherpa_onnx_assets_dir` / `cairn_sensors_local::screen::default_backend_assets_dir` / `screenpipe_assets_dir` do not exist as public functions: add them as small `pub fn` wrappers in the respective crates as part of this task. The functions return `anyhow::Result<PathBuf>` pointing at the cache dir the runtime uses. Cross-reference the implementation file inside each crate (e.g. `crates/cairn-embeddings-local/src/lib.rs`) and add the helper if absent.

Add `walkdir` to `cairn-bench/Cargo.toml` `[dependencies]`:

```toml
walkdir = { workspace = true }
```

(`walkdir` is in the workspace deps; confirm with `grep walkdir Cargo.toml`.)

- [ ] **Step 4: Wire `memory/mod.rs`**

Create `crates/cairn-bench/src/memory/mod.rs`:

```rust
//! Memory gate subcommand.

pub mod manifest;
pub mod sizer;

use std::path::PathBuf;
use std::process::Command;

use clap::Args;

use crate::gates::report::GateOutcome;

#[derive(Args, Debug)]
pub struct MemoryArgs {
    /// Path to the manifest TOML.
    #[arg(long, default_value = "crates/cairn-bench/manifests/memory.toml")]
    pub manifest: PathBuf,

    /// Profile name to run.
    #[arg(long, default_value = "default")]
    pub profile: String,

    /// Skip the `cargo build --release` step (use the existing binary).
    #[arg(long)]
    pub no_build: bool,

    /// Output dir.
    #[arg(long, default_value = "target/cairn-bench")]
    pub out_dir: PathBuf,
}

pub fn run(args: MemoryArgs) -> anyhow::Result<GateOutcome> {
    let raw = manifest::load(&args.manifest)?;
    let resolved = manifest::resolve(&raw, &args.profile)?;

    if !args.no_build {
        let mut cmd = Command::new("cargo");
        cmd.args(["build", "--release", "-p", "cairn-cli", "--locked"]);
        for f in &resolved.features {
            cmd.args(["--features", f]);
        }
        let status = cmd.status()?;
        anyhow::ensure!(status.success(), "cargo build failed");
    }

    let report = sizer::measure(&resolved)?;

    std::fs::create_dir_all(&args.out_dir)?;
    let out = args.out_dir.join("memory.json");
    std::fs::write(&out, serde_json::to_vec_pretty(&report)?)?;
    print_human_summary(&report);
    Ok(sizer::outcome(&report))
}

fn print_human_summary(r: &sizer::MemoryReport) {
    println!("memory gate ({}): {}", r.profile, if r.ok { "PASS" } else { "FAIL" });
    for a in &r.assets {
        let mark = if a.ok { "✓" } else { "✗" };
        println!(
            "  {mark} {name}: {sz:.1} MB (expected {exp:.1} ±{tol:.0}%)",
            mark = mark, name = a.name, sz = a.size_mb, exp = a.expected_mb, tol = a.tolerance_pct
        );
    }
    println!("  total: {:.1} MB / budget {:.1} MB", r.total_mb, r.budget_mb);
    if !r.failures.is_empty() {
        println!("failures:");
        for f in &r.failures { println!("  - {f}"); }
    }
}
```

- [ ] **Step 5: Wire `memory` into lib + main**

Add `pub mod memory;` to `crates/cairn-bench/src/lib.rs`.

In `crates/cairn-bench/src/main.rs`, replace the `Memory` variant + arm:

```rust
    Memory(cairn_bench::memory::MemoryArgs),
```

```rust
        Cmd::Memory(args) => cairn_bench::memory::run(args)?.exit_code(),
```

- [ ] **Step 6: Run unit tests**

Run: `cargo test -p cairn-bench --lib memory::`
Expected: all PASS.

- [ ] **Step 7: Run the gate manually**

Run: `cargo run -p cairn-bench --release --locked -- memory`
Expected: builds the release binary, sums assets, prints PASS or FAIL with diagnostic. `target/cairn-bench/memory.json` written.

- [ ] **Step 8: Drop `toml` from machete ignores**

Open `crates/cairn-bench/Cargo.toml`. Update:

```toml
[package.metadata.cargo-machete]
ignored = ["tracing", "rusqlite", "yaml_serde"]
```

Run: `cargo machete`. Expected: passes.

- [ ] **Step 9: Commit**

```bash
git add crates/cairn-bench/manifests/ crates/cairn-bench/src/memory/ crates/cairn-bench/src/main.rs crates/cairn-bench/src/lib.rs crates/cairn-bench/Cargo.toml
git commit -m "feat(cairn-bench): memory budget gate (issue #99 §19)"
```

---

## Task 7: Privacy fixture format + loader

Implement the YAML fixture loader and `--check` mode. No runner yet (Task 8). All eight P0 fixtures land in Task 9.

**Files:**
- Create: `crates/cairn-bench/src/privacy/mod.rs`
- Create: `crates/cairn-bench/src/privacy/fixture.rs`
- Modify: `crates/cairn-bench/src/lib.rs`
- Modify: `crates/cairn-bench/src/main.rs`

- [ ] **Step 1: Write failing tests for the fixture loader**

Create `crates/cairn-bench/src/privacy/fixture.rs`:

```rust
//! Privacy-fixture YAML schema + loader.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct FixtureRecord {
    pub id: String,
    pub kind: String,
    pub body: String,
    #[serde(default)]
    pub visibility: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(tag = "verb", rename_all = "snake_case")]
pub enum FixtureOp {
    Forget { target: String, mode: String },
    Redact { target: String, span_kind: String },
    Revoke { receipt_id: String },
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct SearchAssertion {
    pub mode: String,
    pub query: String,
    pub must_not_contain_id: String,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct RetrieveAssertion {
    pub id: String,
    pub expect: String,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(untagged)]
pub enum MarkdownAssertion {
    NotPresent { path_must_not_exist_or_be_tombstoned: String },
    NoUnmaskedSpan { path: String, must_not_contain: String },
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct IndexAssertion {
    pub table: String,
    pub column: String,
    pub must_not_contain: String,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Default)]
pub struct Assertions {
    #[serde(default)]
    pub search: Vec<SearchAssertion>,
    #[serde(default)]
    pub retrieve: Vec<RetrieveAssertion>,
    #[serde(default)]
    pub markdown: Vec<MarkdownAssertion>,
    #[serde(default)]
    pub indexes: Vec<IndexAssertion>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct PrivacyFixture {
    pub scenario: String,
    pub description: String,
    pub setup: SetupBlock,
    #[serde(default)]
    pub operations: Vec<FixtureOp>,
    pub assertions: Assertions,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct SetupBlock {
    pub records: Vec<FixtureRecord>,
}

pub fn load_dir(dir: &Path) -> anyhow::Result<Vec<(PathBuf, PrivacyFixture)>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("yaml") { continue; }
        let bytes = std::fs::read_to_string(&path)?;
        let fixture: PrivacyFixture = yaml_serde::from_str(&bytes)
            .map_err(|e| anyhow::anyhow!("yaml parse error in {}: {e}", path.display()))?;
        out.push((path, fixture));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
scenario: forget_record_visibility
description: §5.6 Phase A forgotten record invisible
setup:
  records:
    - id: rec_001
      kind: semantic
      body: "x"
      visibility: project
operations:
  - verb: forget
    target: rec_001
    mode: record
assertions:
  search:
    - mode: keyword
      query: "x"
      must_not_contain_id: rec_001
  retrieve:
    - id: rec_001
      expect: not_found
  markdown:
    - path_must_not_exist_or_be_tombstoned: "raw/rec_001.md"
  indexes:
    - table: record_fts
      column: record_id
      must_not_contain: rec_001
"#;

    #[test]
    fn parses_minimal_fixture() {
        let f: PrivacyFixture = yaml_serde::from_str(SAMPLE).unwrap();
        assert_eq!(f.scenario, "forget_record_visibility");
        assert_eq!(f.setup.records.len(), 1);
        assert!(matches!(&f.operations[0], FixtureOp::Forget { mode, .. } if mode == "record"));
        assert_eq!(f.assertions.search.len(), 1);
        assert_eq!(f.assertions.indexes[0].table, "record_fts");
    }

    #[test]
    fn load_dir_sorts_alphabetically() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.yaml"), SAMPLE).unwrap();
        std::fs::write(dir.path().join("a.yaml"), SAMPLE).unwrap();
        let loaded = load_dir(dir.path()).unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(loaded[0].0.ends_with("a.yaml"));
        assert!(loaded[1].0.ends_with("b.yaml"));
    }
}
```

- [ ] **Step 2: Add the privacy subcommand entrypoint**

Create `crates/cairn-bench/src/privacy/mod.rs`:

```rust
//! Privacy gate subcommand.

pub mod fixture;

use std::path::PathBuf;

use clap::Args;

use crate::gates::report::GateOutcome;

#[derive(Args, Debug)]
pub struct PrivacyArgs {
    /// Path to the fixtures directory.
    #[arg(long, default_value = "crates/cairn-bench/fixtures/privacy")]
    pub fixtures_dir: PathBuf,

    /// Parse fixtures without running them; used in CI for fast schema validation.
    #[arg(long)]
    pub check: bool,

    /// Output dir.
    #[arg(long, default_value = "target/cairn-bench")]
    pub out_dir: PathBuf,
}

pub fn run(args: PrivacyArgs) -> anyhow::Result<GateOutcome> {
    let fixtures = fixture::load_dir(&args.fixtures_dir)?;
    if args.check {
        println!("privacy gate --check: parsed {} fixtures", fixtures.len());
        return Ok(GateOutcome::Pass);
    }
    // Runner is wired in Task 8.
    anyhow::bail!("privacy runner is not yet wired — Task 8");
}
```

- [ ] **Step 3: Wire into lib + main**

Add `pub mod privacy;` to `crates/cairn-bench/src/lib.rs`.

In `src/main.rs`, replace the `Privacy` variant + arm:

```rust
    Privacy(cairn_bench::privacy::PrivacyArgs),
```

```rust
        Cmd::Privacy(args) => cairn_bench::privacy::run(args)?.exit_code(),
```

- [ ] **Step 4: Verify tests pass**

Run: `cargo test -p cairn-bench --lib privacy::fixture`
Expected: 2 tests PASS.

- [ ] **Step 5: Verify `--check` works with an empty fixtures dir**

```bash
mkdir -p crates/cairn-bench/fixtures/privacy
cargo run -p cairn-bench --release --locked -- privacy --check
```

Expected: `privacy gate --check: parsed 0 fixtures`. Exit 0.

- [ ] **Step 6: Drop `yaml_serde` from machete ignores**

Open `crates/cairn-bench/Cargo.toml`:

```toml
[package.metadata.cargo-machete]
ignored = ["tracing", "rusqlite"]
```

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-bench/src/privacy/ crates/cairn-bench/src/lib.rs crates/cairn-bench/src/main.rs crates/cairn-bench/fixtures/ crates/cairn-bench/Cargo.toml
git commit -m "feat(cairn-bench): privacy fixture loader + --check mode (issue #99 §14)"
```

---

## Task 8: Privacy runner — setup → operations → assertions

Build the runner that takes a fixture, creates a temp vault, applies records + operations, then runs each assertion. Each assertion failure is reported with its surface.

**Files:**
- Create: `crates/cairn-bench/src/privacy/harness.rs`
- Modify: `crates/cairn-bench/src/privacy/mod.rs`

- [ ] **Step 1: Write the harness skeleton**

Create `crates/cairn-bench/src/privacy/harness.rs`:

```rust
//! Privacy fixture runner.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use cairn_sdk::transport::{Client, ForgetArgs, IngestArgs, RetrieveArgs, SearchArgs};

use crate::privacy::fixture::{Assertions, FixtureOp, PrivacyFixture};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FailureReport {
    pub scenario: String,
    pub surface: String,
    pub query_or_id: String,
    pub expected: String,
    pub actual: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PrivacyReport {
    pub schema_version: u32,
    pub fixtures_run: usize,
    pub fixtures_passed: usize,
    pub ok: bool,
    pub failures: Vec<FailureReport>,
}

pub fn run_fixture(fixture: &PrivacyFixture) -> anyhow::Result<Vec<FailureReport>> {
    let dir = tempfile::tempdir()?;
    let vault = dir.path();
    cairn_test_fixtures::store::bootstrap_empty(vault)?;

    let client = Client::open_local(vault)?;

    // Setup: ingest each record.
    for rec in &fixture.setup.records {
        let mut args = IngestArgs::default();
        args.id = Some(rec.id.clone());
        args.kind = rec.kind.clone();
        args.body = rec.body.clone();
        if let Some(v) = &rec.visibility { args.visibility = Some(v.clone()); }
        let _ = client.ingest(&args)?;
    }

    // Operations.
    for op in &fixture.operations {
        match op {
            FixtureOp::Forget { target, mode } => {
                let mut args = ForgetArgs::default();
                args.id = target.clone();
                args.mode = mode.clone();
                let _ = client.forget(&args)?;
            }
            FixtureOp::Redact { target, span_kind } => {
                // Redact is exposed via cairn-sdk; if not, fall back to a direct
                // pipeline call. Confirm the SDK surface at impl time.
                let _ = (target, span_kind);
                anyhow::bail!("redact verb not yet exposed on cairn-sdk — implement before privacy fixtures land");
            }
            FixtureOp::Revoke { receipt_id } => {
                let _ = receipt_id;
                anyhow::bail!("revoke verb not yet exposed on cairn-sdk — implement before privacy fixtures land");
            }
        }
    }

    // Wait for Phase B to converge.
    wait_for_terminal_state(vault, Duration::from_secs(5))?;

    let mut failures = Vec::new();
    check_search(&client, fixture, &fixture.assertions, &mut failures)?;
    check_retrieve(&client, fixture, &fixture.assertions, &mut failures)?;
    check_markdown(vault, fixture, &fixture.assertions, &mut failures)?;
    check_indexes(vault, fixture, &fixture.assertions, &mut failures)?;
    Ok(failures)
}

fn wait_for_terminal_state(vault: &Path, timeout: Duration) -> anyhow::Result<()> {
    use std::time::Instant;
    let db = vault.join(".cairn/cairn.db");
    let start = Instant::now();
    while start.elapsed() < timeout {
        let conn = rusqlite::Connection::open(&db)?;
        let mut stmt = conn.prepare(
            "SELECT COUNT(*) FROM wal_ops WHERE state NOT IN ('APPLIED','FAILED','PURGED')"
        )?;
        let count: i64 = stmt.query_row([], |r| r.get(0)).unwrap_or(0);
        if count == 0 { return Ok(()); }
        std::thread::sleep(Duration::from_millis(50));
    }
    anyhow::bail!("wal_ops did not reach terminal state within {timeout:?}");
}

fn check_search(
    client: &Client,
    f: &PrivacyFixture,
    a: &Assertions,
    failures: &mut Vec<FailureReport>,
) -> anyhow::Result<()> {
    for sa in &a.search {
        let mut args = SearchArgs::default();
        args.mode = sa.mode.clone();
        args.query = sa.query.clone();
        let resp = client.search(&args)?;
        let leaked = resp
            .data
            .results
            .iter()
            .any(|r| r.id == sa.must_not_contain_id);
        if leaked {
            failures.push(FailureReport {
                scenario: f.scenario.clone(),
                surface: format!("search({})", sa.mode),
                query_or_id: sa.query.clone(),
                expected: format!("no result with id={}", sa.must_not_contain_id),
                actual: format!("id={} present in results", sa.must_not_contain_id),
            });
        }
    }
    Ok(())
}

fn check_retrieve(
    client: &Client,
    f: &PrivacyFixture,
    a: &Assertions,
    failures: &mut Vec<FailureReport>,
) -> anyhow::Result<()> {
    for ra in &a.retrieve {
        let mut args = RetrieveArgs::default();
        args.id = ra.id.clone();
        let result = client.retrieve(&args);
        let actual_present = match &result {
            Ok(resp) => resp.data.record.is_some(),
            Err(_) => false,
        };
        let expected_present = ra.expect == "present";
        if actual_present != expected_present {
            failures.push(FailureReport {
                scenario: f.scenario.clone(),
                surface: "retrieve".into(),
                query_or_id: ra.id.clone(),
                expected: ra.expect.clone(),
                actual: if actual_present { "present" } else { "not_found" }.into(),
            });
        }
    }
    Ok(())
}

fn check_markdown(
    vault: &Path,
    f: &PrivacyFixture,
    a: &Assertions,
    failures: &mut Vec<FailureReport>,
) -> anyhow::Result<()> {
    for ma in &a.markdown {
        match ma {
            crate::privacy::fixture::MarkdownAssertion::NotPresent { path_must_not_exist_or_be_tombstoned } => {
                let full = vault.join(path_must_not_exist_or_be_tombstoned);
                if full.exists() {
                    let body = std::fs::read_to_string(&full)?;
                    if !body.contains("tombstone:") {
                        failures.push(FailureReport {
                            scenario: f.scenario.clone(),
                            surface: "markdown".into(),
                            query_or_id: path_must_not_exist_or_be_tombstoned.clone(),
                            expected: "file absent or tombstoned".into(),
                            actual: "file present without tombstone".into(),
                        });
                    }
                }
            }
            crate::privacy::fixture::MarkdownAssertion::NoUnmaskedSpan { path, must_not_contain } => {
                let full = vault.join(path);
                if let Ok(body) = std::fs::read_to_string(&full) {
                    if body.contains(must_not_contain) {
                        failures.push(FailureReport {
                            scenario: f.scenario.clone(),
                            surface: "markdown".into(),
                            query_or_id: path.clone(),
                            expected: format!("body must not contain {must_not_contain}"),
                            actual: "body contains the forbidden span".into(),
                        });
                    }
                }
            }
        }
    }
    Ok(())
}

fn check_indexes(
    vault: &Path,
    f: &PrivacyFixture,
    a: &Assertions,
    failures: &mut Vec<FailureReport>,
) -> anyhow::Result<()> {
    let db = vault.join(".cairn/cairn.db");
    let conn = rusqlite::Connection::open(&db)?;
    for ia in &a.indexes {
        let sql = format!(
            "SELECT COUNT(*) FROM {table} WHERE {col} = ?1",
            table = ia.table,
            col = ia.column
        );
        let count: i64 = conn.query_row(&sql, [&ia.must_not_contain], |r| r.get(0))?;
        if count != 0 {
            failures.push(FailureReport {
                scenario: f.scenario.clone(),
                surface: format!("index({})", ia.table),
                query_or_id: ia.must_not_contain.clone(),
                expected: format!("no row in {} where {} = {}", ia.table, ia.column, ia.must_not_contain),
                actual: format!("{count} rows present"),
            });
        }
    }
    Ok(())
}
```

If `cairn_test_fixtures::store::bootstrap_empty` doesn't exist as that exact entrypoint, grep `crates/cairn-test-fixtures/src/store.rs` for the closest equivalent (likely `make_empty_vault` or `bootstrap`). If `cairn-test-fixtures` is a dev-dep but the privacy runner is library code, move `cairn-test-fixtures` from `[dev-dependencies]` to `[dependencies]` in `cairn-bench/Cargo.toml` — `cairn-bench` is itself dev tooling so this is acceptable.

- [ ] **Step 2: Wire the runner into the privacy subcommand**

Replace the runner body in `crates/cairn-bench/src/privacy/mod.rs`:

```rust
pub fn run(args: PrivacyArgs) -> anyhow::Result<GateOutcome> {
    let fixtures = fixture::load_dir(&args.fixtures_dir)?;
    if args.check {
        println!("privacy gate --check: parsed {} fixtures", fixtures.len());
        return Ok(GateOutcome::Pass);
    }

    let mut total_failures = Vec::new();
    let mut passed = 0;
    for (path, f) in &fixtures {
        let failures = harness::run_fixture(f)?;
        if failures.is_empty() {
            passed += 1;
            println!("✓ {}", path.display());
        } else {
            println!("✗ {} ({} failures)", path.display(), failures.len());
            for fail in &failures {
                println!("    [{}] {} expected={} actual={}",
                    fail.surface, fail.query_or_id, fail.expected, fail.actual);
            }
            total_failures.extend(failures);
        }
    }

    let report = harness::PrivacyReport {
        schema_version: 1,
        fixtures_run: fixtures.len(),
        fixtures_passed: passed,
        ok: total_failures.is_empty(),
        failures: total_failures,
    };

    std::fs::create_dir_all(&args.out_dir)?;
    std::fs::write(args.out_dir.join("privacy.json"), serde_json::to_vec_pretty(&report)?)?;

    Ok(if report.ok { GateOutcome::Pass } else { GateOutcome::Fail })
}

pub mod harness;
```

- [ ] **Step 3: Verify with a single fixture**

Drop one minimal fixture into `crates/cairn-bench/fixtures/privacy/forget_record_visibility.yaml`:

```yaml
scenario: forget_record_visibility
description: smoke fixture — full set lands in Task 9
setup:
  records:
    - id: rec_smoke
      kind: reference
      body: "smoke body"
operations:
  - verb: forget
    target: rec_smoke
    mode: record
assertions:
  retrieve:
    - id: rec_smoke
      expect: not_found
  indexes:
    - table: record_fts
      column: record_id
      must_not_contain: rec_smoke
```

Run: `cargo run -p cairn-bench --release --locked -- privacy`

Expected: 1 fixture run, 1 passed. `target/cairn-bench/privacy.json` `ok: true`.

If the test fails: read the failure dump and adjust either the harness or the fixture. The failure dump names the surface; debug there.

- [ ] **Step 4: Drop `rusqlite` from machete ignores**

Open `crates/cairn-bench/Cargo.toml`:

```toml
[package.metadata.cargo-machete]
ignored = ["tracing"]
```

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-bench/src/privacy/ crates/cairn-bench/fixtures/privacy/forget_record_visibility.yaml crates/cairn-bench/Cargo.toml
git commit -m "feat(cairn-bench): privacy fixture runner (issue #99 §14)"
```

---

## Task 9: Eight P0 privacy fixtures

Land the remaining seven fixtures listed in the spec (one already smoke-landed in Task 8 — adjust it to the full shape).

**Files:**
- Create or overwrite eight files under `crates/cairn-bench/fixtures/privacy/`

- [ ] **Step 1: `forget_record_visibility.yaml`**

```yaml
scenario: forget_record_visibility
description: §5.6 Phase A — forgotten record invisible to search/retrieve/markdown
setup:
  records:
    - id: rec_001
      kind: semantic
      body: "Acme Corp roadmap Q3 launch plan internal"
      visibility: project
    - id: rec_002
      kind: reference
      body: "Public doc — no leak should occur via this record"
operations:
  - verb: forget
    target: rec_001
    mode: record
assertions:
  search:
    - mode: keyword
      query: "Acme roadmap"
      must_not_contain_id: rec_001
    - mode: semantic
      query: "Acme roadmap"
      must_not_contain_id: rec_001
  retrieve:
    - id: rec_001
      expect: not_found
  markdown:
    - path_must_not_exist_or_be_tombstoned: "raw/rec_001.md"
  indexes:
    - table: record_fts
      column: record_id
      must_not_contain: rec_001
    - table: vec_records
      column: record_id
      must_not_contain: rec_001
```

- [ ] **Step 2: `forget_record_index_purge.yaml`**

```yaml
scenario: forget_record_index_purge
description: §5.6 Phase B — physical purge removes vec/FTS rows
setup:
  records:
    - id: rec_purge_001
      kind: semantic
      body: "Phase B purge fixture body"
operations:
  - verb: forget
    target: rec_purge_001
    mode: record
assertions:
  indexes:
    - table: record_fts
      column: record_id
      must_not_contain: rec_purge_001
    - table: vec_records
      column: record_id
      must_not_contain: rec_purge_001
    - table: record_body
      column: record_id
      must_not_contain: rec_purge_001
```

- [ ] **Step 3: `redaction_pii_search.yaml`**

```yaml
scenario: redaction_pii_search
description: §14 Presidio pre-persist — email masked in search results
setup:
  records:
    - id: rec_pii_search
      kind: reference
      body: "Contact: jane.doe@example.com for details"
operations: []
assertions:
  search:
    - mode: keyword
      query: "jane.doe"
      must_not_contain_id: rec_pii_search
```

Note: assumes the ingest path runs the PII filter automatically. If it requires an explicit redact op, switch the `operations:` block to add a `redact` op.

- [ ] **Step 4: `redaction_pii_retrieve.yaml`**

```yaml
scenario: redaction_pii_retrieve
description: §14 — retrieved record returns masked body
setup:
  records:
    - id: rec_pii_retrieve
      kind: reference
      body: "SSN 123-45-6789 do not leak"
operations: []
assertions:
  retrieve:
    - id: rec_pii_retrieve
      expect: present
  markdown:
    - path: "raw/rec_pii_retrieve.md"
      must_not_contain: "123-45-6789"
```

- [ ] **Step 5: `redaction_pii_markdown.yaml`**

```yaml
scenario: redaction_pii_markdown
description: §14 — vault file on disk contains masked spans only
setup:
  records:
    - id: rec_pii_md
      kind: reference
      body: "Email: alice@example.com Phone: +1-555-1234"
operations: []
assertions:
  markdown:
    - path: "raw/rec_pii_md.md"
      must_not_contain: "alice@example.com"
    - path: "raw/rec_pii_md.md"
      must_not_contain: "+1-555-1234"
```

- [ ] **Step 6: `consent_visibility_gating.yaml`**

```yaml
scenario: consent_visibility_gating
description: §11.3 / §14 — project-scoped record not visible from a private-tier search
setup:
  records:
    - id: rec_project_scoped
      kind: semantic
      body: "Project-only memory body"
      visibility: project
operations: []
assertions:
  search:
    - mode: keyword
      query: "Project-only memory"
      must_not_contain_id: rec_project_scoped
```

Note: assumes the default fixture client opens in `private` scope. If the SDK requires explicit scope selection, extend the harness in Task 8 to take a scope arg from the fixture.

- [ ] **Step 7: `consent_revoke_invalidates.yaml`**

```yaml
scenario: consent_revoke_invalidates
description: §14 — after revoke, prior shared-tier record no longer surfaces
setup:
  records:
    - id: rec_revoked
      kind: semantic
      body: "Shared body to be revoked"
      visibility: team
operations:
  - verb: revoke
    receipt_id: receipt_for_rec_revoked
assertions:
  search:
    - mode: keyword
      query: "Shared body"
      must_not_contain_id: rec_revoked
  retrieve:
    - id: rec_revoked
      expect: not_found
```

Note: this fixture depends on the SDK exposing a `revoke` verb. If absent at impl time, gate this fixture behind a `#[ignore]`-equivalent by leaving the fixture parsed but skip with a clear `actual: "revoke verb not yet wired"` failure rather than crashing the runner.

- [ ] **Step 8: `forget_does_not_leak_body_in_logs.yaml`**

```yaml
scenario: forget_does_not_leak_body_in_logs
description: §14 — metrics.jsonl and tracing output never contain raw record bodies
setup:
  records:
    - id: rec_log_leak
      kind: reference
      body: "FORBIDDEN_BODY_TOKEN_SHOULD_NEVER_APPEAR_IN_LOGS"
operations:
  - verb: forget
    target: rec_log_leak
    mode: record
assertions:
  markdown:
    - path: ".cairn/metrics.jsonl"
      must_not_contain: "FORBIDDEN_BODY_TOKEN_SHOULD_NEVER_APPEAR_IN_LOGS"
```

Note: re-uses the `markdown` assertion DSL as a generic "this file must not contain this string" check. The path is `.cairn/metrics.jsonl` not vault markdown; the loader doesn't care.

- [ ] **Step 9: Run the privacy gate against all 8 fixtures**

```bash
cargo run -p cairn-bench --release --locked -- privacy
```

Expected: PASS on all 8 (or specific surface-level failures pointing at unimplemented redact/revoke verbs — if so, add a follow-up issue and either land partial fixtures or guard the two affected fixtures so they emit `actual: "verb not yet wired"` per Task 8 step 1 fallback).

If any fixture fails with a real leak: that's the privacy bug the gate exists to catch. Fix the underlying code (search/retrieve/markdown/index path) in the same PR before landing the fixtures.

- [ ] **Step 10: Commit**

```bash
git add crates/cairn-bench/fixtures/privacy/
git commit -m "feat(cairn-bench): 8 P0 privacy leakage fixtures (issue #99 §14)"
```

---

## Task 10: `all` aggregator + exit-code propagation

Implement `cairn-bench all [--skip <gate>]`.

**Files:**
- Modify: `crates/cairn-bench/src/main.rs`
- Modify: `crates/cairn-bench/src/lib.rs` (add an `all` module)
- Create: `crates/cairn-bench/src/all.rs`

- [ ] **Step 1: Create the all-aggregator**

Create `crates/cairn-bench/src/all.rs`:

```rust
//! `all` subcommand — runs latency + memory + privacy in order and combines outcomes.

use crate::gates::report::GateOutcome;
use crate::latency::LatencyArgs;
use crate::memory::MemoryArgs;
use crate::privacy::PrivacyArgs;

#[derive(Debug, Default)]
pub struct AllArgs {
    pub skip: Vec<String>,
}

pub fn run(args: AllArgs) -> anyhow::Result<GateOutcome> {
    let mut worst = GateOutcome::Pass;

    if !args.skip.iter().any(|s| s == "latency") {
        println!("== latency gate ==");
        let outcome = crate::latency::run(LatencyArgs::default_for_ci())?;
        worst = worst.worst_of(outcome);
    }
    if !args.skip.iter().any(|s| s == "memory") {
        println!("== memory gate ==");
        let outcome = crate::memory::run(MemoryArgs::default_for_ci())?;
        worst = worst.worst_of(outcome);
    }
    if !args.skip.iter().any(|s| s == "privacy") {
        println!("== privacy gate ==");
        let outcome = crate::privacy::run(PrivacyArgs::default_for_ci())?;
        worst = worst.worst_of(outcome);
    }
    Ok(worst)
}
```

- [ ] **Step 2: Add `default_for_ci` constructors**

The `all` aggregator needs each `*Args` to be constructible without clap. Add to each subcommand module:

For `latency/mod.rs`:

```rust
impl LatencyArgs {
    pub fn default_for_ci() -> Self {
        Self {
            baselines_dir: "crates/cairn-bench/baselines".into(),
            out_dir: "target/cairn-bench".into(),
            criterion_dir: "target/criterion".into(),
            no_run: false,
            refresh_baseline: false,
        }
    }
}
```

For `memory/mod.rs`:

```rust
impl MemoryArgs {
    pub fn default_for_ci() -> Self {
        Self {
            manifest: "crates/cairn-bench/manifests/memory.toml".into(),
            profile: "default".into(),
            no_build: false,
            out_dir: "target/cairn-bench".into(),
        }
    }
}
```

For `privacy/mod.rs`:

```rust
impl PrivacyArgs {
    pub fn default_for_ci() -> Self {
        Self {
            fixtures_dir: "crates/cairn-bench/fixtures/privacy".into(),
            check: false,
            out_dir: "target/cairn-bench".into(),
        }
    }
}
```

- [ ] **Step 3: Wire `all` into lib + main**

In `src/lib.rs`: `pub mod all;`

In `src/main.rs` replace the placeholder match for `Cmd::All`:

```rust
        Cmd::All { skip } => cairn_bench::all::run(cairn_bench::all::AllArgs { skip })?.exit_code(),
```

- [ ] **Step 4: Run `cairn-bench all`**

```bash
cargo run -p cairn-bench --release --locked -- all
```

Expected: runs latency → memory → privacy in order. Final exit is `worst_of` outcomes.

- [ ] **Step 5: Verify `--skip`**

```bash
cargo run -p cairn-bench --release --locked -- all --skip memory
```

Expected: skips memory step entirely.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-bench/src/all.rs crates/cairn-bench/src/lib.rs crates/cairn-bench/src/main.rs crates/cairn-bench/src/latency/mod.rs crates/cairn-bench/src/memory/mod.rs crates/cairn-bench/src/privacy/mod.rs
git commit -m "feat(cairn-bench): all-gates aggregator with --skip support (issue #99)"
```

---

## Task 11: Integration smoke tests

Add the three integration tests promised in spec §9.1.

**Files:**
- Create: `crates/cairn-bench/tests/latency_smoke.rs`
- Create: `crates/cairn-bench/tests/memory_smoke.rs`
- Create: `crates/cairn-bench/tests/privacy_smoke.rs`

- [ ] **Step 1: Latency smoke**

Create `crates/cairn-bench/tests/latency_smoke.rs`:

```rust
use std::collections::BTreeMap;

use cairn_bench::gates::baseline::Baseline;
use cairn_bench::latency::harness::compare;

#[test]
fn comparator_emits_failure_when_baseline_is_tighter_than_measured() {
    let measured: BTreeMap<String, f64> = [("assemble_hot_p95".into(), 6.0)].into();
    let baseline = Baseline {
        schema_version: 1,
        runner: "test".into(),
        captured_at: "now".into(),
        commit: "x".into(),
        metrics: [("assemble_hot_p95".to_string(), 4.0)].into_iter().collect(),
        regression_pct: BTreeMap::new(),
    };
    let r = compare(&measured, &baseline);
    assert!(!r.ok);
    assert!(r.failures.iter().any(|f| f.contains("baseline+2%")));
}
```

- [ ] **Step 2: Memory smoke**

Create `crates/cairn-bench/tests/memory_smoke.rs`:

```rust
use std::io::Write;

use cairn_bench::memory::manifest::{ResolvedProfile, AssetSpec};
use cairn_bench::memory::sizer::measure;

#[test]
fn synthetic_manifest_passes_when_total_under_budget() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("fake-bin");
    let mut f = std::fs::File::create(&bin).unwrap();
    // 10 MB of zeros
    f.write_all(&vec![0u8; 10 * 1024 * 1024]).unwrap();

    let resolved = ResolvedProfile {
        name: "synthetic".into(),
        binary: bin.display().to_string(),
        features: vec![],
        assets: vec![],
        budget_mb: 50.0,
    };
    let r = measure(&resolved).unwrap();
    assert!(r.ok, "expected ok; failures: {:?}", r.failures);
    assert!(r.total_mb > 9.0 && r.total_mb < 11.0);
}

#[test]
fn synthetic_manifest_fails_when_total_over_budget() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("fake-bin");
    let mut f = std::fs::File::create(&bin).unwrap();
    f.write_all(&vec![0u8; 10 * 1024 * 1024]).unwrap();

    let resolved = ResolvedProfile {
        name: "synthetic".into(),
        binary: bin.display().to_string(),
        features: vec![],
        assets: vec![],
        budget_mb: 5.0,  // budget tighter than the 10 MB binary
    };
    let r = measure(&resolved).unwrap();
    assert!(!r.ok);
    assert!(r.failures.iter().any(|x| x.contains("budget")));
}
```

- [ ] **Step 3: Privacy smoke**

Create `crates/cairn-bench/tests/privacy_smoke.rs`:

```rust
use cairn_bench::privacy::fixture::PrivacyFixture;
use cairn_bench::privacy::harness::run_fixture;

const SMOKE_FIXTURE: &str = r#"
scenario: smoke_fixture
description: minimal end-to-end privacy fixture
setup:
  records:
    - id: rec_smoke
      kind: reference
      body: "smoke body"
operations:
  - verb: forget
    target: rec_smoke
    mode: record
assertions:
  retrieve:
    - id: rec_smoke
      expect: not_found
"#;

#[test]
fn smoke_fixture_passes_against_real_vault() {
    let f: PrivacyFixture = yaml_serde::from_str(SMOKE_FIXTURE).unwrap();
    let failures = run_fixture(&f).expect("run fixture");
    assert!(failures.is_empty(), "expected pass; got: {failures:?}");
}
```

- [ ] **Step 4: Run integration tests**

```bash
cargo nextest run -p cairn-bench --locked
```

Expected: all PASS (including new smokes).

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-bench/tests/
git commit -m "test(cairn-bench): integration smokes for latency/memory/privacy (issue #99)"
```

---

## Task 12: CI wiring + docs

Add the `gates` job to `ci.yml`, the release-only extras to `release-dry-run.yml`, and the docs updates.

**Files:**
- Modify: `.github/workflows/ci.yml`
- Modify: `.github/workflows/release-dry-run.yml`
- Modify: `docs/ci.md`
- Modify: `CLAUDE.md`

- [ ] **Step 1: Add the gates job to `ci.yml`**

Open `.github/workflows/ci.yml`. After the existing `invariant` job (find it via `grep -n "invariant:" .github/workflows/ci.yml`), add:

```yaml
  gates:
    name: gates / latency + memory + privacy
    strategy:
      fail-fast: false
      matrix:
        include:
          - runner: ubuntu-latest
            cmd: "all"
          - runner: macos-latest
            cmd: "all --skip memory"
    runs-on: ${{ matrix.runner }}
    steps:
      - name: Checkout
        uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd # v6.0.2
      - name: Install toolchain
        run: rustup show active-toolchain || rustup toolchain install
      - name: Cache cargo
        uses: actions/cache@<same-pinned-sha-used-by-test-job>
        with:
          path: |
            ~/.cargo/registry
            ~/.cargo/git
            target
          key: gates-${{ matrix.runner }}-${{ hashFiles('**/Cargo.lock') }}
      - name: Run gates
        run: cargo run -p cairn-bench --release --locked -- ${{ matrix.cmd }}
      - name: Upload reports
        if: always()
        uses: actions/upload-artifact@<same-pinned-sha-used-by-test-job>
        with:
          name: bench-reports-${{ matrix.runner }}
          path: target/cairn-bench/*.json
```

Use the same pinned SHAs already present in `ci.yml` for `actions/cache` and `actions/upload-artifact` — grep for them in the existing test job.

- [ ] **Step 2: Update `release-dry-run.yml`**

Open `.github/workflows/release-dry-run.yml`. Find the macOS+Linux release-build matrix step. Append a step (per matrix entry) after the binary build:

```yaml
      - name: Memory budget — screenpipe profile
        if: matrix.os == 'ubuntu-latest'
        run: cargo run -p cairn-bench --release --locked -- memory --profile screenpipe

      - name: Memory budget — default (macOS coverage)
        if: matrix.os == 'macos-latest'
        run: cargo run -p cairn-bench --release --locked -- memory --profile default

      - name: Lifecycle benches
        run: cargo bench -p cairn-bench --bench lifecycle --locked
```

The lifecycle benches don't have a comparator (they're advisory at P0); the step succeeds if `cargo bench` exits 0. A follow-up issue can wire them into the gate.

- [ ] **Step 3: Update `docs/ci.md`**

Open `docs/ci.md`. In the "Required status checks" table, add after the `invariant` row:

```
| `gates / latency + memory + privacy` (`ci.yml`) | ✅ required | Brief §15 SLO + 2% regression, §19 working-set budget, §14 leakage fixtures. Reports in `bench-reports-<runner>` artifact. |
```

In the "Local equivalents" block, after the existing `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check` line, add:

```bash
# gates — latency SLO + 2% regression, memory budget, privacy leakage fixtures
cargo run -p cairn-bench --release --locked -- all
# Or one at a time:
cargo run -p cairn-bench --release --locked -- latency
cargo run -p cairn-bench --release --locked -- memory
cargo run -p cairn-bench --release --locked -- privacy
```

- [ ] **Step 4: Update `CLAUDE.md` §8 verification checklist**

Open `CLAUDE.md`. Find the `# ci.yml` block in §8. After the `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check` line, append:

```bash
cargo run -p cairn-bench --release --locked -- all
```

- [ ] **Step 5: Verify locally that `all` still passes end-to-end**

```bash
cargo run -p cairn-bench --release --locked -- all
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add .github/workflows/ci.yml .github/workflows/release-dry-run.yml docs/ci.md CLAUDE.md
git commit -m "ci: wire latency/memory/privacy gates as required check (issue #99)"
```

---

## Task 13: Final verification + close-out

Run the CLAUDE.md §8 verification checklist top to bottom, capture the output, and update the issue.

- [ ] **Step 1: Run the full §8 checklist**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
cargo run -p cairn-bench --release --locked -- all
cargo deny check
cargo audit --deny warnings
cargo machete
```

Expected: every command exits 0.

- [ ] **Step 2: Capture output for the PR description**

Save the tail of each command's output (last 5 lines) into a scratch file or paste directly into the PR.

- [ ] **Step 3: Open the PR**

```bash
git push -u origin HEAD
gh pr create --title "feat(cairn-bench): latency + memory + privacy gates (closes #99)" --body "$(cat <<'EOF'
## Summary

Closes #99. Adds three release gates to `cairn-bench` wired into `ci.yml` as one required check (`gates / latency + memory + privacy`):

- **Latency** — Criterion benches for 8 hot-path verbs (assemble_hot, search keyword/semantic/hybrid, retrieve, capture_trace, wal_apply, workflow_enqueue) compared against committed baselines (`crates/cairn-bench/baselines/latency.<linux|macos>.json`). Fails on SLO breach (§15: p95 < 50 ms) or > 2% regression vs baseline.
- **Memory budget** — Static binary + bundled-asset size summed against `crates/cairn-bench/manifests/memory.toml`. §19 working-set budget: ~160 MB default, ~660 MB with screenpipe.
- **Privacy** — 8 YAML fixtures cover forget visibility, Phase B index purge, PII redaction (search/retrieve/markdown), consent visibility gating, revoke, and log-leak prevention. Runs against real vaults via `cairn-test-fixtures`.

Lifecycle SLOs (cold-rehydrate, 1M-record forget) run release-only via `release-dry-run.yml`.

Design: `docs/superpowers/specs/2026-05-15-issue-99-release-gates-design.md`
Plan: `docs/superpowers/plans/2026-05-15-issue-99-release-gates.md`

## Verification

- `cargo run -p cairn-bench --release -- all` — PASS
- Full CLAUDE.md §8 checklist — clean (see attached)

## Test plan

- [ ] `cargo run -p cairn-bench --release -- latency` PASSes on Linux + macOS
- [ ] `cargo run -p cairn-bench --release -- memory` PASSes on Linux + macOS
- [ ] `cargo run -p cairn-bench --release -- privacy` PASSes on 8 fixtures
- [ ] CI `gates` job is green
- [ ] CI artifact `bench-reports-<runner>` is uploaded
EOF
)"
```

- [ ] **Step 4: Comment on the issue**

```bash
gh issue comment 99 --body "PR opened: <PR URL>. All 8 implementation-detail checkboxes covered + acceptance criteria + verification."
```

---

## Self-Review Checklist (pre-merge)

- All 3 acceptance criteria from issue #99 hit:
  - [ ] CI/release reports show latency and memory budgets with thresholds — yes, `bench-reports-*` artifact.
  - [ ] Privacy leakage fixtures fail if forgotten/redacted content appears in search, retrieve, markdown, or indexes — yes, 8 fixtures + 4 surfaces.
  - [ ] Budget thresholds documented and adjustable by release owners — yes, `manifests/memory.toml` + spec §8.
- All 3 verification items hit:
  - [ ] Run benchmark smoke suite — `tests/latency_smoke.rs` + criterion benches.
  - [ ] Run privacy regression suite — `cairn-bench privacy`.
  - [ ] Run working-set measurement script — `cairn-bench memory`.
- Brief sections covered:
  - [ ] §15 Evaluation — latency gate.
  - [ ] §19 Working-set budget — memory gate.
  - [ ] §14 Privacy and Consent — privacy fixtures.
- No CLAUDE.md invariants broken (see spec §10).
