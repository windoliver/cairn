# Issue #83 — Hot-prefix cache invalidation, stale-context lint, assembly metrics

| | |
|---|---|
| **Issue** | https://github.com/windoliver/cairn/issues/83 |
| **Parent epic** | #14 — Hot memory assembly + auto-built user profile |
| **Depends on** | #82 (closed) — profile / pinned / playbook retrieval |
| **Closes** | #259 — hot-memory lint canary deferred-step gap |
| **Brief sections** | §7 Hot Memory · §7.1 AutoUserProfile · §15 Evaluation |
| **Phase** | v0.1 — Minimum substrate (P0) |
| **Status** | DRAFT — pending implementation plan |

---

## 1. Problem

Brief §7 specifies a per-agent hot prefix that is "cached in the hot tier" and
"re-assembled on Dream, on high-salience write, and on `SessionStart`." Today
`assemble_hot` is a pure function: every call recomputes the prefix from
SQLite + filesystem. There is no cache, no invalidation, no signal that a
forgotten record might still be served, and no metric for the brief's §15
SLO (`p95 turn latency with hot-assembly + write < 50 ms`).

Issue #83 lands three deliverables:

1. **Cache + invalidation** — a per-`(agent, recipe)` SQLite-backed cache,
   invalidated by watermark bumps on every write to a hot-prefix source class.
