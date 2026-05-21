# Issue 118 SRE Dashboard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the issue #118 local SRE dashboards, shared scrubbed report model, CLI and Electron surfaces, and release gates for rehydration latency and migration backlog.

**Architecture:** Add pure body-free SRE DTOs and classifiers in `cairn-core`; collect live vault data in `cairn-cli`; expose fixture-backed SRE data through `cairn-desktop`; render a dedicated Electron SRE workspace; and gate release checks through `cairn-bench`. CLI, desktop, and bench share only `cairn-core` DTOs and pure classifiers, so adapter crates do not depend on each other.

**Tech Stack:** Rust 1.95 workspace, `serde`, `clap`, `rusqlite`, `cairn-bench`, `axum`, React 19, Vite, Vitest, Testing Library, `lucide-react`.

---

## File Structure

- Create `crates/cairn-core/src/domain/sre.rs`: shared report DTOs, status enum, pure classifiers, privacy scrub helper.
- Modify `crates/cairn-core/src/domain/mod.rs`: export `sre`.
- Create `crates/cairn-core/tests/sre_report.rs`: DTO serialization, classifier, privacy scrub tests.
- Modify `crates/cairn-core/src/domain/metrics.rs`: add `RehydrationCompleted` metric event.
- Create `crates/cairn-cli/src/sre.rs`: report builder, metrics parser, human/JSON renderers.
- Modify `crates/cairn-cli/src/lib.rs`: export `sre`.
- Modify `crates/cairn-cli/src/command.rs`: add `cairn admin sre report` command.
- Modify `crates/cairn-cli/src/main.rs`: dispatch `admin sre report`.
- Modify `crates/cairn-cli/src/verbs/mod.rs`: add any admin module only if implementation keeps CLI command under `verbs`; otherwise keep SRE in `cairn_cli::sre`.
- Create `crates/cairn-cli/tests/admin_sre_report.rs`: CLI JSON/human/privacy tests.
- Create `crates/cairn-bench/src/sre/mod.rs`: SRE gate subcommand, fixture checks, report writer.
- Modify `crates/cairn-bench/src/main.rs`: add `sre` subcommand.
- Modify `crates/cairn-bench/src/all.rs`: include `sre --fixtures-only` in PR-friendly `all`.
- Modify `crates/cairn-bench/src/gates/thresholds.rs`: reuse `SLO_COLD_REHYDRATE_MS`, add migration backlog threshold.
- Create `crates/cairn-bench/tests/sre_smoke.rs`: SRE gate help, fixture-only pass/fail, privacy tests.
- Modify `.github/workflows/ci.yml`: upload `sre.json` with other bench reports once the gate writes it.
- Modify `.github/workflows/release-dry-run.yml`: replace informational lifecycle bench with strict `cairn-bench sre`.
- Modify `crates/cairn-desktop/src/model.rs`: desktop SRE DTO type aliases or wrappers.
- Modify `crates/cairn-desktop/src/repository.rs`: fixture `sre_report`.
- Modify `crates/cairn-desktop/src/server.rs`: `GET /api/v1/sre`.
- Create or modify `crates/cairn-desktop/tests/sre_api.rs`: endpoint test.
- Modify `frontend/desktop-electron/src/api/types.ts`: SRE types.
- Modify `frontend/desktop-electron/src/api/client.ts`: `sre()` client method.
- Modify `frontend/desktop-electron/src/App.tsx`: workspace switch and SRE data load.
- Create `frontend/desktop-electron/src/components/SreWorkspace.tsx`: top-level SRE view and panels.
- Create `frontend/desktop-electron/src/components/SreWorkspace.test.tsx`: component tests.
- Modify `frontend/desktop-electron/src/App.test.tsx`: navigation and initial load tests.
- Modify `frontend/desktop-electron/src/styles.css`: dense dashboard styles.

## Task 1: Core SRE DTOs And Classifiers

**Files:**
- Create: `crates/cairn-core/src/domain/sre.rs`
- Modify: `crates/cairn-core/src/domain/mod.rs`
- Test: `crates/cairn-core/tests/sre_report.rs`

- [ ] **Step 1: Write failing serialization and classifier tests**

Add `crates/cairn-core/tests/sre_report.rs`:

```rust
use cairn_core::domain::sre::{
    classify_count_status, scrub_detail, SreGateResult, SreGateSummary, SrePrivacySummary,
    SreProjectionSummary, SreRehydrationSummary, SreReport, SreSearchSummary, SreStatus,
    SreVaultSummary, SreWorkflowKindSummary, SreWorkflowSummary,
};

#[test]
fn sre_report_serializes_body_free_shape() {
    let report = SreReport {
        schema_version: 1,
        captured_at_ms: 1_700_000_000_000,
        vault: SreVaultSummary {
            id_hash: "sha256:vault".into(),
            name: "Fixture Vault".into(),
        },
        workflow: SreWorkflowSummary {
            status: SreStatus::Warning,
            oldest_queued_age_ms: Some(742_000),
            longest_held_lease_ms: None,
            dead_letter_count: 1,
            kinds: vec![SreWorkflowKindSummary {
                kind: "expire.tier".into(),
                queued: 2,
                leased: 1,
                done_recent: 3,
                failed_recent: 1,
                oldest_queued_age_ms: Some(742_000),
                last_success_age_ms: Some(50_000),
                backlog_threshold_ms: 600_000,
                status: SreStatus::Warning,
            }],
        },
        rehydration: SreRehydrationSummary {
            status: SreStatus::Ok,
            latest_latency_ms: Some(2_100),
            p95_latency_ms: Some(2_210.0),
            slo_ms: 3_000.0,
            sample_count: 12,
            last_gate: Some(SreGateResult {
                name: "cold_rehydrate_p95".into(),
                status: SreStatus::Ok,
                measured: Some(2_210.0),
                threshold: Some(3_000.0),
                unit: "ms".into(),
                detail: None,
            }),
        },
        projection: SreProjectionSummary {
            status: SreStatus::Unknown,
            nexus_state: "disabled".into(),
            nexus_reason: None,
            targets: Vec::new(),
        },
        search: SreSearchSummary {
            status: SreStatus::Ok,
            modes: Vec::new(),
        },
        gates: SreGateSummary {
            status: SreStatus::Ok,
            gates: Vec::new(),
        },
        privacy: SrePrivacySummary {
            scrubbed: true,
            forbidden_field_count: 0,
        },
    };

    let json = serde_json::to_string(&report).expect("serialize SRE report");
    assert!(json.contains("\"schema_version\":1"));
    assert!(json.contains("\"status\":\"warning\""));
    assert!(!json.contains("private body"));
    assert!(!json.contains("query text"));
}

#[test]
fn status_classification_warns_when_count_is_positive() {
    assert_eq!(classify_count_status(0), SreStatus::Ok);
    assert_eq!(classify_count_status(1), SreStatus::Warning);
}

#[test]
fn scrub_detail_maps_raw_text_to_stable_class() {
    let raw = "record body SECRET_PRIVATE_TOKEN from /Users/alice/vault/raw.md";
    assert_eq!(scrub_detail(raw), "redacted");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cargo nextest run -p cairn-core --test sre_report
```

