# Coherence Benchmarks & Release Gate — Design

**Issue**: [#137](https://github.com/windoliver/cairn/issues/137) — `[P3] Harden coherence benchmarks and release gates`
**Parent**: [#31](https://github.com/windoliver/cairn/issues/31) — `[P3] Harden replay cassettes, coherence benchmarks, and documentation freeze`
**Dependencies**: [#136](https://github.com/windoliver/cairn/issues/136) (closed) — extended replay cassettes for research, engineering, support domains
**Phase**: v0.4 (Evaluation + docs freeze) · priority P3
**Brief sections**: §15 Evaluation, §18 Success Criteria
**Status**: Spec — pending implementation plan

---

## 1. Goal

Add a deterministic, offline coherence benchmark suite that scores the existing extended replay cassettes (#136) along five named metrics, gates beta and release-candidate builds on per-metric thresholds plus regression deltas, and persists a versioned trend file that survives schema changes. The gate must fail closed when a metric drops below its floor or regresses more than 2 % from the committed baseline.

This satisfies brief §18 success criterion #5 ("Golden queries + multi‑session coherence + orphan / conflict / staleness metrics all regression‑tested in CI") and operationalises the §15 CI gate rule ("fails build if any metric drops > 2% or a golden query breaks") for the multi‑session coherence dimension.

## 2. Non‑goals

- LLM‑judged scoring. Brief §15 mandates the eval gate runs with no LLM and no network. All five metrics are deterministic ratios of golden expectations.
- Latency or privacy SLOs. Those live in `cairn-bench` already (`benches/latency.rs`, `tests/privacy_smoke.rs`) and are owned by separate issues.
- Orphan / conflict / staleness metrics from the broader §15 ecosystem. Those are surfaced by `EvaluationWorkflow` and the `lint` verb; the coherence gate consumes their existing outputs only insofar as `stale_avoidance` checks cassette‑declared stale record IDs.
- New cassette content. #136 landed the three extended domains; this design tags them but does not add domains.
- A separate `cairn-bench-coherence` crate. The work fits as a module under `cairn-bench`.

## 3. Source of truth in the brief

| Brief excerpt | This design's response |
|---|---|
| §15: "Replay engine — deterministic, no LLM, no network" | Coherence runs on top of the existing `cairn-test-fixtures::replay` engine. No new runtime path. |
| §15: "fails build if any metric drops > 2% or a golden query breaks" | Per‑metric `max_drop_pct = 2.0` (0.0 for `forget_completeness`); gate exits 69 on failure. |
| §15: "Multi‑session coherence. Long‑horizon tests spanning 5 / 10 / 50 sessions" | All five metrics are scored over the #136 extended cassettes, which already span multiple sessions per domain. |
| §18 #5: "regression‑tested in CI" | New CI job `coherence-gate` runs on every PR (`--gate beta`) and on `release/*` branches (`--gate rc`). |
| §18 #4: "forget‑me…append‑only consent log survives GDPR review" | `forget_completeness` floor is 1.0 with `max_drop_pct = 0.0`. Any regression fails the gate. |

## 4. Architecture

```
crates/cairn-bench/
├── src/
│   ├── coherence/
│   │   ├── mod.rs          # public API: run_coherence_gate(opts) -> GateOutcome
│   │   ├── category.rs     # MetricCategory enum (5 variants) + Display
│   │   ├── score.rs        # ReplayReport -> CategoryScores aggregation
│   │   ├── threshold.rs    # ThresholdManifest load + evaluate(gate, scores, baseline)
│   │   ├── trend.rs        # append-only JSONL writer + per-line schema_version load
│   │   └── report.rs       # human + JSON output rendering
│   └── main.rs             # new subcommand: `cairn-bench coherence run`
├── manifests/
│   └── coherence.toml      # per-metric beta_min, rc_min, max_drop_pct
├── baselines/
│   ├── coherence.json      # current floor (single object, overwritten on --update-baseline)
│   └── coherence-trend.jsonl  # append-only run history
├── schemas/
│   ├── coherence-threshold.schema.json
│   ├── coherence-baseline.schema.json
│   └── coherence-trend.schema.json
└── tests/
    └── coherence_smoke.rs  # smoke: load extended cassettes + assert gate

crates/cairn-test-fixtures/src/replay.rs
└── + optional metric_category field on each ReplayAction variant
   (serde(default), Option<MetricCategory>, no breaking change)
└── + optional stale_record_ids on ReplaySearchAction (default empty)

fixtures/v0/replay/{research,engineering,support}_domain.json
└── metric_category tags backfilled in this PR

ci.yml
└── new job coherence-gate:
    PR / main:   coherence run --gate beta
    release/*:   coherence run --gate rc
```

**Dependency direction**: `cairn-bench` already depends on `cairn-test-fixtures` (dev‑dep). The coherence module adds no workspace‑crate dependencies. `cairn-core` is not touched, preserving the §6 boundary rule.

**Why a module not a crate**: per CLAUDE.md §6.7 "new dep = justify in PR" and the design brief's preference for collapsing tooling rather than adding crates. The coherence module is one consumer of the replay harness, not a distinct contract.

## 5. Metric definitions

All five metrics are ratios of cassette‑declared golden expectations satisfied. Inputs come from a single `ReplayReport` produced by the existing harness, partitioned by a new optional `metric_category` field on each action.

### 5.1 Metric categorization

A new optional field is added to every `ReplayAction` variant:

```rust
#[serde(default)]
pub metric_category: Option<MetricCategory>,
```

where:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricCategory {
    RecallPrecision,
    StaleAvoidance,
    SummaryQuality,
    SearchUsefulness,
    ForgetCompleteness,
}
```

The field is `Option` so existing P0 cassettes (`codex_consumer`, `p0_stories`, `p0_keyword_only`) stay valid with `serde(deny_unknown_fields)` and are excluded from coherence scoring. They keep running unchanged as pure exact‑match golden checks under `cargo nextest`.

Extended cassettes from #136 receive backfilled tags in this PR. The tagging is mechanical: action verb → default category (see table below), overridden inline when the cassette needs a different category for that specific action.

| Action verb | Default category |
|---|---|
| `RetrieveSession`, `RetrieveTurn`, `AssembleHot`, `CaptureTrace`, `RecordPresent (expected_present=true)` | `RecallPrecision` |
| `Summarize` | `SummaryQuality` |
| `Search` | `SearchUsefulness` (auto‑switches to `StaleAvoidance` when `stale_record_ids` is non‑empty — see §5.2) |
| `ForgetRecord` | `ForgetCompleteness` |
| `Lint`, `RecordPresent (expected_present=false)` | unset — excluded from coherence |

### 5.2 Stale‑sensitive search

A new optional field on `ReplaySearchAction`:

```rust
#[serde(default)]
pub stale_record_ids: Vec<String>,
```

When non‑empty, the action is automatically classified as `StaleAvoidance` (overriding any explicit `metric_category`) and the pass condition switches from "top hit matches expected" to "none of the returned record IDs intersect `stale_record_ids`". The two semantics never overlap on a single action.

`StaleAvoidance` is scoped to `Search` actions only in this PR. Hot‑memory stale‑leak detection (an `AssembleHot` variant of the same idea) is a sensible follow‑up but would require lifting `stale_record_ids` onto every relevant action variant; deferred to keep the field's domain narrow.

### 5.3 Per‑metric definitions

| Metric | Pass condition for one action | Score formula |
|---|---|---|
| `recall_precision` | Action passed (existing golden match) | `passed / total` over actions tagged `RecallPrecision` |
| `stale_avoidance` | `actual.record_ids` disjoint from `stale_record_ids` (Search actions only) | `passed / total` over `StaleAvoidance` actions |
| `summary_quality` | `actual.record_ids` set‑equals `expected.record_ids` | `passed / total` over `SummaryQuality` actions |
| `search_usefulness` | `actual.status == "hits"` AND `actual.record_ids[0] == expected.record_ids[0]` (top‑1) | `passed / total` over `SearchUsefulness` actions |
| `forget_completeness` | `retrieve_found == false` AND follow‑up search excludes record (existing golden match) | `passed / total` over `ForgetCompleteness` actions |

`coherence_overall = mean(5 per-category scores)` is computed for trend display only. It is never gated on directly.

### 5.4 Empty categories

A category with zero tagged actions yields `score = 1.0, passed = 0, total = 0`. This is treated as a "vacuous pass" and never trips the gate — the alternative (failing on missing coverage) creates a chicken‑and‑egg for newly added categories. Coverage of the five categories is asserted separately in `coherence_smoke.rs` to prevent silent regressions where all tags get removed.

## 6. Thresholds and gate

### 6.1 Manifest

`crates/cairn-bench/manifests/coherence.toml`:

```toml
schema_version = 1

[recall_precision]
beta_min   = 0.90
rc_min     = 0.95
max_drop_pct = 2.0

[stale_avoidance]
beta_min   = 0.95
rc_min     = 0.98
max_drop_pct = 2.0

[summary_quality]
beta_min   = 0.85
rc_min     = 0.90
max_drop_pct = 2.0

[search_usefulness]
beta_min   = 0.85
rc_min     = 0.90
max_drop_pct = 2.0

[forget_completeness]
beta_min   = 1.00
rc_min     = 1.00
max_drop_pct = 0.0
```

Floors above are placeholders chosen conservatively; the first PR run on real cassettes establishes the actual baseline, and the manifest is tightened in a follow‑up only if the observed scores comfortably exceed the placeholders. Bumping any floor downward requires an explicit reason in the PR description.

### 6.2 Gate evaluation

For each metric, given gate mode `g ∈ {none, beta, rc}`, computed score `s`, and (optional) prior baseline score `b`:

1. `g == none` → record, never fail.
2. Floor check: `s < g_min` → fail (`belowFloor`).
3. Delta check (only if `b` is present): `(b - s) > max_drop_pct / 100` → fail (`exceededDrop`).
4. Otherwise → pass.

Gate outcome is the union of per‑metric outcomes. The process exits non‑zero on any failure.

### 6.3 Privacy non‑negotiable

`forget_completeness` has `rc_min = 1.00` and `max_drop_pct = 0.0`. This binds the coherence gate to brief §18 #4 ("Privacy") and §14 (consent). Any tombstone or reader‑fence regression fails the gate immediately, even on a development branch with `--gate beta` — because `beta_min = 1.0` too.

### 6.4 Baseline lifecycle

- First PR run finds no `coherence.json` → only floor check runs, delta check is skipped, and the PR commits the initial baseline.
- Baseline is updated only by an explicit human invocation of `cairn-bench coherence run --update-baseline` followed by committing the resulting `coherence.json`. CI never passes the flag — neither on PRs nor on `release/*` branches. Floor movement is a deliberate code change with a PR reviewer.
- The `--update-baseline` flag rewrites `coherence.json` in place (atomic temp‑file + rename) and is the only path that mutates the baseline. The trend file is always appended on every run regardless of `--update-baseline`.

### 6.5 Exit codes

Following CLAUDE.md §6.5 (sysexits‑style):

| Exit | Meaning |
|---|---|
| `0` | All gates passed (or `--gate none`) |
| `69` (`EX_UNAVAILABLE`) | One or more metric gates failed |
| `78` (`EX_CONFIG`) | Bad manifest, missing/corrupt baseline schema, malformed cassette |
| `1` | Runtime error (I/O, store, harness panic) |

## 7. Trend persistence

### 7.1 Baseline file

`crates/cairn-bench/baselines/coherence.json` — single object, schema‑versioned, overwritten on `--update-baseline`:

```json
{
  "schema_version": 1,
  "captured_at": "2026-05-24T12:00:00Z",
  "cairn_version": "0.0.0",
  "git_sha": "241d1820",
  "metrics": {
    "recall_precision":    { "score": 0.94, "passed": 47, "total": 50 },
    "stale_avoidance":     { "score": 0.98, "passed": 49, "total": 50 },
    "summary_quality":     { "score": 0.92, "passed": 23, "total": 25 },
    "search_usefulness":   { "score": 0.88, "passed": 22, "total": 25 },
    "forget_completeness": { "score": 1.00, "passed": 10, "total": 10 }
  }
}
```

### 7.2 Trend file

`crates/cairn-bench/baselines/coherence-trend.jsonl` — append‑only, one line per run:

```jsonl
{"schema_version":1,"run_id":"01J…","ts":"2026-05-24T12:00:00Z","cairn_version":"0.0.0","git_sha":"241d1820","gate":"beta","outcome":"pass","metrics":{…same shape as baseline.metrics…}}
{"schema_version":1,"run_id":"01J…","ts":"2026-05-25T08:00:00Z","cairn_version":"0.0.0","git_sha":"abc12345","gate":"rc","outcome":"fail","failures":["search_usefulness"],"metrics":{…}}
```

JSONL is chosen over a JSON array because each append is a single `write` syscall — no read‑modify‑write race when two concurrent bench runs share the working tree. Each line is human‑diffable in PRs.

### 7.3 Versioning and migration

Every line is self‑describing via `schema_version`. The loader dispatches per line:

```rust
fn load_trend(path: &Path) -> Result<Vec<TrendEntry>, TrendError> {
    let mut out = Vec::new();
    for line in read_lines(path)? {
        let raw: serde_json::Value = serde_json::from_str(&line)?;
        let version = raw.get("schema_version").and_then(Value::as_u64);
        let entry = match version {
            Some(1) => from_v1(raw)?,
            Some(other) => return Err(TrendError::UnknownSchemaVersion(other)),
            None      => return Err(TrendError::MissingSchemaVersion),
        };
        out.push(entry);
    }
    Ok(out)
}
```

When v2 lands, it adds a sibling `from_v2` and bumps the writer to emit v2; v1 lines remain readable forever. Unknown future versions fail closed (CLAUDE.md §6.2: "Fail closed on capability").

### 7.4 JSON Schemas

`crates/cairn-bench/schemas/coherence-{threshold,baseline,trend}.schema.json` — committed alongside the data. `coherence_smoke.rs` round‑trips each fixture through `jsonschema` (dev‑dep already present) to lock the shape.

### 7.5 Retention

No rotation in P0. One CI run ≈ 500 bytes; 1 MB ≈ 2000 runs ≈ years of weekly releases. Re‑evaluate at 1 MB.

## 8. CLI surface

New subcommand under the existing `cairn-bench` binary:

```
cairn-bench coherence run
  [--gate beta|rc|none]            # default: beta
  [--cassettes <dir>]              # default: fixtures/v0/replay/
  [--include <id>...]              # default: research_domain, engineering_domain, support_domain
  [--manifest <path>]              # default: crates/cairn-bench/manifests/coherence.toml
  [--baseline <path>]              # default: crates/cairn-bench/baselines/coherence.json
  [--trend <path>]                 # default: crates/cairn-bench/baselines/coherence-trend.jsonl
  [--update-baseline]              # overwrite baseline.json with this run's scores
  [--json]                         # machine-readable report on stdout
  [--no-trend-write]               # skip appending to trend.jsonl (for dry-runs)
```

### 8.1 Default `--include` set

The three extended domains from #136. P0 cassettes are excluded because they intentionally have no `metric_category` tags — including them would force every category denominator to count their actions as untagged exclusions, which has no semantic effect, but their inclusion in the default set would mislead operators into thinking they contribute to the score.

### 8.2 Human output

```
coherence gate=beta  cassettes=3  actions=160
  recall_precision      0.940  pass  (47/50)   floor=0.90  Δ=+0.005
  stale_avoidance       0.980  pass  (49/50)   floor=0.95  Δ=+0.000
  summary_quality       0.920  pass  (23/25)   floor=0.85  Δ=+0.020
  search_usefulness     0.880  pass  (22/25)   floor=0.85  Δ=−0.005
  forget_completeness   1.000  pass  (10/10)   floor=1.00  Δ=+0.000
  overall               0.944  PASS
trend appended: baselines/coherence-trend.jsonl
```

Gate failure prints the failing rows plus a `remediation:` hint pointing at `crates/cairn-bench/manifests/coherence.toml` and brief §15. TTY detection uses `std::io::IsTerminal` (CLAUDE.md §6.5) to colourise; piped output stays plain.

### 8.3 JSON output (`--json`)

```json
{
  "schema_version": 1,
  "gate": "beta",
  "outcome": "pass",
  "failures": [],
  "metrics": { … same shape as baseline.metrics … },
  "deltas":  { "recall_precision": 0.005, … },
  "trend_path": "crates/cairn-bench/baselines/coherence-trend.jsonl",
  "run_id": "01J…"
}
```

Locked by an `insta` snapshot test (`coherence_json_output_snapshot`).

## 9. CI wiring

New job in `.github/workflows/ci.yml`, running after the existing `bench-smoke` step:

```yaml
coherence-gate:
  needs: bench-smoke
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4
    - uses: dtolnay/rust-toolchain@stable
      with: { toolchain: 1.95.0 }
    - run: cargo run -p cairn-bench --release --locked -- coherence run --gate ${{ startsWith(github.ref, 'refs/heads/release/') && 'rc' || 'beta' }}
```

The job is required for merge on `main` and on any `release/*` branch. CI never writes the baseline (see §6.4); every run is read‑only against the committed floor.

`docs/ci.md` gets one paragraph in the bench section explaining the job and its exit codes.

`CLAUDE.md` §8 (verification checklist) gets one new line:

```bash
cargo run -p cairn-bench --release --locked -- coherence run --gate beta
```

## 10. Testing strategy

Test coverage is split across unit (in the coherence submodules), integration (`crates/cairn-bench/tests/coherence_smoke.rs`), and snapshot (`insta` for human + JSON output). No DB mocking — the integration tests drive the real `SqliteMemoryStore` through the replay harness, which is the whole point per CLAUDE.md §6.4 ("No mocking the DB").

| Layer | Test | What it locks down |
|---|---|---|
| Unit `score.rs` | `bucket_by_category_partitions_actions` | 5 actions tagged into 5 categories → 5 buckets with correct counts |
| Unit `score.rs` | `score_zero_total_is_vacuous_pass` | empty category → 1.0 score, never trips gate |
| Unit `score.rs` | `score_partial_pass` | 3‑of‑5 pass → 0.6 |
| Unit `threshold.rs` | `gate_pass_at_floor` | score == floor → pass |
| Unit `threshold.rs` | `gate_fail_below_floor` | score < floor → fail, category in `failures` |
| Unit `threshold.rs` | `gate_fail_on_drop` | score above floor but drop > max_drop_pct → fail |
| Unit `threshold.rs` | `gate_skips_delta_without_baseline` | missing baseline → floor check only |
| Unit `threshold.rs` | `forget_completeness_intolerant` | score 0.99 with `rc_min=1.0` → fail under both `beta` and `rc` |
| Unit `trend.rs` | `load_v1_then_unknown_fails_closed` | mixed `schema_version` lines, unknown → error |
| Unit `trend.rs` | `append_atomic_under_concurrent_writes` (proptest) | two parallel appends both land, no truncation |
| Integration | `coherence_smoke::extended_cassettes_pass_beta_gate` | all 3 #136 cassettes through real coherence pipeline → beta gate passes against committed baseline |
| Integration | `coherence_smoke::missing_category_excluded_from_scoring` | one action with `metric_category: None` → no denominator change |
| Integration | `coherence_smoke::all_five_categories_have_coverage` | the union of #136 cassettes touches all 5 categories |
| Integration | `coherence_smoke::schemas_validate_against_jsonschema` | manifest + baseline + one trend line through `jsonschema` |
| Snapshot | `coherence_human_output_snapshot` | human table format locked |
| Snapshot | `coherence_json_output_snapshot` | `--json` shape locked (downstream tooling contract) |
| CLI smoke | `cli_smoke::coherence_run_exit_code_69_on_fail` | binary invoked with crafted low baseline → exit 69 |
| CLI smoke | `cli_smoke::coherence_run_exit_code_0_on_pass` | binary invoked with committed baseline → exit 0 |

## 11. Risks and mitigations

| Risk | Mitigation |
|---|---|
| Floor placeholders in §6.1 prove wrong on first real run | Seed step in implementation plan: run once locally, commit the produced `coherence.json` as the baseline, and tighten the manifest only after observing real scores. |
| Adding `metric_category` field breaks existing fixtures (`#[serde(deny_unknown_fields)]`) | Field is `Option` with `#[serde(default)]` and lives on each action variant; existing cassettes parse unchanged. Tested in `missing_category_excluded_from_scoring`. |
| `stale_record_ids` on `ReplaySearchAction` shifts pass semantics on the same action | The two paths are mutually exclusive at the score layer: empty → SearchUsefulness, non‑empty → StaleAvoidance. The cassette author cannot accidentally double‑count. Asserted by `bucket_by_category_partitions_actions`. |
| Concurrent CI runs racing the trend JSONL | Single‑syscall append is atomic on POSIX up to PIPE_BUF, plenty for 500‑byte lines. `append_atomic_under_concurrent_writes` proptest exercises the path. |
| Schema version bumps break old trend lines | Loader dispatches per line on `schema_version`; old `from_v1` is never removed. Forward compat is the explicit promise of §7.3. |
| One‑PR scope is large (~1500 LOC + 6 fixture files) | Two natural cut points documented in §12 below; either can be invoked if review load proves too high. |

## 12. Implementation slicing (fallback)

The user picked single‑PR scope. If the PR proves too large to review, two natural splits:

- **Cut A** (recommended fallback): land the coherence module + manifest + smoke tests + tag backfill in one PR; CI wiring in a second PR. Keeps the test‑and‑fix loop tight; the second PR is mechanical.
- **Cut B**: land categorization tags + score/threshold modules in PR1; trend persistence + RC gate + CI in PR2. Cleaner separation; two review cycles.

Both cuts are non‑destructive — neither requires reverting work from the other.

## 13. Out of scope (matches issue #137)

- Marketing claims or external certification.
- LLM‑judged metrics (re‑stated from §2).
- New cassettes beyond #136 (re‑stated from §2).
- Per‑user or per‑tenant trend tracking (single global trend file).
- Public dashboards. Trend file is consumable by any downstream tool; this design ships no UI.

## 14. Traceability

- Brief §15 (Evaluation): pipeline shape, no‑LLM rule, 2 % regression rule.
- Brief §18 (Success Criteria): #4 (privacy gate) binds `forget_completeness` floor at 1.0; #5 (regression‑tested in CI) drives the CI job in §9.
- Brief §6 (taxonomy): unchanged.
- `docs/design/traceability.md`: add a row mapping §15 / §18 #5 → issue #137 → this design doc.
- CLAUDE.md §8: add the `coherence run --gate beta` line to the verification checklist.

## 15. Open questions

None blocking. One follow‑up deferred to implementation:

1. Final placeholder values in `coherence.toml` will be revised after the first real seed run.