2. **Stale-context lint** — a real recipe walker (replacing the canary from
   #259) plus three new lint kinds: `stale_profile_line`, `broken_source_link`,
   `missing_summary`.
3. **Assembly metrics** — a `MetricsSink` contract + `JsonlMetricsSink` that
   writes one line per `assemble_hot` call to `.cairn/metrics.jsonl`,
   capturing latency, byte budget usage, and cache-hit status.

## 2. Acceptance criteria (from the issue)

1. Cache never serves forgotten or privacy-blocked content after invalidation.
2. Lint finds stale source links and over-budget prefixes.
3. Latency and budget metrics are available to the evaluation harness.

## 3. Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                        cairn-core                                │
│  ┌────────────────────────────────────────────────────────────┐  │
│  │  contract::hot_prefix_cache::HotPrefixCache  (new trait)   │  │
│  │  contract::metrics::MetricsSink              (new trait)   │  │
│  │  domain::hot_prefix::SourceClass             (new enum)    │  │
│  │  domain::hot_prefix::SourceWatermarks        (new struct)  │  │
│  │  domain::metrics::MetricEvent                (new enum)    │  │
│  │  verbs::assemble_hot::cached_assemble        (new fn)      │  │
│  │  verbs::lint::checks::hot_memory             (rewritten)   │  │
│  └────────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────┘
                              ▲       ▲
                              │       │
       ┌──────────────────────┘       └─────────────────┐
       │                                                │
┌──────┴───────────────────┐               ┌────────────┴──────────┐
│   cairn-store-sqlite      │               │      cairn-cli         │
│  - migration:             │               │  - JsonlMetricsSink    │
│    hot_prefix_cache,      │               │    (.cairn/metrics     │
│    hot_source_watermarks  │               │    .jsonl)             │
│  - SqliteHotPrefixCache   │               │  - wires sink + cache  │
│  - classify-and-bump on   │               │    into assemble_hot,  │
│    every store write      │               │    ingest, forget      │
│    (inside the WAL tx)    │               │                        │
└───────────────────────────┘               └────────────────────────┘
```

### 3.1 Boundary rules

- `cairn-core` defines traits + pure logic; **zero I/O**, satisfies the
  `check-core-boundary.sh` invariant.
- `cairn-store-sqlite` owns persistence and watermark mutation; bumps live
  inside the existing WAL state machine.
- `cairn-cli` owns the JSONL sink and wiring; the metrics file is written by
  the binary, not by the library.
- The cache is a **performance** layer; failures degrade to direct assembly.
  The watermark bump is a **correctness** layer; failures roll back the
  parent write transaction.

## 4. Data model

### 4.1 SQLite migration

`crates/cairn-store-sqlite/migrations/<NNNN>_hot_prefix_cache.up.sql`:

```sql
CREATE TABLE hot_source_watermarks (
  class TEXT PRIMARY KEY,
  watermark INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL
) WITHOUT ROWID;

INSERT INTO hot_source_watermarks (class, watermark, updated_at_ms) VALUES
  ('profile_evidence', 0, 0),
  ('pinned',           0, 0),
  ('purpose_index',    0, 0),
  ('summaries',        0, 0),
  ('playbooks',        0, 0),
  ('policy',           0, 0);

CREATE TABLE hot_prefix_cache (
  agent_id TEXT NOT NULL,
  recipe_hash TEXT NOT NULL,
  prefix BLOB NOT NULL,
  segments_json TEXT NOT NULL,
  bytes INTEGER NOT NULL,
  watermarks_json TEXT NOT NULL,
  assembled_at_ms INTEGER NOT NULL,
  assembly_latency_ms INTEGER NOT NULL,
  PRIMARY KEY (agent_id, recipe_hash)
) WITHOUT ROWID;
```

Migration is append-only (CLAUDE.md §6.11) — never edited after merge.

### 4.2 Core domain types

`crates/cairn-core/src/domain/hot_prefix.rs` (new file):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SourceClass {
    ProfileEvidence,
    Pinned,
    PurposeIndex,
    Summaries,
    Playbooks,
    Policy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceWatermarks {
    pub profile_evidence: u64,
    pub pinned: u64,
    pub purpose_index: u64,
    pub summaries: u64,
    pub playbooks: u64,
    pub policy: u64,
}

impl SourceWatermarks {
    #[must_use]
    pub fn matches(&self, other: &Self) -> bool { /* field-wise eq */ }
}

#[must_use]
pub fn classify_record(r: &MemoryRecord) -> SmallVec<[SourceClass; 2]> {
    // see §5.1 below
}
```

### 4.3 Cache contract

`crates/cairn-core/src/contract/hot_prefix_cache.rs` (new file):

```rust
use crate::generated::verbs::assemble_hot::Segment;

pub struct CachedPrefix {
    pub prefix: Vec<u8>,
    pub segments: Vec<Segment>,
    pub bytes: u64,
    pub watermarks: SourceWatermarks,
    pub assembled_at_ms: i64,
    pub assembly_latency_ms: u64,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CacheError {
    #[error("hot-prefix cache backend: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("cache row corrupt: {reason}")]
    Corrupt { reason: String },
    #[error("watermark schema mismatch: missing class {class:?}")]
    WatermarkSchemaMismatch { class: SourceClass },
}

#[async_trait::async_trait]
pub trait HotPrefixCache: Send + Sync {
    async fn current_watermarks(&self) -> Result<SourceWatermarks, CacheError>;
    async fn get(
        &self,
        agent: &Identity,
        recipe_hash: &str,
    ) -> Result<Option<CachedPrefix>, CacheError>;
    async fn put(
        &self,
        agent: &Identity,
        recipe_hash: &str,
        entry: &CachedPrefix,
    ) -> Result<(), CacheError>;
    async fn bump(&self, classes: &[SourceClass]) -> Result<SourceWatermarks, CacheError>;
}
```

### 4.4 Metrics contract

`crates/cairn-core/src/contract/metrics.rs` (new file):

```rust
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event")]
#[non_exhaustive]
pub enum MetricEvent {
    #[serde(rename = "hot_prefix_assembled")]
    HotPrefixAssembled {
        ts_ms: i64,
        vault_id: String,
        agent_id: String,
        recipe_hash: String,
        latency_ms: u64,
        bytes: u64,
        budget_bytes: u64,
        budget_used_ratio: f64,
        cache_hit: bool,
        watermarks: SourceWatermarks,
    },
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MetricsError {
    #[error("metrics sink io: {0}")]
    Io(#[from] std::io::Error),
    #[error("metrics sink serialize: {0}")]
    Serialize(#[from] serde_json::Error),
}

#[async_trait::async_trait]
pub trait MetricsSink: Send + Sync {
    async fn emit(&self, event: MetricEvent) -> Result<(), MetricsError>;
}
```

### 4.5 IDL deltas

`crates/cairn-idl/schema/verbs/lint.json` — add three variants to the `Kind`
enum:

```diff
   "kind": {
     "$id": "#/$defs/Kind",
     "type": "string",
     "enum": [
       "broken_actor_chain",
+      "broken_source_link",
       "contradictory_edge",
       "contradiction",
       "data_gap",
       "deferred_check",
       "ambiguous_edge",
       "hot_memory_over_budget",
       "index_drift",
       "malformed_record",
       "missing_concept",
       "missing_provenance",
+      "missing_summary",
       "orphan",
       "projection_drift",
       "projection_missing",
       "stale",
+      "stale_profile_line",
       "stale_schema"
     ]
   }
```

`cargo run -p cairn-idl --bin cairn-codegen` regenerates
`crates/cairn-core/src/generated/verbs/lint.rs`. Snapshot tests in
`crates/cairn-idl/tests/codegen_snapshot.rs` regenerate via
`cargo insta review`.

`crates/cairn-core/src/verbs/lint/mod.rs` `kind_key` map gains the three new
arms. `summarize` byKind aggregation works automatically once the variants
exist.

## 5. Data flow

### 5.1 Record classification

Inside `cairn-core::domain::hot_prefix`, pure function:

```rust
pub fn classify_record(r: &MemoryRecord) -> SmallVec<[SourceClass; 2]> {
    let mut out = SmallVec::new();
    match r.kind {
        MemoryKind::User
        | MemoryKind::Feedback
        | MemoryKind::Entity
        | MemoryKind::Strategy => {
            out.push(SourceClass::ProfileEvidence);
            if r.pinned {
                out.push(SourceClass::Pinned);
            }
        }
        MemoryKind::Project if r.pinned => out.push(SourceClass::Pinned),
        MemoryKind::Playbook => out.push(SourceClass::Playbooks),
        _ => {}
    }
    out
}
```

A `Pinned` user record bumps both `ProfileEvidence` and `Pinned`. The
classifier is the single source of truth — store-side write paths call it,
lint and tests reuse it.

### 5.2 Write path (watermark bump inside the WAL tx)

Every `cairn-store-sqlite` write that lands a `MemoryRecord` already runs
inside a two-phase WAL apply (CLAUDE.md §6.11, brief §5.6). After the record
is staged but before activation, the store calls
`classify_record(&record)` and runs:

```sql
UPDATE hot_source_watermarks
   SET watermark = watermark + 1, updated_at_ms = :now
 WHERE class IN (:classes);
```

Inside the **same transaction**. If the parent write commits, the bump
commits; if it rolls back, the bump rolls back. There is no
post-commit hook — that would violate brief invariant 5.

Four non-record write triggers, each handled at the call site that performs
the underlying mutation (still inside the relevant SQLite tx where one
exists, otherwise inside a dedicated write tx):

| Trigger                                        | Bumps              | Call site            |
|------------------------------------------------|--------------------|----------------------|
| ingest / update / forget on a classified record| matching class(es) | `MemoryStore::apply` |
| write to `purpose.md` / `index.md`             | `PurposeIndex`     | vault writer helper  |
| write to any `_summary.md`                     | `Summaries`        | summarize verb       |
| `.cairn/config.yaml` `hot_memory.*` change     | `Policy`           | config writer helper |
| any forget verb (record / session / scope)     | **all 6**          | `verbs::forget`      |

The forget hard-gate ensures acceptance criterion 1: a forgotten record's
class is bumped, but to defend against any classification gap (a kind we
forgot to map) the forget path bumps every class. One extra reassembly per
forget; correctness over efficiency.

### 5.3 Cache-aware assembly

`crates/cairn-core/src/verbs/assemble_hot/cached.rs` (new module):

```rust
pub async fn cached_assemble(
    config: &CairnConfig,
    agent: &Identity,
    cache: &dyn HotPrefixCache,
    metrics: &dyn MetricsSink,
    vault_id: &str,
    load_bodies: impl FnOnce() -> Result<Vec<String>, AssembleHotError>,
) -> Result<AssembleHotData, AssembleHotError> {
    let recipe_hash = recipe_hash_canonical(&config.vault.hot_memory.recipe);
    let started = Instant::now();
    let wm_now = cache
        .current_watermarks()
        .await
        .map_err(AssembleHotError::Cache)?;

    if let Some(entry) = cache.get(agent, &recipe_hash).await.unwrap_or(None) {
        if entry.watermarks.matches(&wm_now) {
            let latency_ms = started.elapsed().as_millis() as u64;
            metrics.emit(hot_prefix_assembled_event(
                vault_id, agent, &recipe_hash, &entry, latency_ms, true,
                wm_now, &config.vault.hot_memory,
            )).await.ok();
            return Ok(entry.into_assemble_hot_data());
        }
    }

    let bodies = load_bodies()?;
    let data = assemble_hot_from_bodies(&config.vault.hot_memory, bodies, None)?;
    let latency_ms = started.elapsed().as_millis() as u64;
    let entry = CachedPrefix { /* ... */ };
    cache.put(agent, &recipe_hash, &entry).await.ok();
    metrics.emit(hot_prefix_assembled_event(
        vault_id, agent, &recipe_hash, &entry, latency_ms, false,
        wm_now, &config.vault.hot_memory,
    )).await.ok();
    Ok(data)
}
```

Recipe hash is `sha256(canonical_json(recipe))` for stability across reorderings.

### 5.4 Lint walker (replaces the #259 canary)

The lint walker needs two inputs the current `LintInputs` struct does not
expose: the vault root (for filesystem source-link checks) and a way to
load step bodies (for the dry-run assembler call). Both are added to
`LintInputs` as new optional fields:

```rust
pub struct LintInputs<'a> {
    // ...existing fields...
    /// Vault root for filesystem-backed lint checks (broken source links,
    /// missing summaries). `None` falls the new checks back to no-ops so
    /// fixture-only tests of unrelated checks remain green.
    pub vault_root: Option<&'a Path>,
    /// Function loading step bodies for the dry-run walker. `None` skips
    /// the walker (keeps the over-budget check on the canary path until
    /// the dispatch layer wires a real loader).
    pub hot_body_loader: Option<&'a (dyn Fn(HotRecipeStep) -> Result<String, String> + 'a)>,
}
```

Stale-profile-line detection scans the assembled profile body for cited
evidence `record_id`s and resolves them against `inputs.records` plus the
existing `consent_lookup`. Any cited id absent from the active-records list
or whose `consent_state ∈ {forgotten, expired}` emits `StaleProfileLine`.

`crates/cairn-core/src/verbs/lint/checks/hot_memory.rs` rewritten:

```rust
pub fn run(inputs: &LintInputs<'_>) -> Vec<Finding> {
    let mut findings = Vec::new();
    let max_bytes = u64::from(inputs.config.vault.hot_memory.max_bytes);

    // 1. Real walker: dry-run the assembler against fixture-loaded bodies.
    let bodies = load_lint_bodies(inputs);
    match assemble_hot_from_bodies(&inputs.config.vault.hot_memory, bodies, None) {
        Ok(data) if data.bytes > max_bytes => {
            findings.push(over_budget_finding(data.bytes, max_bytes));
        }
        Ok(_) => {}
        Err(e) => findings.push(walker_failure_finding(e)),
    }

    // 2. Stale profile lines.
    findings.extend(stale_profile_lines(inputs));

    // 3. Broken source links.
    findings.extend(broken_source_links(inputs));

    // 4. Missing rolling summaries.
    findings.extend(missing_summaries(inputs));

    findings
}
```

Each helper emits its specific `Kind`:

- `stale_profile_lines` walks the AutoUserProfile body, resolves each cited
  evidence record id, emits `Kind::StaleProfileLine` (Warning) for any whose
  `consent_state ∈ {forgotten, expired}`.
- `broken_source_links` checks that `purpose.md`, `index.md`, every
  `_summary.md` referenced in the recipe scope, and every active playbook
  path resolves and stays inside the vault root. Failure emits
  `Kind::BrokenSourceLink` (Error).
- `missing_summaries` checks each folder in the recipe scope where
  `vault.hot_memory.summaries.required` is true; absent `_summary.md` emits
  `Kind::MissingSummary` (Warning).

The deferred-step canary findings are removed entirely; `closes #259`.

## 6. Errors and edge cases

### 6.1 Failure-mode policy

| Condition                                      | Action                                                       |
|------------------------------------------------|--------------------------------------------------------------|
| `cache.get` returns `Backend` error            | log `warn`, bypass cache, reassemble, do not `put`            |
| `cache.get` returns `Corrupt`                  | log `error`, delete row, reassemble, `put` fresh              |
| `cache.put` fails                              | log `warn`, return assembled prefix                           |
| `cache.bump` fails inside parent write tx      | propagate error → roll back the parent tx (correctness)       |
| `current_watermarks` returns < 6 rows          | `WatermarkSchemaMismatch` → reseed; if reseed fails, sysexit 69 |
| `MetricsSink::emit` fails                      | log `warn`, swallow; never break the verb                     |

### 6.2 Edge cases

1. **Recipe change at runtime.** New `recipe_hash` → fresh row; the
   `Policy` watermark bump from the config-write hook ensures the active row
   reassembles too. Stale rows linger until manual cleanup (LRU is out of
   scope, see §10).
2. **Multi-agent vault.** PK is `(agent_id, recipe_hash)`. Forget bumps
   watermarks vault-wide → both agents reassemble. Acceptance criterion 1
   holds across agents.
3. **Concurrent assemble (same agent).** Both readers miss, both
   reassemble, both `put` via `INSERT OR REPLACE`. Wasted work but correct.
4. **Counter overflow.** `u64` at 1B writes/sec overflows in 584 years —
   not a real concern.
5. **Older schema row.** `WatermarkSchemaMismatch` fires; cache layer
   deletes + reseeds. Forward compat lives in the migration.
6. **Forget Phase B (async purge).** Phase A bumps watermarks → cache
   invalidated → reader-invisible. Phase B doesn't bump (records already
   gone from the readers' POV).

## 7. Integration touch points

| File                                                    | Change                                                                |
|---------------------------------------------------------|-----------------------------------------------------------------------|
| `crates/cairn-core/src/contract/mod.rs`                 | re-export `hot_prefix_cache::HotPrefixCache`, `metrics::MetricsSink` |
| `crates/cairn-core/src/contract/hot_prefix_cache.rs`    | NEW — trait + `CachedPrefix` + `CacheError`                           |
| `crates/cairn-core/src/contract/metrics.rs`             | NEW — `MetricsSink` trait + `MetricsError`                            |
| `crates/cairn-core/src/domain/hot_prefix.rs`            | NEW — `SourceClass`, `SourceWatermarks`, `classify_record`            |
| `crates/cairn-core/src/domain/metrics.rs`               | NEW — `MetricEvent`                                                   |
| `crates/cairn-core/src/verbs/assemble_hot/cached.rs`    | NEW — `cached_assemble`                                               |
| `crates/cairn-core/src/verbs/assemble_hot/mod.rs`       | re-export `cached_assemble`                                           |
| `crates/cairn-core/src/verbs/lint/checks/hot_memory.rs` | rewrite as real walker; emit 3 new kinds                              |
| `crates/cairn-core/src/verbs/lint/mod.rs`               | extend `kind_key` for the 3 new variants                              |
| `crates/cairn-idl/schema/verbs/lint.json`               | add 3 enum variants                                                   |
| `crates/cairn-core/src/generated/verbs/lint.rs`         | regen via `cairn-codegen`                                             |
| `crates/cairn-store-sqlite/migrations/<NNNN>.up.sql`    | new tables + seed (see §4.1)                                          |
| `crates/cairn-store-sqlite/src/store/`                  | `SqliteHotPrefixCache` impl; classify+bump on every write             |
| `crates/cairn-cli/src/verbs/assemble_hot.rs`            | wire cache + metrics sink; route through `cached_assemble`            |
| `crates/cairn-cli/src/metrics.rs`                       | NEW — `JsonlMetricsSink`                                              |
| `docs/site/src/reference/generated/`                    | regen via `cairn-docgen --write`                                      |

No new capability advertised in `status` (brief §8.0.a): this is internal
infra, not a new verb or mode.

## 8. Tests

### 8.1 Unit tests (in-module)

| Module                                  | Coverage                                                            |
|-----------------------------------------|---------------------------------------------------------------------|
| `domain/hot_prefix.rs`                  | `SourceWatermarks::matches` (proptest, 6 fields)                    |
| `domain/hot_prefix.rs`                  | `classify_record` table: every `MemoryKind` × pinned ∈ {0, 1}       |
| `verbs/assemble_hot/cached.rs`          | hit / miss / corrupt-row / backend-error paths via mock cache       |
| `verbs/assemble_hot/cached.rs`          | metrics emit on hit + miss; sink error swallowed                    |
| `verbs/lint/checks/hot_memory.rs`       | over-budget walker emits `HotMemoryOverBudget` Error                |
| `verbs/lint/checks/hot_memory.rs`       | stale profile line: forgotten record cited in profile body          |
| `verbs/lint/checks/hot_memory.rs`       | broken link: missing `purpose.md` → `BrokenSourceLink` Error        |
| `verbs/lint/checks/hot_memory.rs`       | missing summary: folder w/o `_summary.md` → Warning                 |
| `verbs/lint/checks/hot_memory.rs`       | clean recipe → no findings (closes #259 deferred warnings)          |

### 8.2 Property tests

- `SourceWatermarks::matches` reflexive, symmetric; bumping any field breaks.
- Cache round-trip: `put(entry)` then `get` returns byte-identical `entry`.

### 8.3 Integration tests

`crates/cairn-store-sqlite/tests/`:

| Test                                             | Scenario                                                                |
|--------------------------------------------------|-------------------------------------------------------------------------|
| `hot_prefix_cache_hit_after_put`                 | put + get round-trip with real SQLite                                   |
| `bump_invalidates_cached_row`                    | put → bump → get returns stale watermarks → caller reassembles          |
| `bump_inside_write_tx_rolls_back_on_error`       | inject failure mid-tx → no watermark drift                              |
| `forget_bumps_all_six_classes`                   | one forget → all 6 watermarks incremented exactly once                  |
| `concurrent_assemble_does_not_corrupt`           | 8 tokio tasks racing on miss → final row consistent                     |
| `migration_seeds_six_classes`                    | fresh DB → exactly the 6 rows                                           |
| `purpose_md_write_bumps_purpose_index`           | edit `purpose.md` via store helper → watermark up                       |

### 8.4 CLI tests

`crates/cairn-cli/tests/`:

| Test                                              | Scenario                                                                |
|---------------------------------------------------|-------------------------------------------------------------------------|
| `assemble_hot_writes_metrics_jsonl`               | one assemble_hot call → exactly one JSON line in `.cairn/metrics.jsonl` |
| `assemble_hot_cache_hit_metric`                   | two back-to-back calls → second line `cache_hit: true`                  |
| `forget_invalidates_cache_e2e`                    | put → forget → next assemble_hot has `cache_hit: false`                 |
| `lint_emits_three_new_kinds`                      | fixture vault triggers each new kind; insta snapshot                    |
| `lint_clean_vault_no_hot_memory_findings`         | post-#259 closure: clean recipe → no deferred warnings                  |
| `hot_prefix_latency_smoke`                        | 10 iterations on fixture vault → p95 latency_ms < 50ms (brief §15)      |

### 8.5 Wire-compat snapshots

- `crates/cairn-idl/tests/snapshots/codegen_snapshot__*lint*.snap` regenerated.
- `crates/cairn-core/tests/snapshots/assemble_hot_*.snap` unchanged
  (response shape identical; cache is internal).

### 8.6 TDD discipline (CLAUDE.md §7)

Every test in §8.1–§8.4 is committed **failing first** in a dedicated commit,
then the implementation lands in a follow-up commit. PR diff makes that
sequence visible.

## 9. Verification checklist (CLAUDE.md §8)

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
mdbook build docs/site
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" \
  cargo doc --workspace --no-deps --document-private-items --locked
cargo deny check
cargo audit --deny warnings
cargo machete
```

## 10. Out of scope (file follow-up issues)

- LRU eviction of `hot_prefix_cache` rows. P0 cache is unbounded; vault is
  single-user so growth is `O(unique recipes × agents)`.
- Per-step assembly profiling (which step contributed how many bytes/ms).
- OpenTelemetry export of metrics events (gated, brief §6.6 P1+).
- Backfill: existing vaults get `watermark=0` from the migration; first
  `assemble_hot` after upgrade is a guaranteed miss. No special handling.
- Disk-spill protection on `metrics.jsonl` (rotation / size cap).

## 11. Invariants touched

| # | Brief §2 invariant                          | How this design honors it                                         |
|---|---------------------------------------------|-------------------------------------------------------------------|
| 1 | Harness-agnostic                            | All wiring lives in core / store / cli — no harness-specific code |
| 2 | Stand-alone P0                              | Pure Rust + SQLite + filesystem; no network                       |
| 3 | CLI is ground truth                         | One `cached_assemble` fn; CLI / MCP / SDK / skill all call it     |
| 4 | Seven contracts + pure functions            | Adds two new contracts: `HotPrefixCache`, `MetricsSink`           |
| 5 | WAL + two-phase apply for every mutation    | Watermark bumps run inside the parent SQLite tx                   |
| 6 | Fail closed on capability                   | Cache failures degrade to direct assembly; bump failures abort tx |
| 7 | `forbid(unsafe_code)`                       | No unsafe added                                                   |
| 8 | No `unwrap` / `expect` in core              | All paths return typed `Result`                                   |
| 9 | Privacy by construction                     | Forget bumps all 6 watermarks; cache cannot serve forgotten rows  |
|10 | Sources / records / schema layer separation | Cache is `.cairn/` infra; doesn't touch `sources/` or `wiki/`     |