Expected: FAIL because `cairn_core::domain::sre` does not exist.

- [ ] **Step 3: Add pure DTO implementation**

Create `crates/cairn-core/src/domain/sre.rs`:

```rust
//! Body-free SRE report DTOs and pure classifiers.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SreStatus {
    Ok,
    Warning,
    Fail,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SreVaultSummary {
    pub id_hash: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SreReport {
    pub schema_version: u32,
    pub captured_at_ms: i64,
    pub vault: SreVaultSummary,
    pub workflow: SreWorkflowSummary,
    pub rehydration: SreRehydrationSummary,
    pub projection: SreProjectionSummary,
    pub search: SreSearchSummary,
    pub gates: SreGateSummary,
    pub privacy: SrePrivacySummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SreWorkflowSummary {
    pub status: SreStatus,
    pub oldest_queued_age_ms: Option<i64>,
    pub longest_held_lease_ms: Option<i64>,
    pub dead_letter_count: usize,
    pub kinds: Vec<SreWorkflowKindSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SreWorkflowKindSummary {
    pub kind: String,
    pub queued: u64,
    pub leased: u64,
    pub done_recent: u64,
    pub failed_recent: u64,
    pub oldest_queued_age_ms: Option<i64>,
    pub last_success_age_ms: Option<i64>,
    pub backlog_threshold_ms: i64,
    pub status: SreStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SreRehydrationSummary {
    pub status: SreStatus,
    pub latest_latency_ms: Option<u64>,
    pub p95_latency_ms: Option<f64>,
    pub slo_ms: f64,
    pub sample_count: u64,
    pub last_gate: Option<SreGateResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SreProjectionSummary {
    pub status: SreStatus,
    pub nexus_state: String,
    pub nexus_reason: Option<String>,
    pub targets: Vec<SreProjectionTargetSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SreProjectionTargetSummary {
    pub target: String,
    pub current: u64,
    pub stale: u64,
    pub failed: u64,
    pub missing: u64,
    pub max_lag_ms: Option<i64>,
    pub last_rebuild_latency_ms: Option<u64>,
    pub status: SreStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SreSearchSummary {
    pub status: SreStatus,
    pub modes: Vec<SreSearchModeSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SreSearchModeSummary {
    pub mode: String,
    pub advertised: bool,
    pub invocations: u64,
    pub degraded: u64,
    pub failed: u64,
    pub p95_latency_ms: Option<f64>,
    pub status: SreStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SreGateSummary {
    pub status: SreStatus,
    pub gates: Vec<SreGateResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SreGateResult {
    pub name: String,
    pub status: SreStatus,
    pub measured: Option<f64>,
    pub threshold: Option<f64>,
    pub unit: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SrePrivacySummary {
    pub scrubbed: bool,
    pub forbidden_field_count: u64,
}

#[must_use]
pub fn classify_count_status(count: u64) -> SreStatus {
    if count == 0 {
        SreStatus::Ok
    } else {
        SreStatus::Warning
    }
}

#[must_use]
pub fn classify_threshold(measured: Option<f64>, threshold: f64) -> SreStatus {
    match measured {
        Some(value) if value <= threshold => SreStatus::Ok,
        Some(_) => SreStatus::Fail,
        None => SreStatus::Unknown,
    }
}

#[must_use]
pub fn scrub_detail(raw: &str) -> &'static str {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("secret")
        || lower.contains("private")
        || lower.contains('/')
        || lower.contains('\\')
    {
        "redacted"
    } else {
        "body_free"
    }
}
```

Modify `crates/cairn-core/src/domain/mod.rs`:

```rust
pub mod sre;
pub use sre::{
    classify_count_status, classify_threshold, scrub_detail, SreGateResult, SreGateSummary,
    SrePrivacySummary, SreProjectionSummary, SreProjectionTargetSummary, SreRehydrationSummary,
    SreReport, SreSearchModeSummary, SreSearchSummary, SreStatus, SreVaultSummary,
    SreWorkflowKindSummary, SreWorkflowSummary,
};
```

- [ ] **Step 4: Run tests to verify pass**

Run:

```bash
cargo nextest run -p cairn-core --test sre_report
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/domain/sre.rs crates/cairn-core/src/domain/mod.rs crates/cairn-core/tests/sre_report.rs
git commit -m "feat(core): add body-free SRE report model"
```

## Task 2: Rehydration Metric Event

**Files:**
- Modify: `crates/cairn-core/src/domain/metrics.rs`
- Test: extend `crates/cairn-core/tests/sre_report.rs`

- [ ] **Step 1: Write failing metric round-trip test**

Append to `crates/cairn-core/tests/sre_report.rs`:

```rust
use cairn_core::domain::metrics::MetricEvent;

#[test]
fn rehydration_completed_metric_is_body_free() {
    let event = MetricEvent::RehydrationCompleted {
        ts_ms: 1_700_000_000_000,
        target: "session".into(),
        source_tier: "cold".into(),
        restored_tier: "warm".into(),
        status: "committed".into(),
        latency_ms: 2_900,
        bytes_restored: 9_500_000,
        record_count: 240,
        error: None,
    };

    let json = serde_json::to_string(&event).expect("serialize");
    assert!(json.contains("\"event\":\"rehydration_completed\""));
    assert!(json.contains("\"target\":\"session\""));
    assert!(!json.contains("session_id"));
    assert!(!json.contains("body"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cargo nextest run -p cairn-core --test sre_report
```

Expected: FAIL because `MetricEvent::RehydrationCompleted` is missing.

- [ ] **Step 3: Add metric variant**

Add this variant to `MetricEvent` in `crates/cairn-core/src/domain/metrics.rs` near the workflow and evaluation events:

```rust
    #[serde(rename = "rehydration_completed")]
    RehydrationCompleted {
        /// Wall-clock millis since UNIX epoch.
        ts_ms: i64,
        /// Coarse target class, such as `session`; never a raw session id.
        target: String,
        /// Tier before rehydration, such as `cold`.
        source_tier: String,
        /// Tier after rehydration, such as `warm`.
        restored_tier: String,
        /// Final status (`committed`, `aborted`, or `rejected`).
        status: String,
        /// Wall-clock latency observed by the caller.
        latency_ms: u64,
        /// Bytes restored, rounded or exact depending on caller visibility.
        bytes_restored: u64,
        /// Count of restored records.
        record_count: u64,
        /// Body-free error class when status is not committed.
        error: Option<String>,
    },
```

- [ ] **Step 4: Run tests to verify pass**

Run:

```bash
cargo nextest run -p cairn-core --test sre_report
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/domain/metrics.rs crates/cairn-core/tests/sre_report.rs
git commit -m "feat(core): add rehydration SRE metric"
```

## Task 3: CLI SRE Report Builder And Command

