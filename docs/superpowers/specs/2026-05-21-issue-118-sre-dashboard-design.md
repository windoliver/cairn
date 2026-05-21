# Issue 118 SRE Dashboard And Rehydration Gates Design

## Summary

Issue [#118](https://github.com/windoliver/cairn/issues/118) adds the v0.2
operator SRE surface for tier movement, rehydration latency, projection lag,
search degradation, and release gates. It builds on the closed dependencies
from issue #117 (`cairn-bench` scorecards and gate infrastructure) and issue
#107 (explicit cold-rehydration request path).

The approved direction is a dedicated SRE workspace in the existing Electron
GUI alpha, backed by the same scrubbed report model exposed through the CLI and
desktop HTTP API. Release checks use `cairn-bench`, not the GUI, so automated
gates stay deterministic and headless.

## Design Sources

- `docs/design/design-brief.md` section 15 Evaluation: replay, latency SLOs,
  privacy SLOs, regression gates, and no-network evaluation.
- `docs/design/design-brief.md` section 18.c US6: cold rehydration SLO and SRE
  observability expectations.
- `docs/design/design-brief.md` section 19 v0.2 sequencing: OpenTelemetry,
  tier-migration dashboards, rehydration latency gates, Electron GUI alpha,
  and public benchmark harness.
- Issue #117: `cairn-bench` scorecard and release-scorecard precedent.
- Issue #107: explicit `retrieve --session --rehydrate` path and body-free
  rehydration trace precedent.

## Scope

In scope:

- A body-free SRE report model shared by CLI, desktop backend, Electron, and
  bench gate fixtures.
- CLI operator summary through `cairn admin sre report`, with `--json` for
  machine-readable output and concise human output by default.
- Desktop backend endpoint `GET /api/v1/sre`.
- Electron dedicated SRE workspace reachable from the existing vault sidebar
  or workspace switcher.
- Local dashboard views for workflow tier movement, rehydration latency,
  projection lag, and search degradation.
- Release gates for rehydration latency and migration backlog.
- Fixture tests for dashboard data, latency gates, and privacy scrubbing.

Out of scope:

- Production hosted dashboards, remote metrics collection, Prometheus/Grafana
  packaging, and SaaS alert routing.
- New storage authority. SQLite remains the authority; Nexus projection state
  is derived.
- New Electron packaging or auto-update work.
- Query-text, record-body, snippet, source-path, or raw-error exposure in SRE
  reports.

## Current State

`origin/main` already has the relevant substrate:

- `crates/cairn-bench` with latency, memory, privacy, scorecard, and `all`
  gates.
- `crates/cairn-bench/benches/lifecycle.rs` with a `cold_rehydrate_p95`
  criterion bench, currently release-informational.
- `crates/cairn-core/src/domain/metrics.rs` with body-free metric events for
  verb invocation, search completion, projection rebuild, workflow jobs, and
  evaluation sweeps.
- `crates/cairn-core/src/contract/workflow_jobs.rs` and
  `crates/cairn-store-sqlite/src/workflow_jobs_reader.rs` with read-only
  workflow job health queries for lint.
- `MemoryStore::projection_summaries` and `projection_failures` for Nexus
  projection health and failure summaries.
- `crates/cairn-desktop` and `frontend/desktop-electron` for the GUI alpha.

The missing piece is a single operator-facing SRE view that aggregates these
signals without leaking private content, plus release gates that turn the
rehydration and migration backlog SLOs into pass/fail checks.

## Architecture

### Shared SRE Model

Create a pure DTO and classifier module in `cairn-core`, for example
`crates/cairn-core/src/domain/sre.rs`. It contains no I/O and no adapter
dependencies. It is serializable and intentionally body-free.

Top-level shape:

```rust
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
```

The same module also owns pure helper functions that classify DTO inputs into
`ok`, `warning`, `fail`, and `unknown` states. CLI, desktop, and bench code may
all call these pure helpers without depending on each other.

The report model uses only these classes of values:

- ids or salted/hash-safe identifiers
- workflow kind, projection target, search mode, gate name, status, severity
- counts, durations, ages, percentiles, thresholds, ratios
- body-free error classes such as `nonzero_exit`, `sidecar_unavailable`, or
  `semantic_provider_transient`

It never contains record body, snippet, query text, source path, raw exception
message, tool output, OCR text, transcript text, or raw LLM payload.

### Data Collection

Add a small report builder in CLI-owned code, for example
`crates/cairn-cli/src/sre.rs`, because it needs local filesystem, config, store,
metrics, and Nexus probes. The builder gathers adapter data, maps it into the
pure `cairn-core::domain::sre` DTOs, and returns `SreReport`.

Inputs:

- `CairnConfig` and vault root.
- `WorkflowJobsReader` for workflow backlog, stuck jobs, dead letters, and
  last successful workflow sweeps.
- `MemoryStore::projection_summaries` and `projection_failures` when a store is
  available.
- `nexus::evaluate_projection_status` for sidecar reachability.
- `.cairn/metrics.jsonl`, parsed with tolerant unknown-event handling.
- Optional `target/cairn-bench/*.json` paths when the caller explicitly passes
  `--bench-report-dir`; absent files become `status = "unknown"`, not an error.

The desktop backend must not depend on `cairn-cli`. For the current
fixture-backed alpha it returns a deterministic fixture `SreReport` built from
`cairn-core` DTOs. When the desktop server grows a vault-backed repository, it
should gather the same adapter inputs locally and reuse only the pure
classification helpers from `cairn-core`.

## Report Sections

### Workflow Tier Movement

Operators need to see stuck tier migrations and slow workflow processing. The
workflow section reports aggregate job state by kind:

```rust
pub struct SreWorkflowSummary {
    pub status: SreStatus,
    pub oldest_queued_age_ms: Option<i64>,
    pub longest_held_lease_ms: Option<i64>,
    pub dead_letter_count: usize,
    pub kinds: Vec<SreWorkflowKindSummary>,
}

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
```

The first implementation uses workflow jobs as the local source of truth for
backlog. It groups known tier-movement kinds such as `expire.tier`,
`dream.light`, `dream.rem`, `dream.deep`, `evaluate.sweep`, and projection or
rehydration jobs when present. Unknown kinds are still listed, because they may
come from newer workflow plugins.

Thresholds:

- `workflows.lint.stuck_queue_threshold_ms` for backlog age.
- `workflows.lint.overdue_threshold_ms` for overdue lifecycle kinds.
- A release-gate migration backlog default of 10 minutes unless overridden by
  a `cairn-bench` fixture manifest.

### Rehydration Latency

The rehydration section combines runtime metrics and gate reports:

```rust
pub struct SreRehydrationSummary {
    pub status: SreStatus,
    pub latest_latency_ms: Option<u64>,
    pub p95_latency_ms: Option<f64>,
    pub slo_ms: f64,
    pub sample_count: u64,
    pub last_gate: Option<SreGateResult>,
}
```

Add a new body-free metric event in `MetricEvent`:

```rust
#[serde(rename = "rehydration_completed")]
RehydrationCompleted {
    ts_ms: i64,
    target: String,
    source_tier: String,
    restored_tier: String,
    status: String,
    latency_ms: u64,
    bytes_restored: u64,
    record_count: u64,
    error: Option<String>,
}
```

`target` is a coarse class such as `session`; it is not a session id. Existing
issue #107 policy traces remain useful for per-call debugging, but metrics are
the right dashboard input because they are append-only and scrubbed by design.

The gate SLO is the design brief section 15 cold-rehydration target:
`cold-rehydration (<= 10 MB session) < 3 s p95`.

### Projection Lag

The projection section summarizes Nexus and markdown projection health without
showing projected bodies:

```rust
pub struct SreProjectionSummary {
    pub status: SreStatus,
    pub nexus_state: String,
    pub nexus_reason: Option<String>,
    pub targets: Vec<SreProjectionTargetSummary>,
}

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
```

Sources:

- `nexus::evaluate_projection_status` for disabled, healthy, degraded.
- `MemoryStore::projection_summaries` for target-level item states.
- `MemoryStore::projection_failures` for failed rows, counted by target and
  body-free reason class.
- `MetricEvent::ProjectionRebuild` for rebuild latency and queue lag.

Projection reasons are normalized to safe classes before entering the report.
Raw sidecar errors stay in logs and lint, not dashboard JSON.

### Search Degradation

The search section reports whether each search mode is available and degraded:

```rust
pub struct SreSearchSummary {
    pub status: SreStatus,
    pub modes: Vec<SreSearchModeSummary>,
}

pub struct SreSearchModeSummary {
    pub mode: String,
    pub advertised: bool,
    pub invocations: u64,
    pub degraded: u64,
    pub failed: u64,
    pub p95_latency_ms: Option<f64>,
    pub status: SreStatus,
}
```

Sources:

- Status capabilities for advertised search modes.
- `MetricEvent::SearchCompleted` and `MetricEvent::VerbInvocation` for
  invocation, degradation, failure, and latency counts.
- Existing `semantic_degraded` response behavior feeds the metric state as
  `degradation_state = "partial"` or a similarly stable value.

The report never includes the query, hit snippets, titles, or record ids.

### Release Gates

`SreGateSummary` lists the latest local gate status known to the operator:

```rust
pub struct SreGateSummary {
    pub status: SreStatus,
    pub gates: Vec<SreGateResult>,
}

pub struct SreGateResult {
    pub name: String,
    pub status: SreStatus,
    pub measured: Option<f64>,
    pub threshold: Option<f64>,
    pub unit: String,
    pub detail: Option<String>,
}
```

Details are scrubbed and stable. Examples: `baseline_regression`,
`slo_exceeded`, `missing_input`, `fixture_failed`.

## CLI Surface

Add `cairn admin sre report`:

```text
cairn admin sre report [--json] [--bench-report-dir <path>]
```

Default human output is a compact operator summary:

```text
SRE status: warning
workflow: warning (oldest queued expire.tier 742000ms)
rehydration: ok (p95 2210ms / 3000ms)
projection: degraded (nexus sidecar unavailable)
search: warning (semantic degraded 3/42)
gates: fail (migration_backlog)
```

`--json` prints the full `SreReport`.

Exit code policy:

- `0` when report builds, regardless of status. This is a dashboard command,
  not a release gate.
- `69` when required runtime dependencies are unavailable in a way that
  prevents building the report, such as an unreadable bound vault DB.
- `78` for invalid config or explicit bad `--bench-report-dir`.

## Desktop Backend Surface

Extend `crates/cairn-desktop`:

- Add DTOs mirroring `SreReport` or re-export JSON-compatible desktop-specific
  wrappers if the backend needs camelCase field names.
- Add `DesktopRepository::sre_report()`.
- Add `GET /api/v1/sre`.
- Fixture repository returns deterministic warning-state data covering all four
  dashboard cards and release gate states.

The endpoint must be cache-light: each request reads current fixture or vault
state and returns one report. Streaming and live refresh are out of scope.

## Electron GUI Surface

Add a dedicated SRE workspace to `frontend/desktop-electron`.

Navigation:

- Keep the existing record workspace as the default first screen.
- Add a compact workspace switch in the sidebar: Records and SRE.
- Use `lucide-react` icons already present in `package.json` for the switch and
  status cards.

Components:

- `SreWorkspace.tsx`: top-level layout.
- `SreSummaryStrip.tsx`: four status cards for workflow, rehydration,
  projection, and search.
- `SreWorkflowPanel.tsx`: table of workflow kinds, state counts, oldest queue
  age, last success age, and status.
- `SreRehydrationPanel.tsx`: latest latency, p95, SLO, sample count, and gate
  result.
- `SreProjectionPanel.tsx`: Nexus state and target summaries.
- `SreSearchDegradationPanel.tsx`: mode availability, degraded counts, failure
  counts, and p95 latency.
- `SreGatePanel.tsx`: release-gate pass/fail rows.

Layout:

- Use a dense, utilitarian dashboard, not a landing page.
- No nested cards. The workspace uses full-width bands and bordered panels.
- Status colors are restrained and not a one-hue palette: green for ok, amber
  for warning, red for fail, neutral gray for unknown.
- Tables keep stable column widths so counts and status labels do not shift the
  layout.
- Mobile/narrow desktop collapses panels into a single column with the summary
  strip still first.

## Bench And Release Gates

Add `cairn-bench sre`:

```text
cairn-bench sre [--out-dir target/cairn-bench] [--fixtures-only] [--refresh-baseline]
```

Checks:

1. `cold_rehydrate_p95`: parse the existing lifecycle criterion output or run
   the lifecycle bench unless `--fixtures-only` is set. Fails when p95 exceeds
   `SLO_COLD_REHYDRATE_MS` or the committed baseline regression threshold.
2. `migration_backlog`: run a deterministic SQLite workflow-job fixture through
   the SRE report comparator. Fails when queued tier-migration jobs exceed the
   backlog age threshold.
3. `projection_lag_fixture`: run a fixture report with stale and failed
   projection rows and verify status classification.
4. `sre_privacy_scrub`: serialize reports seeded with sentinel private text in
   source records, queries, paths, raw errors, and snippets; fail if the
   serialized report contains any sentinel token.

Output:

- `target/cairn-bench/sre.json` with per-check results.
- Human summary to stdout.

CI wiring:

- `ci.yml`: run `cairn-bench sre --fixtures-only` with the existing gates job
  so dashboard fixtures and privacy scrub run on every PR.
- `release-dry-run.yml`: replace the current informational lifecycle bench with
  `cairn-bench sre`, so cold rehydration and migration backlog become release
  pass/fail gates.
- `cairn-bench all`: include the SRE gate using `SreArgs::default_for_ci()`,
  which sets `fixtures_only = true`, with `--skip sre` supported for local
  focused runs. Full cold-rehydrate lifecycle measurement stays in
  `release-dry-run.yml`.

## Privacy And Redaction

Privacy is enforced in three places:

1. Report model: no fields exist for private text payloads.
2. Report builder: raw error strings are mapped to stable classes before
   serialization.
3. Tests: fixture reports deliberately include sentinel strings in source data
   and assert the JSON output does not contain them.

Allowed examples:

- `record_count = 42`
- `kind = "expire.tier"`
- `projection target = "bm25s_lexical"`
- `error = "sidecar_unavailable"`
- `session_id_hash = "sha256:..."`

Forbidden examples:

- query text
- record body
- snippets
- source file paths
- raw provider or sidecar exception text
- OCR/transcript/tool output

## Testing

Rust tests:

- `cairn-core` unit tests for `SreReport` serialization and privacy-safe enum
  round trips.
- `cairn-cli` tests for `cairn admin sre report --json` using a temp vault and
  fixture metrics.
- `cairn-store-sqlite` tests for workflow summary queries by kind and state.
- `cairn-bench` tests for `sre --fixtures-only`, migration backlog failure,
  cold rehydrate comparator failure, and privacy scrub failure.
- `cairn-desktop` tests for `GET /api/v1/sre`.

Frontend tests:

- API client test for `sre()`.
- Component tests for warning/fail status rendering.
- Navigation test switching between Records and SRE.
- Layout regression smoke through `npm run build` and `npm test`.

Verification commands:

```bash
cargo nextest run -p cairn-core -p cairn-store-sqlite -p cairn-cli -p cairn-bench -p cairn-desktop
cargo run -p cairn-bench -- sre --fixtures-only
npm --prefix frontend/desktop-electron test
npm --prefix frontend/desktop-electron run build
```

## Rollout

1. Add the shared model and report builder with fixture tests.
2. Add `cairn admin sre report`.
3. Add `cairn-bench sre` and CI/release wiring.
4. Add desktop backend endpoint.
5. Add Electron SRE workspace.
6. Run the full verification set and update generated command docs if the
   repo's docgen requires it.

The implementation should be test-first. Production code for each report
section should follow a failing test that proves the body-free report shape,
classification, or gate behavior.

## Open Decisions Resolved

- Dashboard placement: dedicated SRE workspace in Electron.
- CLI command namespace: `admin sre report`, because this is an operator
  surface.
- Hosted dashboard: out of scope for this issue.
- Release gate strictness: fixture checks run on every PR; cold rehydrate SLO
  and migration backlog fail release checks.
