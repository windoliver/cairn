# Issue #99 — Latency, Memory Budget, and Privacy Regression Gates

- **Issue**: [#99](https://github.com/windoliver/cairn/issues/99) — [P0] Add latency, memory budget, and privacy regression gates
- **Parent epic**: [#18](https://github.com/windoliver/cairn/issues/18) — P0 evaluation harness, benchmarks, and release packaging
- **Depends on**: [#97](https://github.com/windoliver/cairn/issues/97) (closed, replay harness shipped in PR #375)
- **Brief sources**: §15 Evaluation, §19 Sequencing / Working-set budget, §14 Privacy and Consent
- **Phase**: v0.1 minimum substrate, P0
- **Date**: 2026-05-15

---

## 1. Goal

Add three independent regression gates wired into `ci.yml` as one new required check:

1. **Latency** — brief §15 SLOs (p95 hot-prefix < 50 ms, search < 50 ms, retrieve < 5 ms, etc.) plus the §15 "2% regression" rule against a committed baseline.
2. **Memory budget** — brief §19 working-set budget (~160 MB always-on default, ~660 MB with screenpipe) enforced as static artifact size.
3. **Privacy regression** — fixture-driven check that forgotten or redacted content never appears in `search`, `retrieve`, vault markdown, or SQLite indexes.

The gates run on every PR (lightweight subset) and on tagged releases (full lifecycle suite). Reports are committed-by-reference JSON uploaded as CI artifacts. Thresholds are documented and adjustable by release owners.

## 2. Non-goals

- **Runtime RSS sampler.** Static binary + bundled-asset size only; runtime memory profiling waits for stable platform numbers.
- **Public SRE dashboards.** Brief §15 puts these on the P1 SRE surface; issue #99 explicitly calls them out as out of scope.
- **Multi-session coherence benchmarks** (`cairn bench` user-facing binary) — brief §19 schedules these for v0.2.
- **Coherence metric regression (orphans / conflicts / staleness).** Separate issue.
- **Windows / BSD memory budget.** P0 ships Linux + macOS only, matching §16 distribution channels.
- **Cassette-based replay coherence gates.** Different surface; tracked under §15 future work.

## 3. Architecture

### 3.1 Crate home

Extend the existing `cairn-bench` crate. Today it exposes a `cairn-bench` binary that runs a BrainBench retrieval-quality scorecard. We add three sibling subcommands on the same binary:

```
cairn-bench scorecard   # existing: BrainBench retrieval quality (unchanged)
cairn-bench latency     # NEW: criterion-driven SLO + regression gate
cairn-bench memory      # NEW: binary + model size budget gate
cairn-bench privacy     # NEW: forget/redact/consent leakage fixtures
cairn-bench all         # NEW: runs latency + memory + privacy; exit non-zero on any fail
```

No new workspace crate. `cairn-bench` remains dev tooling per its existing manifest description.

### 3.2 Module layout

```
crates/cairn-bench/
├── Cargo.toml
├── baselines/
│   ├── latency.linux.json          # committed; refresh via --refresh-baseline
│   └── latency.macos.json
├── benches/
│   ├── latency.rs                  # criterion harness, in-process via cairn-sdk
│   └── lifecycle.rs                # release-only: cold-rehydrate, 1M-record forget
├── fixtures/
│   └── privacy/
│       ├── forget_record_visibility.yaml
│       ├── forget_record_index_purge.yaml
│       ├── redaction_pii_search.yaml
│       ├── redaction_pii_retrieve.yaml
│       ├── redaction_pii_markdown.yaml
│       ├── consent_visibility_gating.yaml
│       ├── consent_revoke_invalidates.yaml
│       └── forget_does_not_leak_body_in_logs.yaml
├── manifests/
│   └── memory.toml
└── src/
    ├── main.rs                     # clap dispatch (existing + 3 new subcommands)
    ├── scorecard/                  # existing files moved under a module
    ├── latency/
    │   ├── mod.rs                  # subcommand entrypoint
    │   ├── harness.rs              # invokes criterion benches, parses JSON
    │   └── thresholds.rs           # §15 SLO constants
    ├── memory/
    │   ├── mod.rs
    │   ├── manifest.rs             # parse memory.toml, profile extension
    │   └── sizer.rs                # binary + asset summation
    ├── privacy/
    │   ├── mod.rs
    │   ├── fixture.rs              # YAML loader + assertion DSL
    │   └── harness.rs              # runs a fixture against a temp vault
    └── gates/
        ├── baseline.rs             # shared: load/save baseline.json
        ├── report.rs               # shared: JSON + human report writer
        └── thresholds.rs           # shared: brief-derived constants
```

### 3.3 Reports

Each subcommand writes a JSON report to `target/cairn-bench/<gate>.json` and prints a human summary to stdout.

Exit codes:

- `0` — all checks passed
- `1` — one or more checks failed
- `2` — baseline file missing (latency gate) or manifest missing (memory gate)
- `3` — internal harness error (panic-caught, surfaced as exit code)

## 4. Latency gate

### 4.1 What gets measured

Criterion benches in `crates/cairn-bench/benches/latency.rs`, driven from inside the `cairn-bench latency` subcommand. Criterion runs as a library so the gate can read the JSON output and compare to the baseline.

**Driver — subprocess via `cairn` CLI.** Each bench iteration spawns the release-built `cairn` binary against a temp vault and waits for it to exit. Rationale: as of the v0.1 codebase, the `cairn-sdk` Rust API returns `Unimplemented` for `assemble_hot`, `retrieve --record`, `capture_trace`, `forget`, and `lint` — these verbs are only wired end-to-end through the CLI (issue #193 will land an SDK-wired path; gates flip to in-process at that point). Subprocess wall-clock includes process spawn + tokio runtime init (~100-200 ms cold, ~20-50 ms warm); the gate amortizes this by reusing the same vault across iterations and using criterion's warm-up sample window.

| Bench | CLI command | §15 SLO (in-process) | Subprocess SLO (proxy) |
|---|---|---|---|
| `assemble_hot_p95` | `cairn assemble_hot --json` | < 50 ms p95 | < 300 ms p95 |
| `search_keyword_p95` | `cairn search <q> --mode keyword --json` | < 50 ms p95 | < 300 ms p95 |
| `search_semantic_p95` | `cairn search <q> --mode semantic --json` (with `CAIRN_MOCK_EMBEDDER=1`) | < 50 ms p95 | < 300 ms p95 |
| `search_hybrid_p95` | `cairn search <q> --mode hybrid --json` (with `CAIRN_MOCK_EMBEDDER=1`) | < 50 ms p95 | < 300 ms p95 |
| `retrieve_p95` | `cairn retrieve --id <id> --json` | < 50 ms p95 (§15 umbrella); §19.a aspirational p50 < 5 ms is advisory | < 300 ms p95 |
| `capture_trace_p95` | `cairn capture_trace --from <event-file> --json` | < 50 ms p95 | < 300 ms p95 |
| `wal_apply_p95` | `cairn ingest --kind reference --body <s> --json` | < 50 ms p95 | < 300 ms p95 |
| `workflow_enqueue_p95` | `cairn capture_trace --from <stop-event> --json` (Stop event triggers rolling-summary enqueue) | < 50 ms p95 | < 300 ms p95 |

The headline brief §15 50 ms SLO is the underlying contract. The gate's hard SLO is the subprocess proxy (~300 ms p95, matching the precedent set by `crates/cairn-cli/tests/hot_prefix_latency_smoke.rs:88`). Brief §15 also requires "> 2% regression fails build" — that rule applies to the subprocess proxy too (regression detection is unit-independent), and is the load-bearing guard for day-to-day PRs. When issue #193 wires the SDK end-to-end, the latency gate flips to in-process and the proxy threshold is removed.

Lifecycle SLOs (cold-rehydration < 3 s, forget 1M-record < 1 s/30 s) live in `benches/lifecycle.rs` and run only on `release-dry-run.yml` because the setup cost is too high for every PR.

### 4.2 Vault

Reuse the P0 replay fixture vault from `cairn-test-fixtures`. Seeded vault is created once per bench iteration via `tempfile::tempdir`.

### 4.3 Criterion config

`Criterion::default().measurement_time(Duration::from_secs(8)).sample_size(20).warm_up_time(Duration::from_secs(2))` — keeps each bench under ~10 s, total ~80 s wall for the 8 benches. The sample size starts at 20 (not criterion's default 100) because the subprocess driver caps useful throughput; tune up later if regression noise on CI runners exceeds the 2% threshold.

### 4.4 Comparator

`cairn-bench latency` reads the criterion JSON output for each bench, then for each metric:

1. **Absolute SLO** — measured p95 must be < §15 threshold. Fails with `SLOExceeded { bench, measured_ms, slo_ms }`.
2. **Regression vs baseline** — measured p95 must be ≤ `baseline.metrics[bench] * 1.02`. Fails with `RegressionExceeded { bench, measured_ms, baseline_ms, threshold_ms }`.
3. **Improvement hint (advisory)** — if measured < baseline × 0.90, prints a suggestion to refresh the baseline. Does not fail.

### 4.5 Baseline file

`crates/cairn-bench/baselines/latency.<linux|macos>.json` (committed). One file per runner profile. Two profiles ship in P0: `linux` (matches `ubuntu-latest`) and `macos` (matches `macos-latest`). The gate picks the right file from `$RUNNER_OS`; a missing baseline returns exit code 2.

Shape:

```json
{
  "schema_version": 1,
  "runner": "ubuntu-latest-github-actions",
  "captured_at": "2026-05-15T00:00:00Z",
  "commit": "...",
  "metrics": {
    "assemble_hot_p95_ms": 4.2,
    "search_keyword_p95_ms": 6.1,
    "search_semantic_p95_ms": 12.7,
    "search_hybrid_p95_ms": 14.1,
    "retrieve_p95_ms": 1.3,
    "capture_trace_p95_ms": 5.8,
    "wal_apply_p95_ms": 3.0,
    "workflow_enqueue_p95_ms": 1.9
  }
}
```

Optional per-metric `regression_pct` override fields are accepted for cases where a metric is genuinely noisier than 2% on a particular runner. Default 2% remains in effect when unset.

### 4.6 Refresh flow

`cairn-bench latency --refresh-baseline` runs all benches and rewrites the matching baseline file with current numbers, the captured commit SHA, and the timestamp. Maintainers refresh in a dedicated PR titled `chore(bench): refresh latency baseline`; reviewers inspect the diff.

### 4.7 Report shape

`target/cairn-bench/latency.json`:

```json
{
  "schema_version": 1,
  "runner": "ubuntu-latest-github-actions",
  "commit": "...",
  "captured_at": "2026-05-15T00:00:00Z",
  "metrics": [
    {
      "bench": "assemble_hot_p95",
      "measured_ms": 4.31,
      "slo_ms": 50,
      "baseline_ms": 4.2,
      "regression_threshold_ms": 4.284,
      "slo_ok": true,
      "regression_ok": true
    }
  ],
  "ok": true,
  "failures": []
}
```

## 5. Memory budget gate

### 5.1 What gets measured

Static artifact sizes only. No runtime RSS sampling.

**P0 scope.** Brief §19 budgets include four categories of asset: the Rust core binary, the embedding model, sherpa-onnx runtime + models, and the default screen backend (xcap + OS OCR). Of these, only the first two are inspectable from a clean `cargo build --release` + `cairn bootstrap` cycle on a CI runner — sherpa-onnx and screen backends ship as external runtime deps without a stable public path helper at v0.1. The P0 gate therefore enforces only the binary + downloaded embedding model; the other categories ship as a follow-up issue (filed during the implementation PR). The manifest format below already supports them so the follow-up is a one-file edit.

| Component | Source | §19 budget | P0 enforced? |
|---|---|---|---|
| Rust core binary | `target/release/cairn` (stripped) | ~15 MB design / ~36 MB measured | yes |
| Embedding model | model file under `.cairn/models/` after first `cairn bootstrap` | ~25 MB design / ~128 MB measured | yes |
| sherpa-onnx runtime + models | external runtime dep, no stable path helper today | ~100 MB | no (deferred) |
| Default screen backend | `xcap` + OS OCR (0 on macOS Vision, ~20 MB Linux tesseract) | ~20 MB | no (deferred) |
| Screenpipe (opt-in) | counted only under `--features screenpipe-runtime` | ~500 MB | no (deferred) |
| **Always-on default total (P0 gate)** | binary + model | **~164 MB measured** | yes |
| **Brief §19 design target (full system)** | sum of all four | ~160 MB | informational |

The measured numbers diverge from the brief §19 design figures: the binary (release build with statically-linked `sqlite-vec` + `candle`) ships at ~36 MB rather than 15 MB, and the default embedding model (`bge-small-en-v1.5` ONNX) is ~128 MB rather than 25 MB. Total binary+model at ~164 MB lands almost exactly at the brief §19 ~160 MB working-set target — the per-component allocation is what shifted, not the aggregate. Brief §19 should be updated in a follow-up to reflect the measured per-component numbers; the gate's budget anchors on the measured reality.

### 5.2 Mechanism

`cairn-bench memory` runs in two phases:

1. **Build.** Spawn `cargo build --release -p cairn-cli --locked` (and `--features screenpipe-runtime` for the second profile). Captures `target/release/cairn` size after strip.
2. **Sum.** Walk asset paths declared in `crates/cairn-bench/manifests/memory.toml`. Asset paths are resolved by reusing the production code paths (`cairn-embeddings-local::ensure_model` etc.) so the manifest and the runtime stay aligned.

### 5.3 Manifest

`crates/cairn-bench/manifests/memory.toml`. The manifest schema accepts both enforced and deferred entries; deferred entries carry `enforced = false` and are not counted against the total or per-asset tolerance.

```toml
[profile.default]
binary = "target/release/cairn"
assets = [
  { source = "embedding_model", expected_mb = 25, enforced = true },
  { source = "sherpa_onnx_voice", expected_mb = 100, enforced = false },
  { source = "screen_default", expected_mb = 20, enforced = false },
]
budget_mb = 200       # P0 enforced ceiling (~164 MB measured + headroom). Brief §19 ~160 MB design target.

[profile.screenpipe]
extends = "default"
features = ["screenpipe-runtime"]
assets_add = [
  { source = "screenpipe_runtime", expected_mb = 500, enforced = false },
]
budget_mb = 200
```

### 5.4 Per-asset check

Each asset has an `expected_mb` and a ±25% tolerance band. Fails with `AssetBudgetExceeded { asset, size_mb, expected_mb, tolerance }`. Catches the "one fat new dep snuck in" failure mode even when the total still fits.

### 5.5 Total check

Sum must be ≤ `budget_mb`. Fails with `TotalBudgetExceeded { total_mb, budget_mb }`.

### 5.6 CI cost

Per-PR runs the `default` profile on Linux only. The `screenpipe` profile and macOS coverage run on `release-dry-run.yml`. Linux release build with cargo cache is ~3 min.

### 5.7 Adjustability

`manifests/memory.toml` is the operator-tunable surface mentioned in the acceptance criterion. Bumping a threshold is a one-file PR with a diff explaining why; same PR updates brief §19 numbers if relevant.

### 5.8 Report shape

`target/cairn-bench/memory.json`:

```json
{
  "schema_version": 1,
  "profile": "default",
  "budget_mb": 200,
  "total_mb": 158.3,
  "assets": [
    {"name": "binary", "size_mb": 14.9, "expected_mb": 15, "tolerance_pct": 25, "ok": true},
    {"name": "embedding_model", "size_mb": 24.8, "expected_mb": 25, "tolerance_pct": 25, "ok": true}
  ],
  "ok": true,
  "failures": []
}
```

## 6. Privacy regression gate

### 6.1 Leak vectors

| Vector | Read surface | Leak definition |
|---|---|---|
| `search` | `cairn-sdk::search` (keyword + semantic + hybrid) | Forgotten/redacted record id, body, or terms appear in result set |
| `retrieve` | `cairn-sdk::retrieve` by id | Returns body of a forgotten record, or returns un-redacted PII |
| `markdown` | vault files under `raw/` / `wiki/` / `skills/` | Forgotten record's file still present with body, or unmasked PII spans on disk |
| `indexes` | direct SQL probes against `.cairn/cairn.db` (FTS5 `record_fts`, `sqlite-vec` table, `record_body`) | Row references the forgotten `record_id`, or FTS token from a redacted span exists |

### 6.2 Fixture format

YAML, one file per scenario under `crates/cairn-bench/fixtures/privacy/`:

```yaml
scenario: forget_record_visibility
description: §5.6 Phase A — forgotten record invisible to search/retrieve/markdown
setup:
  records:
    - id: rec_001
      kind: semantic
      body: "Acme Corp roadmap Q3"
      visibility: project
    - id: rec_002
      kind: reference
      body: "Public doc no leak"
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

### 6.3 P0 fixture suite

| Fixture | Covers |
|---|---|
| `forget_record_visibility.yaml` | §5.6 Phase A reader-invisible after `forget --mode record` |
| `forget_record_index_purge.yaml` | §5.6 Phase B physical purge — vec/FTS rows removed |
| `redaction_pii_search.yaml` | §14 Presidio pre-persist — emails/SSNs masked in search results |
| `redaction_pii_retrieve.yaml` | Same record retrieved returns masked body |
| `redaction_pii_markdown.yaml` | Vault file on disk contains masked spans only |
| `consent_visibility_gating.yaml` | Records scoped `project` not visible from `private`-tier search |
| `consent_revoke_invalidates.yaml` | After consent revoke, prior shared-tier record no longer surfaces |
| `forget_does_not_leak_body_in_logs.yaml` | `metrics.jsonl` and `tracing` output never contain raw body |

### 6.4 Harness mechanism

`cairn-bench privacy`:

1. For each fixture: create temp vault via `cairn-test-fixtures`, apply `setup.records` via `cairn-sdk::ingest`.
2. Apply `operations` (forget / redact / revoke) via SDK.
3. Wait for §5.6 Phase B to complete (poll `wal_ops` table for terminal state, max 5 s).
4. Run each assertion. Search/retrieve via SDK; markdown via direct file probe; indexes via `rusqlite` open of `.cairn/cairn.db`.
5. Report per-fixture pass/fail with the specific assertion that failed.

`cairn-bench privacy --check` parses all fixtures without running them; wired into CI to catch malformed YAML fast.

### 6.5 Failure policy

A single failed assertion fails the whole privacy gate. No advisory mode — privacy regressions are non-negotiable per brief §14.

### 6.6 Report shape

`target/cairn-bench/privacy.json`:

```json
{
  "schema_version": 1,
  "fixtures_run": 8,
  "fixtures_passed": 8,
  "ok": true,
  "failures": []
}
```

On failure each `failures[]` entry carries `{scenario, surface, query_or_id, expected, actual}`.

## 7. CI wiring

### 7.1 New required job in `ci.yml`

Two matrix entries, two slightly different command lines so per-PR macOS skips the expensive release build:

```yaml
gates:
  name: gates / latency + memory + privacy
  strategy:
    matrix:
      include:
        - runner: ubuntu-latest
          cmd: "all"                                   # latency + memory + privacy
        - runner: macos-latest
          cmd: "all --skip memory"                     # latency + privacy only
  runs-on: ${{ matrix.runner }}
  steps:
    - uses: actions/checkout@<pinned-sha>
    - run: rustup show active-toolchain || rustup toolchain install
    - uses: actions/cache@<pinned-sha>
    - run: cargo run -p cairn-bench --release --locked -- ${{ matrix.cmd }}
    - uses: actions/upload-artifact@<pinned-sha>
      if: always()
      with:
        name: bench-reports-${{ matrix.runner }}
        path: target/cairn-bench/*.json
```

`cairn-bench all [--skip <gate>]` runs the remaining gates in latency → memory → privacy order and returns the worst exit code. Reports always upload, even on failure.

macOS memory budget (default + screenpipe profiles) runs via `release-dry-run.yml`, where a release build already happens anyway.

### 7.2 Update `docs/ci.md`

New row in the required-status-checks table:

```
| `gates / latency + memory + privacy` (`ci.yml`) | ✅ required | Brief §15 SLO + 2% regression, §19 working-set budget, §14 leakage fixtures. Reports in `bench-reports-<runner>` artifact. |
```

Add matching row to the local-equivalents block and to the CLAUDE.md §8 verification checklist:

```bash
cargo run -p cairn-bench --release --locked -- all
cargo run -p cairn-bench --release --locked -- latency        # one gate at a time
cargo run -p cairn-bench --release --locked -- memory
cargo run -p cairn-bench --release --locked -- privacy
```

### 7.3 Release-only extras

In `release-dry-run.yml`:

- `cairn-bench memory --profile screenpipe` (heavy budget, expensive build).
- `cairn-bench lifecycle` (cold-rehydration, 1M-record forget — too slow per-PR).

## 8. Operator workflows

### 8.1 Baseline refresh

1. Maintainer runs locally: `cargo run -p cairn-bench --release -- latency --refresh-baseline`.
2. Diffs the matching `baselines/latency.<os>.json`. Sanity-checks that no metric got worse for unrelated reasons.
3. Opens a dedicated PR `chore(bench): refresh latency baseline`. Reviewer signs off on the new numbers.

### 8.2 Memory budget raise

1. New dep / new asset legitimately raises a budget.
2. Edit `crates/cairn-bench/manifests/memory.toml`, bump the affected `budget_mb` or `expected_mb`.
3. Same PR updates brief §19 numbers and CLAUDE.md if relevant. Brief and gate stay in lockstep.

### 8.3 New privacy fixture

1. Add YAML under `crates/cairn-bench/fixtures/privacy/`.
2. Run `cargo run -p cairn-bench -- privacy` locally — confirm it fails on current code (reproduces the leak).
3. Fix the leak.
4. Same PR lands the fixture + the fix.

## 9. Testing strategy

### 9.1 Testing the gate harness itself

| Layer | Location | Coverage |
|---|---|---|
| Unit | `cairn-bench/src/gates/baseline.rs` `#[cfg(test)]` | Baseline JSON round-trip, regression math (2% rule), runner profile selection from `$RUNNER_OS` |
| Unit | `cairn-bench/src/memory/manifest.rs` | Manifest parse, profile extension, asset tolerance band, `TotalBudgetExceeded` vs `AssetBudgetExceeded` discrimination |
| Unit | `cairn-bench/src/privacy/fixture.rs` | YAML fixture parse, assertion DSL parse, failure-report shape |
| Integration | `cairn-bench/tests/latency_smoke.rs` | One tiny criterion bench end-to-end against an in-memory vault; assert report written, exit code matches when baseline is tighter than measured |
| Integration | `cairn-bench/tests/memory_smoke.rs` | Synthetic manifest pointing at a temp file of known size; assert pass + fail paths |
| Integration | `cairn-bench/tests/privacy_smoke.rs` | Run one fixture against a real seeded vault via `cairn-test-fixtures`; inject a deliberately broken `forget` via a mock store and assert the gate fails with the expected surface |
| Snapshot (insta) | `cairn-bench/tests/snapshots/` | Human summary output stable; CI artifact JSON shape stable |

### 9.2 Determinism + offline discipline

- Every code path runs with the existing offline flag set; no LLM, no network calls.
- Latency benches use the same fixture vault every run.
- Privacy fixtures are fully declarative.
- Memory manifest is committed.
- Only nondeterminism is runner-class clock noise, absorbed by the 2% threshold and the ±25% asset tolerance band.

## 10. Invariants touched

From CLAUDE.md §4:

- **Invariant 2 (stand-alone P0).** All gate code runs offline, no network. Asset paths reuse production resolution code paths so the offline contract holds.
- **Invariant 3 (CLI is ground truth).** Benches drive the `cairn` CLI subprocess directly (in v0.1; flips to in-process via `cairn-sdk` once issue #193 lands the SDK-wired verb paths). No parallel verb implementation in either mode.
- **Invariant 4 (seven contracts).** Privacy gate exercises `MemoryStore::forget` + `MemoryStore::search` + `MemoryStore::retrieve` only; no new contract.
- **Invariant 8 (no `unwrap` in `cairn-core`).** Gate code lives in `cairn-bench`, not `cairn-core`. `unwrap` rules for bins/tests apply.
- **Invariant 9 (privacy by construction).** Privacy gate is the regression net for this invariant — fixtures fail if a leak appears.

## 11. Open questions

None remaining after Section 5 design review. Items deferred to implementation:

- Exact criterion sample count tuning per bench (start at 50, adjust during baseline capture).
- Whether asset tolerance band needs to be per-asset rather than uniform 25% (decide after first baseline pass).