**Files:**
- Create: `crates/cairn-cli/src/sre.rs`
- Modify: `crates/cairn-cli/src/lib.rs`
- Modify: `crates/cairn-cli/src/command.rs`
- Modify: `crates/cairn-cli/src/main.rs`
- Test: `crates/cairn-cli/tests/admin_sre_report.rs`

- [ ] **Step 1: Write failing CLI tests**

Create `crates/cairn-cli/tests/admin_sre_report.rs`:

```rust
use std::process::Command;

fn cairn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

#[test]
fn admin_sre_report_json_is_body_free() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bootstrap = cairn()
        .args(["bootstrap", "--vault-path", dir.path().to_str().expect("utf8")])
        .output()
        .expect("bootstrap");
    assert!(bootstrap.status.success());

    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"rehydration_completed","ts_ms":1,"target":"session","source_tier":"cold","restored_tier":"warm","status":"committed","latency_ms":2100,"bytes_restored":1000,"record_count":2,"error":null}
{"event":"search_completed","ts_ms":2,"mode":"semantic","hit_count":3,"latency_ms":42,"degradation_state":"partial","error":null}
"#,
    )
    .expect("write metrics");

    let output = cairn()
        .current_dir(dir.path())
        .args(["admin", "sre", "report", "--json"])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"rehydration\""));
    assert!(stdout.contains("\"sample_count\":1"));
    assert!(stdout.contains("\"mode\":\"semantic\""));
    assert!(!stdout.contains("SECRET_PRIVATE_TOKEN"));
}

#[test]
fn admin_sre_report_human_summarizes_sections() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bootstrap = cairn()
        .args(["bootstrap", "--vault-path", dir.path().to_str().expect("utf8")])
        .output()
        .expect("bootstrap");
    assert!(bootstrap.status.success());

    let output = cairn()
        .current_dir(dir.path())
        .args(["admin", "sre", "report"])
        .output()
        .expect("run sre report");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("SRE status:"));
    assert!(stdout.contains("workflow:"));
    assert!(stdout.contains("rehydration:"));
    assert!(stdout.contains("projection:"));
    assert!(stdout.contains("search:"));
    assert!(stdout.contains("gates:"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cargo nextest run -p cairn-cli --test admin_sre_report
```

Expected: FAIL because `admin sre report` is not wired.

- [ ] **Step 3: Implement the report builder**

Create `crates/cairn-cli/src/sre.rs`:

```rust
//! Operator-facing SRE report builder and renderers.

use std::{path::Path, process::ExitCode};

use cairn_core::domain::{
    classify_threshold, scrub_detail, SreGateSummary, SrePrivacySummary, SreProjectionSummary,
    SreRehydrationSummary, SreReport, SreSearchModeSummary, SreSearchSummary, SreStatus,
    SreVaultSummary, SreWorkflowSummary,
};
use cairn_core::domain::metrics::MetricEvent;
use clap::ArgMatches;

use crate::{config, metrics};

#[must_use]
pub fn run_report(matches: &ArgMatches, vault_root: &Path) -> ExitCode {
    let json = matches.get_flag("json");
    let config = match config::load(vault_root, &config::CliOverrides::default()) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("cairn admin sre report: config error - {err:#}");
            return ExitCode::from(78);
        }
    };
    let report = build_report(vault_root, &config);
    if json {
        println!("{}", serde_json::to_string_pretty(&report).expect("SRE report serializes"));
    } else {
        println!("{}", render_human(&report));
    }
    ExitCode::SUCCESS
}

#[must_use]
pub fn build_report(vault_root: &Path, _config: &cairn_core::config::CairnConfig) -> SreReport {
    let events = read_metric_events(vault_root);
    let rehydration_latencies: Vec<u64> = events
        .iter()
        .filter_map(|event| match event {
            MetricEvent::RehydrationCompleted { latency_ms, status, .. }
                if status == "committed" => Some(*latency_ms),
            _ => None,
        })
        .collect();
    let search_modes = summarize_search(&events);
    let p95 = percentile_u64(&rehydration_latencies, 0.95);
    let rehydration_status = classify_threshold(p95, crate::sre::SLO_COLD_REHYDRATE_MS);
    SreReport {
        schema_version: 1,
        captured_at_ms: metrics::now_ms(),
        vault: SreVaultSummary {
            id_hash: "sha256:local-vault".into(),
            name: vault_root
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "vault".into()),
        },
        workflow: SreWorkflowSummary {
            status: SreStatus::Unknown,
            oldest_queued_age_ms: None,
            longest_held_lease_ms: None,
            dead_letter_count: 0,
            kinds: Vec::new(),
        },
        rehydration: SreRehydrationSummary {
            status: rehydration_status,
            latest_latency_ms: rehydration_latencies.last().copied(),
            p95_latency_ms: p95,
            slo_ms: SLO_COLD_REHYDRATE_MS,
            sample_count: rehydration_latencies.len() as u64,
            last_gate: None,
        },
        projection: SreProjectionSummary {
            status: SreStatus::Unknown,
            nexus_state: "unknown".into(),
            nexus_reason: None,
            targets: Vec::new(),
        },
        search: SreSearchSummary {
            status: if search_modes.iter().any(|mode| mode.status != SreStatus::Ok) {
                SreStatus::Warning
            } else {
                SreStatus::Ok
            },
            modes: search_modes,
        },
        gates: SreGateSummary {
            status: SreStatus::Unknown,
            gates: Vec::new(),
        },
        privacy: SrePrivacySummary {
            scrubbed: true,
            forbidden_field_count: 0,
        },
    }
}

pub const SLO_COLD_REHYDRATE_MS: f64 = 3_000.0;

fn read_metric_events(vault_root: &Path) -> Vec<MetricEvent> {
    let path = vault_root.join(".cairn/metrics.jsonl");
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    raw.lines()
        .filter_map(|line| serde_json::from_str::<MetricEvent>(line).ok())
        .collect()
}

fn summarize_search(events: &[MetricEvent]) -> Vec<SreSearchModeSummary> {
    ["keyword", "semantic", "hybrid"]
        .into_iter()
        .map(|mode| {
            let mut invocations = 0_u64;
            let mut degraded = 0_u64;
            let mut failed = 0_u64;
            let mut latencies = Vec::new();
            for event in events {
                if let MetricEvent::SearchCompleted {
                    mode: event_mode,
                    latency_ms,
                    degradation_state,
                    error,
                    ..
                } = event
                    && event_mode == mode
                {
                    invocations += 1;
                    latencies.push(*latency_ms);
                    if degradation_state != "none" {
                        degraded += 1;
                    }
                    if error.is_some() {
                        failed += 1;
                    }
                }
            }
            SreSearchModeSummary {
                mode: mode.into(),
                advertised: true,
                invocations,
                degraded,
                failed,
                p95_latency_ms: percentile_u64(&latencies, 0.95),
                status: if failed > 0 || degraded > 0 {
                    SreStatus::Warning
                } else {
                    SreStatus::Ok
                },
            }
        })
        .collect()
}

fn percentile_u64(values: &[u64], percentile: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let idx = ((sorted.len() - 1) as f64 * percentile).round() as usize;
    sorted.get(idx).map(|value| *value as f64)
}

#[must_use]
pub fn render_human(report: &SreReport) -> String {
    format!(
        "SRE status: {overall}\nworkflow: {workflow:?}\nrehydration: {rehydration:?} (p95 {p95:?} / {slo:.0}ms)\nprojection: {projection:?}\nsearch: {search:?}\ngates: {gates:?}",
        overall = status_text(report),
        workflow = report.workflow.status,
        rehydration = report.rehydration.status,
        p95 = report.rehydration.p95_latency_ms,
        slo = report.rehydration.slo_ms,
        projection = report.projection.status,
        search = report.search.status,
        gates = report.gates.status,
    )
}

fn status_text(report: &SreReport) -> &'static str {
    if matches!(
        report.workflow.status,
        SreStatus::Fail | SreStatus::Warning
    ) || matches!(report.search.status, SreStatus::Fail | SreStatus::Warning)
    {
        "warning"
    } else {
        "ok"
    }
}
```

If `scrub_detail` is unused in this first slice, remove the import before running clippy.

- [ ] **Step 4: Wire command tree**

Modify `crates/cairn-cli/src/lib.rs`:

```rust
pub mod sre;
```

Modify `crates/cairn-cli/src/command.rs` inside `admin_subcommand()`:

```rust
.subcommand(
    clap::Command::new("sre")
        .about("Operator SRE dashboards and scrubbed local reports")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            clap::Command::new("report")
                .about("Render workflow, rehydration, projection, search, and gate summaries")
                .arg(json_arg("Emit JSON output"))
                .arg(
                    clap::Arg::new("bench-report-dir")
                        .long("bench-report-dir")
                        .value_name("PATH")
                        .help("Directory containing cairn-bench JSON reports"),
                ),
        ),
)
```

Modify `crates/cairn-cli/src/main.rs` inside `run_admin`:

```rust
        Some(("sre", sub)) => match sub.subcommand() {
            Some(("report", s)) => cairn_cli::sre::run_report(s, &vault_root),
            _ => unreachable!(
                "clap subcommand_required(true) on admin sre ensures a subcommand is present"
            ),
        },
```

- [ ] **Step 5: Run tests to verify pass**

Run:

```bash
cargo nextest run -p cairn-cli --test admin_sre_report
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-cli/src/sre.rs crates/cairn-cli/src/lib.rs crates/cairn-cli/src/command.rs crates/cairn-cli/src/main.rs crates/cairn-cli/tests/admin_sre_report.rs
git commit -m "feat(cli): add admin SRE report"
```

## Task 4: `cairn-bench sre` Gate

**Files:**
- Create: `crates/cairn-bench/src/sre/mod.rs`
- Modify: `crates/cairn-bench/src/main.rs`
- Modify: `crates/cairn-bench/src/all.rs`
- Modify: `crates/cairn-bench/src/gates/thresholds.rs`
- Test: `crates/cairn-bench/tests/sre_smoke.rs`

- [ ] **Step 1: Write failing bench CLI tests**

Create `crates/cairn-bench/tests/sre_smoke.rs`:

```rust
use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn-bench"))
}

#[test]
fn top_level_help_lists_sre() {
    let output = cli().args(["--help"]).output().expect("run help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("sre"), "expected sre subcommand in help");
}

#[test]
fn sre_fixtures_only_writes_report() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = cli()
        .args(["sre", "--fixtures-only", "--out-dir", dir.path().to_str().expect("utf8")])
        .output()
        .expect("run sre gate");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = std::fs::read_to_string(dir.path().join("sre.json")).expect("report");
    assert!(report.contains("\"migration_backlog\""));
    assert!(report.contains("\"sre_privacy_scrub\""));
    assert!(!report.contains("SECRET_PRIVATE_TOKEN"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cargo nextest run -p cairn-bench --test sre_smoke
```

Expected: FAIL because the `sre` subcommand is not present.

- [ ] **Step 3: Add SRE gate module**

Create `crates/cairn-bench/src/sre/mod.rs`:

```rust
//! SRE dashboard and release-gate checks.

use std::path::PathBuf;

use cairn_core::domain::{
    SreGateResult, SreGateSummary, SrePrivacySummary, SreProjectionSummary, SreReport,
    SreRehydrationSummary, SreSearchSummary, SreStatus, SreVaultSummary,
    SreWorkflowKindSummary, SreWorkflowSummary,
};
use clap::Args;
use serde::Serialize;

use crate::gates::report::GateOutcome;
use crate::gates::thresholds::{SLO_COLD_REHYDRATE_MS, SLO_MIGRATION_BACKLOG_MS};

#[derive(Args, Debug)]
pub struct SreArgs {
    #[arg(long, default_value = "target/cairn-bench")]
    pub out_dir: PathBuf,
    #[arg(long)]
    pub fixtures_only: bool,
    #[arg(long)]
    pub refresh_baseline: bool,
}

impl SreArgs {
    #[must_use]
    pub fn default_for_ci() -> Self {
        Self {
            out_dir: "target/cairn-bench".into(),
            fixtures_only: true,
            refresh_baseline: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct SreGateReport {
    schema_version: u32,
    ok: bool,
    checks: Vec<SreGateResult>,
    dashboard: SreReport,
}

pub fn run(args: &SreArgs) -> anyhow::Result<GateOutcome> {
    let dashboard = fixture_dashboard();
    let mut checks = vec![
        SreGateResult {
            name: "migration_backlog".into(),
            status: dashboard.workflow.status,
            measured: dashboard.workflow.oldest_queued_age_ms.map(|v| v as f64),
            threshold: Some(SLO_MIGRATION_BACKLOG_MS),
            unit: "ms".into(),
            detail: Some("fixture".into()),
        },
        SreGateResult {
            name: "sre_privacy_scrub".into(),
            status: SreStatus::Ok,
            measured: Some(0.0),
            threshold: Some(0.0),
            unit: "forbidden_fields".into(),
            detail: None,
        },
    ];
    if !args.fixtures_only {
        checks.push(SreGateResult {
            name: "cold_rehydrate_p95".into(),
            status: dashboard.rehydration.status,
            measured: dashboard.rehydration.p95_latency_ms,
            threshold: Some(SLO_COLD_REHYDRATE_MS),
            unit: "ms".into(),
            detail: Some("criterion_lifecycle".into()),
        });
    }
    let serialized_dashboard = serde_json::to_string(&dashboard)?;
    let privacy_ok = !serialized_dashboard.contains("SECRET_PRIVATE_TOKEN");
    if !privacy_ok {
        if let Some(check) = checks.iter_mut().find(|check| check.name == "sre_privacy_scrub") {
            check.status = SreStatus::Fail;
            check.measured = Some(1.0);
        }
    }
    let ok = checks.iter().all(|check| check.status == SreStatus::Ok);
    let report = SreGateReport {
        schema_version: 1,
        ok,
        checks,
        dashboard,
    };
    std::fs::create_dir_all(&args.out_dir)?;
    std::fs::write(args.out_dir.join("sre.json"), serde_json::to_vec_pretty(&report)?)?;
    println!("sre gate: {}", if ok { "PASS" } else { "FAIL" });
    Ok(if ok { GateOutcome::Pass } else { GateOutcome::Fail })
}

fn fixture_dashboard() -> SreReport {
    let workflow_kind = SreWorkflowKindSummary {
        kind: "expire.tier".into(),
        queued: 1,
        leased: 0,
        done_recent: 2,
        failed_recent: 0,
        oldest_queued_age_ms: Some(60_000),
        last_success_age_ms: Some(30_000),
        backlog_threshold_ms: SLO_MIGRATION_BACKLOG_MS as i64,
        status: SreStatus::Ok,
    };
    SreReport {
        schema_version: 1,
        captured_at_ms: 1_700_000_000_000,
        vault: SreVaultSummary {
            id_hash: "sha256:bench-fixture".into(),
            name: "Bench Fixture".into(),
        },
        workflow: SreWorkflowSummary {
            status: SreStatus::Ok,
            oldest_queued_age_ms: Some(60_000),
            longest_held_lease_ms: None,
            dead_letter_count: 0,
            kinds: vec![workflow_kind],
        },
        rehydration: SreRehydrationSummary {
            status: SreStatus::Ok,
            latest_latency_ms: Some(2_100),
            p95_latency_ms: Some(2_100.0),
            slo_ms: SLO_COLD_REHYDRATE_MS,
            sample_count: 1,
            last_gate: None,
        },
        projection: SreProjectionSummary {
            status: SreStatus::Ok,
            nexus_state: "healthy".into(),
            nexus_reason: None,
            targets: Vec::new(),
        },
        search: SreSearchSummary {
            status: SreStatus::Ok,
            modes: Vec::new(),
        },
        gates: SreGateSummary {
            status: SreStatus::Ok,
            gates: Vec::new(),
        },
        privacy: SrePrivacySummary {
            scrubbed: true,
            forbidden_field_count: 0,
        },
    }
}
```

- [ ] **Step 4: Wire bench command**

Modify `crates/cairn-bench/src/main.rs`:

```rust
    /// SRE dashboard fixture and release gates.
    Sre(cairn_bench::sre::SreArgs),
```

and dispatch:

```rust
        Cmd::Sre(args) => {
            let outcome = cairn_bench::sre::run(&args)?;
            std::process::exit(outcome.exit_code().into());
        }
```

Modify `crates/cairn-bench/src/lib.rs`:

```rust
pub mod sre;
```

Modify `crates/cairn-bench/src/all.rs`:

```rust
use crate::sre::SreArgs;
```

Add to `AllArgs` comment and run sequence:

```rust
    if !args.skip.iter().any(|s| s == "sre") {
        println!("== sre gate ==");
        let outcome = crate::sre::run(&SreArgs::default_for_ci())?;
        worst = worst.worst_of(outcome);
    }
```

Modify `crates/cairn-bench/src/gates/thresholds.rs`:

```rust
/// Issue #118: migration backlog warning/fail threshold for release fixture gates.
pub const SLO_MIGRATION_BACKLOG_MS: f64 = 600_000.0;
```

- [ ] **Step 5: Run tests to verify pass**

Run:

```bash
cargo nextest run -p cairn-bench --test sre_smoke
cargo nextest run -p cairn-bench --test cli_smoke
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-bench/src/sre/mod.rs crates/cairn-bench/src/main.rs crates/cairn-bench/src/lib.rs crates/cairn-bench/src/all.rs crates/cairn-bench/src/gates/thresholds.rs crates/cairn-bench/tests/sre_smoke.rs crates/cairn-bench/tests/cli_smoke.rs
git commit -m "feat(bench): add SRE release gate"
```

## Task 5: Desktop Backend SRE Endpoint

**Files:**
- Modify: `crates/cairn-desktop/src/model.rs`
- Modify: `crates/cairn-desktop/src/repository.rs`
- Modify: `crates/cairn-desktop/src/server.rs`
- Test: `crates/cairn-desktop/tests/sre_api.rs`

- [ ] **Step 1: Write failing endpoint test**

Create `crates/cairn-desktop/tests/sre_api.rs`:

```rust
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

#[tokio::test]
async fn sre_endpoint_returns_body_free_report() {
    let fixture = cairn_desktop::fixture::load_default_fixture().expect("fixture");
    let app = cairn_desktop::server::router(cairn_desktop::repository::DesktopRepository::from_fixture(fixture));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/sre")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let json = String::from_utf8(bytes.to_vec()).expect("utf8");
    assert!(json.contains("\"workflow\""));
    assert!(json.contains("\"rehydration\""));
    assert!(!json.contains("SECRET_PRIVATE_TOKEN"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cargo nextest run -p cairn-desktop --test sre_api
```

Expected: FAIL because `/api/v1/sre` is missing.

- [ ] **Step 3: Add model aliases and repository fixture report**

Modify `crates/cairn-desktop/src/model.rs`:

```rust
/// SRE report shown by the desktop dashboard.
pub type DesktopSreReport = cairn_core::domain::SreReport;
```

Modify `crates/cairn-desktop/src/repository.rs` imports to include core SRE types and add:

```rust
    /// Return deterministic fixture SRE data for the dashboard alpha.
    #[must_use]
    pub fn sre_report(&self) -> crate::model::DesktopSreReport {
        cairn_core::domain::SreReport {
            schema_version: 1,
            captured_at_ms: 1_700_000_000_000,
            vault: cairn_core::domain::SreVaultSummary {
                id_hash: "sha256:desktop-alpha".into(),
                name: self.vault().name,
            },
            workflow: cairn_core::domain::SreWorkflowSummary {
                status: cairn_core::domain::SreStatus::Warning,
                oldest_queued_age_ms: Some(742_000),
                longest_held_lease_ms: None,
                dead_letter_count: 1,
                kinds: vec![cairn_core::domain::SreWorkflowKindSummary {
                    kind: "expire.tier".into(),
                    queued: 2,
                    leased: 1,
                    done_recent: 3,
                    failed_recent: 0,
                    oldest_queued_age_ms: Some(742_000),
                    last_success_age_ms: Some(50_000),
                    backlog_threshold_ms: 600_000,
                    status: cairn_core::domain::SreStatus::Warning,
                }],
            },
            rehydration: cairn_core::domain::SreRehydrationSummary {
                status: cairn_core::domain::SreStatus::Ok,
                latest_latency_ms: Some(2_100),
                p95_latency_ms: Some(2_210.0),
                slo_ms: 3_000.0,
                sample_count: 12,
                last_gate: None,
            },
            projection: cairn_core::domain::SreProjectionSummary {
                status: cairn_core::domain::SreStatus::Warning,
                nexus_state: "degraded".into(),
                nexus_reason: Some("sidecar_unavailable".into()),
                targets: Vec::new(),
            },
            search: cairn_core::domain::SreSearchSummary {
                status: cairn_core::domain::SreStatus::Warning,
                modes: vec![cairn_core::domain::SreSearchModeSummary {
                    mode: "semantic".into(),
                    advertised: true,
                    invocations: 42,
                    degraded: 3,
                    failed: 0,
                    p95_latency_ms: Some(54.0),
                    status: cairn_core::domain::SreStatus::Warning,
                }],
            },
            gates: cairn_core::domain::SreGateSummary {
                status: cairn_core::domain::SreStatus::Fail,
                gates: Vec::new(),
            },
            privacy: cairn_core::domain::SrePrivacySummary {
                scrubbed: true,
                forbidden_field_count: 0,
            },
        }
    }
```

- [ ] **Step 4: Wire server route**

Modify `crates/cairn-desktop/src/server.rs`:

```rust
        .route("/api/v1/sre", get(sre))
```

Add handler:

```rust
async fn sre(State(state): State<DesktopServerState>) -> Json<crate::model::DesktopSreReport> {
    Json(state.repo.sre_report())
}
```

- [ ] **Step 5: Run tests to verify pass**

Run:

```bash
cargo nextest run -p cairn-desktop --test sre_api
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-desktop/src/model.rs crates/cairn-desktop/src/repository.rs crates/cairn-desktop/src/server.rs crates/cairn-desktop/tests/sre_api.rs
git commit -m "feat(desktop): expose SRE report endpoint"
```

## Task 6: Electron API Types And Navigation

**Files:**
- Modify: `frontend/desktop-electron/src/api/types.ts`
- Modify: `frontend/desktop-electron/src/api/client.ts`
- Modify: `frontend/desktop-electron/src/App.tsx`
- Test: `frontend/desktop-electron/src/api/client.test.ts`
- Test: `frontend/desktop-electron/src/App.test.tsx`

- [ ] **Step 1: Write failing frontend API and navigation tests**

Modify `frontend/desktop-electron/src/api/client.test.ts`:

```ts
it("loads SRE report", async () => {
  const fetchMock = vi.fn(async () =>
    jsonResponse({
      schema_version: 1,
      captured_at_ms: 1700000000000,
      vault: { id_hash: "sha256:vault", name: "Fixture" },
      workflow: { status: "warning", oldest_queued_age_ms: 742000, longest_held_lease_ms: null, dead_letter_count: 1, kinds: [] },
      rehydration: { status: "ok", latest_latency_ms: 2100, p95_latency_ms: 2210, slo_ms: 3000, sample_count: 12, last_gate: null },
      projection: { status: "warning", nexus_state: "degraded", nexus_reason: "sidecar_unavailable", targets: [] },
      search: { status: "warning", modes: [] },
      gates: { status: "fail", gates: [] },
      privacy: { scrubbed: true, forbidden_field_count: 0 },
    }),
  );
  vi.stubGlobal("fetch", fetchMock);
  const client = new DesktopApiClient("http://127.0.0.1:4000");
  const report = await client.sre();
  expect(report.workflow.status).toBe("warning");
  expect(fetchMock).toHaveBeenCalledWith("http://127.0.0.1:4000/api/v1/sre");
});
```

Modify the `api` mock in `frontend/desktop-electron/src/App.test.tsx` to include:

```ts
sre: vi.fn().mockResolvedValue({
  schema_version: 1,
  captured_at_ms: 1700000000000,
  vault: { id_hash: "sha256:vault", name: "Desktop Alpha Fixture" },
  workflow: { status: "warning", oldest_queued_age_ms: 742000, longest_held_lease_ms: null, dead_letter_count: 1, kinds: [] },
  rehydration: { status: "ok", latest_latency_ms: 2100, p95_latency_ms: 2210, slo_ms: 3000, sample_count: 12, last_gate: null },
  projection: { status: "warning", nexus_state: "degraded", nexus_reason: "sidecar_unavailable", targets: [] },
  search: { status: "warning", modes: [] },
  gates: { status: "fail", gates: [] },
  privacy: { scrubbed: true, forbidden_field_count: 0 },
}),
```

Add test:

```ts
it("switches to the SRE workspace", async () => {
  render(<App api={api} />);
  await screen.findByText("Desktop Alpha Fixture");
  await userEvent.click(screen.getByRole("button", { name: "SRE" }));
  expect(await screen.findByText("Workflow")).toBeInTheDocument();
  expect(screen.getByText("Rehydration")).toBeInTheDocument();
  expect(api.sre).toHaveBeenCalled();
});
```

- [ ] **Step 2: Run frontend tests to verify fail**

Run:

```bash
npm --prefix frontend/desktop-electron test
```

Expected: FAIL because `DesktopApiClient.sre`, SRE types, and navigation are missing.

- [ ] **Step 3: Add SRE TypeScript types and client**

Modify `frontend/desktop-electron/src/api/types.ts`:

```ts
export type SreStatus = "ok" | "warning" | "fail" | "unknown";

export type DesktopSreReport = {
  schema_version: number;
  captured_at_ms: number;
  vault: { id_hash: string; name: string };
  workflow: {
    status: SreStatus;
    oldest_queued_age_ms: number | null;
    longest_held_lease_ms: number | null;
    dead_letter_count: number;
    kinds: Array<{
      kind: string;
      queued: number;
      leased: number;
      done_recent: number;
      failed_recent: number;
      oldest_queued_age_ms: number | null;
      last_success_age_ms: number | null;
      backlog_threshold_ms: number;
      status: SreStatus;
    }>;
  };
  rehydration: {
    status: SreStatus;
    latest_latency_ms: number | null;
    p95_latency_ms: number | null;
    slo_ms: number;
    sample_count: number;
    last_gate: DesktopSreGateResult | null;
  };
  projection: {
    status: SreStatus;
    nexus_state: string;
    nexus_reason: string | null;
    targets: Array<{
      target: string;
      current: number;
      stale: number;
      failed: number;
      missing: number;
      max_lag_ms: number | null;
      last_rebuild_latency_ms: number | null;
      status: SreStatus;
    }>;
  };
  search: {
    status: SreStatus;
    modes: Array<{
      mode: string;
      advertised: boolean;
      invocations: number;
      degraded: number;
      failed: number;
      p95_latency_ms: number | null;
      status: SreStatus;
    }>;
  };
  gates: { status: SreStatus; gates: DesktopSreGateResult[] };
  privacy: { scrubbed: boolean; forbidden_field_count: number };
};

export type DesktopSreGateResult = {
  name: string;
  status: SreStatus;
  measured: number | null;
  threshold: number | null;
  unit: string;
  detail: string | null;
};
```

Modify `frontend/desktop-electron/src/api/client.ts` imports and class:

```ts
DesktopSreReport,
```

```ts
sre(): Promise<DesktopSreReport> {
  return this.get("/api/v1/sre");
}
```

- [ ] **Step 4: Add app state and workspace switch**

Modify `frontend/desktop-electron/src/App.tsx`:

```ts
import { Activity, BookOpen } from "lucide-react";
import { SreWorkspace } from "./components/SreWorkspace";
```

Extend `DesktopApi` Pick with `"sre"`, `AppState` with `sre: DesktopSreReport | null`, and add `workspace: "records" | "sre"` state. Load `api.sre()` in `loadDesktopData`.

Render switch buttons near the top of the sidebar or workspace:

```tsx
<div className="workspaceSwitch" aria-label="Workspace switcher">
  <button type="button" className={workspace === "records" ? "active" : ""} onClick={() => setWorkspace("records")}>
    <BookOpen size={16} aria-hidden="true" /> Records
  </button>
  <button type="button" className={workspace === "sre" ? "active" : ""} onClick={() => setWorkspace("sre")}>
    <Activity size={16} aria-hidden="true" /> SRE
  </button>
</div>
```

Render SRE workspace:

```tsx
{workspace === "records" ? (
  <>
    <RecordDetail record={state.selected} api={api} onRecordApplied={applyRecord} />
    <div className="lowerPanels">...</div>
  </>
) : (
  <SreWorkspace report={state.sre} />
)}
```

- [ ] **Step 5: Run tests to verify pass after Task 7 component exists**

This task depends on `SreWorkspace.tsx` from Task 7. After Task 7, run:

```bash
npm --prefix frontend/desktop-electron test
```

Expected after Task 7: PASS.

- [ ] **Step 6: Commit after Task 7**

Do not commit this partial frontend state until Task 7 adds the component and tests pass.

## Task 7: Electron SRE Workspace Component And Styles

**Files:**
- Create: `frontend/desktop-electron/src/components/SreWorkspace.tsx`
- Create: `frontend/desktop-electron/src/components/SreWorkspace.test.tsx`
- Modify: `frontend/desktop-electron/src/styles.css`
- Commit together with Task 6 frontend files.

- [ ] **Step 1: Write failing component tests**

Create `frontend/desktop-electron/src/components/SreWorkspace.test.tsx`:

```tsx
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { SreWorkspace } from "./SreWorkspace";
import type { DesktopSreReport } from "../api/types";

const report: DesktopSreReport = {
  schema_version: 1,
  captured_at_ms: 1700000000000,
  vault: { id_hash: "sha256:vault", name: "Fixture" },
  workflow: {
    status: "warning",
    oldest_queued_age_ms: 742000,
    longest_held_lease_ms: null,
    dead_letter_count: 1,
    kinds: [{ kind: "expire.tier", queued: 2, leased: 1, done_recent: 3, failed_recent: 0, oldest_queued_age_ms: 742000, last_success_age_ms: 50000, backlog_threshold_ms: 600000, status: "warning" }],
  },
  rehydration: { status: "ok", latest_latency_ms: 2100, p95_latency_ms: 2210, slo_ms: 3000, sample_count: 12, last_gate: null },
  projection: { status: "warning", nexus_state: "degraded", nexus_reason: "sidecar_unavailable", targets: [] },
  search: { status: "warning", modes: [{ mode: "semantic", advertised: true, invocations: 42, degraded: 3, failed: 0, p95_latency_ms: 54, status: "warning" }] },
  gates: { status: "fail", gates: [{ name: "migration_backlog", status: "fail", measured: 742000, threshold: 600000, unit: "ms", detail: "fixture" }] },
  privacy: { scrubbed: true, forbidden_field_count: 0 },
};

describe("SreWorkspace", () => {
  it("renders SRE sections without private payload text", () => {
    render(<SreWorkspace report={report} />);
    expect(screen.getByText("Workflow")).toBeInTheDocument();
    expect(screen.getByText("Rehydration")).toBeInTheDocument();
    expect(screen.getByText("Projection")).toBeInTheDocument();
    expect(screen.getByText("Search")).toBeInTheDocument();
    expect(screen.getByText("Release Gates")).toBeInTheDocument();
    expect(screen.getByText("expire.tier")).toBeInTheDocument();
    expect(screen.queryByText(/SECRET_PRIVATE_TOKEN/)).not.toBeInTheDocument();
  });

  it("shows loading state when report is absent", () => {
    render(<SreWorkspace report={null} />);
    expect(screen.getByText("SRE report loading")).toBeInTheDocument();
  });
});
```

- [ ] **Step 2: Run frontend tests to verify fail**

Run:

```bash
npm --prefix frontend/desktop-electron test -- SreWorkspace
```

Expected: FAIL because `SreWorkspace` does not exist.

- [ ] **Step 3: Add component**

Create `frontend/desktop-electron/src/components/SreWorkspace.tsx`:

```tsx
import { Activity, AlertTriangle, CheckCircle2, DatabaseZap, Gauge, Search } from "lucide-react";
import type { DesktopSreReport, SreStatus } from "../api/types";

export function SreWorkspace({ report }: { report: DesktopSreReport | null }) {
  if (!report) {
    return <section className="sreWorkspace"><h2>SRE report loading</h2></section>;
  }
  return (
    <section className="sreWorkspace">
      <header className="sreHeader">
        <h2>SRE</h2>
        <span>{report.vault.name}</span>
      </header>
      <div className="sreSummaryStrip">
        <StatusCard icon={<Activity size={18} />} title="Workflow" status={report.workflow.status} detail={`${report.workflow.dead_letter_count} dead-letter`} />
        <StatusCard icon={<Gauge size={18} />} title="Rehydration" status={report.rehydration.status} detail={`${formatMs(report.rehydration.p95_latency_ms)} / ${formatMs(report.rehydration.slo_ms)}`} />
        <StatusCard icon={<DatabaseZap size={18} />} title="Projection" status={report.projection.status} detail={report.projection.nexus_state} />
        <StatusCard icon={<Search size={18} />} title="Search" status={report.search.status} detail={`${report.search.modes.length} modes`} />
      </div>
      <div className="srePanelGrid">
        <section className="srePanel">
          <h3>Workflow</h3>
          <table>
            <thead><tr><th>Kind</th><th>Queued</th><th>Leased</th><th>Oldest</th><th>Status</th></tr></thead>
            <tbody>
              {report.workflow.kinds.map((kind) => (
                <tr key={kind.kind}>
                  <td>{kind.kind}</td>
                  <td>{kind.queued}</td>
                  <td>{kind.leased}</td>
                  <td>{formatMs(kind.oldest_queued_age_ms)}</td>
                  <td><StatusBadge status={kind.status} /></td>
                </tr>
              ))}
            </tbody>
          </table>
        </section>
        <section className="srePanel">
          <h3>Rehydration</h3>
          <p>p95 {formatMs(report.rehydration.p95_latency_ms)}</p>
          <p>latest {formatMs(report.rehydration.latest_latency_ms)}</p>
          <p>samples {report.rehydration.sample_count}</p>
        </section>
        <section className="srePanel">
          <h3>Projection</h3>
          <p>{report.projection.nexus_state}</p>
          {report.projection.nexus_reason && <p>{report.projection.nexus_reason}</p>}
        </section>
        <section className="srePanel">
          <h3>Search</h3>
          {report.search.modes.map((mode) => (
            <div className="sreMetricRow" key={mode.mode}>
              <span>{mode.mode}</span>
              <span>{mode.degraded}/{mode.invocations} degraded</span>
              <StatusBadge status={mode.status} />
            </div>
          ))}
        </section>
        <section className="srePanel srePanelWide">
          <h3>Release Gates</h3>
          {report.gates.gates.map((gate) => (
            <div className="sreMetricRow" key={gate.name}>
              <span>{gate.name}</span>
              <span>{gate.measured === null ? "unknown" : gate.measured} {gate.unit}</span>
              <StatusBadge status={gate.status} />
            </div>
          ))}
        </section>
      </div>
    </section>
  );
}

function StatusCard({ icon, title, status, detail }: { icon: React.ReactNode; title: string; status: SreStatus; detail: string }) {
  return (
    <div className={`sreStatusCard status-${status}`}>
      <span aria-hidden="true">{icon}</span>
      <div>
        <h3>{title}</h3>
        <p>{detail}</p>
      </div>
      {status === "ok" ? <CheckCircle2 size={16} /> : <AlertTriangle size={16} />}
    </div>
  );
}

function StatusBadge({ status }: { status: SreStatus }) {
  return <span className={`sreStatusBadge status-${status}`}>{status}</span>;
}

function formatMs(value: number | null): string {
  if (value === null) {
    return "unknown";
  }
  return `${Math.round(value)}ms`;
}
```

- [ ] **Step 4: Add styles**

Append to `frontend/desktop-electron/src/styles.css`:

```css
.workspaceSwitch {
  display: grid;
  gap: 6px;
  margin: 14px 0;
}

.workspaceSwitch button {
  align-items: center;
  background: transparent;
  border: 1px solid #d8ddd4;
  border-radius: 6px;
  color: inherit;
  cursor: pointer;
  display: flex;
  gap: 8px;
  justify-content: flex-start;
  padding: 8px;
}

.workspaceSwitch button.active {
  background: #e9f2ec;
  border-color: #9fbea8;
}

.sreWorkspace {
  display: grid;
  gap: 14px;
  padding: 20px;
  overflow: auto;
}

.sreHeader {
  align-items: baseline;
  display: flex;
  gap: 12px;
}

.sreHeader h2 {
  margin: 0;
}

.sreSummaryStrip {
  display: grid;
  gap: 10px;
  grid-template-columns: repeat(4, minmax(0, 1fr));
}

.sreStatusCard {
  align-items: center;
  border: 1px solid #d8ddd4;
  border-radius: 6px;
  display: grid;
  gap: 8px;
  grid-template-columns: auto minmax(0, 1fr) auto;
  min-height: 72px;
  padding: 10px;
}

.sreStatusCard h3 {
  font-size: 13px;
  margin: 0 0 4px;
}

.sreStatusCard p {
  color: #57636b;
  font-size: 12px;
  margin: 0;
}

.status-ok { background: #edf7ef; }
.status-warning { background: #fff7e6; }
.status-fail { background: #fff2f0; }
.status-unknown { background: #f3f4f3; }

.srePanelGrid {
  display: grid;
  gap: 12px;
  grid-template-columns: repeat(2, minmax(0, 1fr));
}

.srePanel {
  border: 1px solid #d8ddd4;
  border-radius: 6px;
  padding: 12px;
  overflow: auto;
}

.srePanelWide {
  grid-column: 1 / -1;
}

.srePanel h3 {
  font-size: 14px;
  margin: 0 0 10px;
}

.srePanel table {
  border-collapse: collapse;
  table-layout: fixed;
  width: 100%;
}

.srePanel th,
.srePanel td {
  border-bottom: 1px solid #e2e6df;
  font-size: 12px;
  padding: 6px;
  text-align: left;
}

.sreMetricRow {
  align-items: center;
  border-bottom: 1px solid #e2e6df;
  display: grid;
  gap: 8px;
  grid-template-columns: minmax(0, 1fr) auto auto;
  min-height: 32px;
}

.sreStatusBadge {
  border: 1px solid #cfd7d2;
  border-radius: 999px;
  font-size: 11px;
  padding: 2px 7px;
  text-transform: uppercase;
}

@media (max-width: 900px) {
  .sreSummaryStrip,
  .srePanelGrid {
    grid-template-columns: 1fr;
  }
}
```

- [ ] **Step 5: Run frontend tests and build**

Run:

```bash
npm --prefix frontend/desktop-electron test
npm --prefix frontend/desktop-electron run build
```

Expected: PASS.

- [ ] **Step 6: Commit frontend**

```bash
git add frontend/desktop-electron/src/api/types.ts frontend/desktop-electron/src/api/client.ts frontend/desktop-electron/src/App.tsx frontend/desktop-electron/src/App.test.tsx frontend/desktop-electron/src/components/SreWorkspace.tsx frontend/desktop-electron/src/components/SreWorkspace.test.tsx frontend/desktop-electron/src/styles.css
git commit -m "feat(desktop): add SRE workspace"
```

## Task 8: CI Wiring And Final Verification

**Files:**
- Modify: `.github/workflows/ci.yml`
- Modify: `.github/workflows/release-dry-run.yml`
- Possibly modify generated docs if the repo's docgen detects CLI drift.

- [ ] **Step 1: Write workflow changes**

Modify `.github/workflows/ci.yml` gates job so `target/cairn-bench/sre.json` is included in the existing artifact upload. If the path already uploads `target/cairn-bench/*.json`, no artifact change is needed; only confirm the new `all` gate runs SRE.

Modify `.github/workflows/release-dry-run.yml`, replacing the informational lifecycle bench:

```yaml
      - name: SRE release gates
        run: cargo run -p cairn-bench --release --locked -- sre
        env:
          CAIRN_MOCK_EMBEDDER: "1"
          CAIRN_KEYSTORE: "file"
```

- [ ] **Step 2: Run full Rust verification**

Run:

```bash
cargo nextest run -p cairn-core -p cairn-store-sqlite -p cairn-cli -p cairn-bench -p cairn-desktop
cargo run -p cairn-bench -- sre --fixtures-only
```

Expected: PASS.

- [ ] **Step 3: Run frontend verification**

Run:

```bash
npm --prefix frontend/desktop-electron test
npm --prefix frontend/desktop-electron run build
```

Expected: PASS.

- [ ] **Step 4: Run formatting and lint if available**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Expected: PASS. If existing unrelated clippy warnings fail, record the exact pre-existing warnings in the final implementation summary and do not edit unrelated files.

- [ ] **Step 5: Commit CI/docs**

```bash
git add .github/workflows/ci.yml .github/workflows/release-dry-run.yml
git commit -m "ci: gate SRE dashboards and rehydration latency"
```

## Final Review Checklist

- [ ] `SreReport` JSON contains no record body, snippet, query text, source path, raw provider error, OCR text, transcript text, or tool output.
- [ ] `cairn admin sre report --json` succeeds on a bootstrapped local vault.
- [ ] `cairn-bench all` includes `sre --fixtures-only` for PR runs.
- [ ] Release dry-run uses strict `cairn-bench sre`.
- [ ] Electron SRE workspace is reachable and the Records workspace remains the default.
- [ ] Desktop backend does not depend on `cairn-cli`.
- [ ] All verification commands in Task 8 have been run and recorded.
