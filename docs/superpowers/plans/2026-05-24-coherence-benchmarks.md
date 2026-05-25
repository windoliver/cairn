# Coherence Benchmarks & Release Gate — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement issue [#137](https://github.com/windoliver/cairn/issues/137) — a deterministic coherence benchmark suite that scores the extended replay cassettes (#136) along five named metrics, with per-metric beta/rc thresholds, a 2% regression delta gate, versioned trend persistence, and a CI job that fails closed on regression.

**Architecture:** New `coherence` module under `crates/cairn-bench/src/`. Reuses existing `cairn-test-fixtures::replay` harness; tags each `ReplayAction` with an optional `MetricCategory`; aggregates per-category pass-rate; loads a TOML threshold manifest + JSON baseline; evaluates gate; appends to a versioned JSONL trend file. New `cairn-bench coherence run` subcommand wires it into CI.

**Tech Stack:** Rust 1.95.0, `clap` 4.5 derive API, `serde` + `serde_json` + `toml`, `tokio` async, existing `cairn-test-fixtures::replay`, `criterion` (already a dev-dep — not used here), `insta` for snapshot tests, `jsonschema` for fixture validation, `proptest` for the trend append race test.

**Spec reference:** `docs/design/2026-05-24-coherence-benchmarks-design.md`

---

## Pre-flight

This plan assumes you are working in the `rustling-conjuring-tower` worktree on a fresh branch. If not, create one:

```bash
git checkout -b feat/coherence-benchmarks
```

Verify the baseline state compiles cleanly:

```bash
cargo check --workspace --all-targets --locked
cargo nextest run -p cairn-test-fixtures --locked
```

Both must pass before starting Task 1.

---

## File map

**Created:**
- `crates/cairn-bench/src/coherence/mod.rs` — public API, orchestration
- `crates/cairn-bench/src/coherence/category.rs` — re-export + Display for `MetricCategory`
- `crates/cairn-bench/src/coherence/score.rs` — bucket + score functions
- `crates/cairn-bench/src/coherence/threshold.rs` — manifest loader, gate evaluator
- `crates/cairn-bench/src/coherence/trend.rs` — JSONL writer + per-line migrator
- `crates/cairn-bench/src/coherence/report.rs` — human + JSON output rendering
- `crates/cairn-bench/manifests/coherence.toml` — per-metric thresholds
- `crates/cairn-bench/baselines/coherence.json` — committed floor
- `crates/cairn-bench/baselines/coherence-trend.jsonl` — committed empty trend
- `crates/cairn-bench/schemas/coherence-threshold.schema.json`
- `crates/cairn-bench/schemas/coherence-baseline.schema.json`
- `crates/cairn-bench/schemas/coherence-trend.schema.json`
- `crates/cairn-bench/tests/coherence_smoke.rs`

**Modified:**
- `crates/cairn-test-fixtures/src/replay.rs` — add `MetricCategory` enum; add optional `metric_category: Option<MetricCategory>` to each `ReplayAction` variant; add optional `stale_record_ids: Vec<String>` to `ReplaySearchAction`
- `crates/cairn-test-fixtures/Cargo.toml` — no change required (uses workspace `serde`)
- `crates/cairn-bench/Cargo.toml` — add `proptest = { workspace = true }` to `[dev-dependencies]`
- `crates/cairn-bench/src/lib.rs` — add `pub mod coherence;`
- `crates/cairn-bench/src/main.rs` — add `Coherence` variant + dispatch
- `fixtures/v0/replay/research_domain.json` — backfill `metric_category` tags + add 1 StaleAvoidance search action
- `fixtures/v0/replay/engineering_domain.json` — backfill + add 1 StaleAvoidance search action
- `fixtures/v0/replay/support_domain.json` — backfill + add 1 StaleAvoidance search action
- `.github/workflows/ci.yml` — add `coherence-gate` job
- `docs/ci.md` — document the new job
- `CLAUDE.md` — add `coherence run --gate beta` line to §8 verification list
- `docs/design/traceability.md` — add design-doc reference

---

## Task 1: Add `MetricCategory` enum and `metric_category` field to replay schema

**Files:**
- Modify: `crates/cairn-test-fixtures/src/replay.rs`

This task makes existing cassettes accept (but not require) a `metric_category` tag on each action. Backwards-compatible because `Option` + `#[serde(default)]` defeats `deny_unknown_fields` for the new field name only.

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests { ... }` block at the bottom of `crates/cairn-test-fixtures/src/replay.rs`:

```rust
    #[test]
    fn metric_category_round_trips_through_serde() {
        let raw = serde_json::json!({
            "verb": "summarize",
            "story": "RESEARCH_SUMMARY_GOLDEN",
            "session_id": "research-literature",
            "expected_record_ids": ["01HQZX9F5N00000000000000R4"],
            "metric_category": "summary_quality"
        });
        let action: ReplayAction = serde_json::from_value(raw).expect("parse action");
        match action {
            ReplayAction::Summarize { metric_category, .. } => {
                assert_eq!(metric_category, Some(MetricCategory::SummaryQuality));
            }
            other => panic!("expected Summarize, got {other:?}"),
        }
    }

    #[test]
    fn metric_category_absent_parses_as_none() {
        let raw = serde_json::json!({
            "verb": "summarize",
            "story": "RESEARCH_SUMMARY_GOLDEN",
            "session_id": "research-literature",
            "expected_record_ids": ["01HQZX9F5N00000000000000R4"]
        });
        let action: ReplayAction = serde_json::from_value(raw).expect("parse action");
        match action {
            ReplayAction::Summarize { metric_category, .. } => {
                assert_eq!(metric_category, None);
            }
            other => panic!("expected Summarize, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo nextest run -p cairn-test-fixtures --locked metric_category
```

Expected: compile error — `MetricCategory` not found, no `metric_category` field on `ReplayAction::Summarize`.

- [ ] **Step 3: Add the `MetricCategory` enum**

In `crates/cairn-test-fixtures/src/replay.rs`, just above the `ReplayAction` enum definition (around line 150), insert:

```rust
/// Coherence metric category assigned to a replay action.
///
/// `cairn-bench coherence` aggregates per-category pass rates. An action
/// without a `metric_category` is excluded from coherence scoring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricCategory {
    /// Long-horizon recall — retrieve_session, retrieve_turn, assemble_hot,
    /// capture_trace, record_present(true).
    RecallPrecision,
    /// Stale-context avoidance — search actions with a non-empty
    /// `stale_record_ids` set.
    StaleAvoidance,
    /// Summary quality — summarize actions matching the expected record set.
    SummaryQuality,
    /// Search relevance — search actions whose top-1 hit matches expected.
    SearchUsefulness,
    /// Forget completeness — forget_record actions whose follow-up search
    /// excludes the tombstoned record.
    ForgetCompleteness,
}
```

- [ ] **Step 4: Add `metric_category` field to each `ReplayAction` variant**

Modify the `ReplayAction` enum in the same file. Each variant gains:

```rust
#[serde(default)]
metric_category: Option<MetricCategory>,
```

The variants to update (search for `pub enum ReplayAction`):

- `Search(ReplaySearchAction)` — leave the variant alone; the field goes inside `ReplaySearchAction` instead (see Step 5).
- `AssembleHot { story, expected_record_ids }` → add `#[serde(default)] metric_category: Option<MetricCategory>,`
- `CaptureTrace { story, session_id, expected_trace_events }` → add
- `Summarize { story, session_id, expected_record_ids }` → add
- `Lint { story, expected_status }` → add
- `RetrieveSession { story, session_id, expected_turn_ids, expected_trace_events }` → add
- `RetrieveTurn { story, session_id, turn_id, expected_trace_events }` → add
- `RecordPresent { story, record_id, expected_present }` → add
- `ForgetRecord { story, record_id, followup_query, expected_absent_from_search }` → add

For each variant, the resulting shape looks like:

```rust
    /// Summary replay expectation.
    Summarize {
        /// User story label.
        story: String,
        /// Session id to summarize.
        session_id: String,
        /// Expected summary record ids.
        expected_record_ids: Vec<String>,
        /// Coherence metric category. `None` excludes the action from
        /// `cairn-bench coherence` scoring.
        #[serde(default)]
        metric_category: Option<MetricCategory>,
    },
```

- [ ] **Step 5: Add `metric_category` field to `ReplaySearchAction`**

In the same file (around line 245), modify `ReplaySearchAction`:

```rust
/// Search replay action.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaySearchAction {
    /// User story label.
    pub story: String,
    /// Search mode.
    pub mode: ReplaySearchMode,
    /// Query string.
    pub query: String,
    /// Result limit.
    pub limit: usize,
    /// Expected outcome.
    pub expected: ReplayExpectation,
    /// Coherence metric category. `None` excludes the action from coherence
    /// scoring. When `stale_record_ids` is non-empty, the category is
    /// auto-promoted to `StaleAvoidance` regardless of this field — see
    /// `cairn-bench` coherence module.
    #[serde(default)]
    pub metric_category: Option<MetricCategory>,
}
```

(The `stale_record_ids` field is added in Task 2.)

- [ ] **Step 6: Run tests to verify they pass**

```bash
cargo nextest run -p cairn-test-fixtures --locked metric_category
```

Expected: PASS — both tests green.

- [ ] **Step 7: Run the existing replay test suite to check for regressions**

```bash
cargo nextest run -p cairn-test-fixtures --locked
cargo test --doc -p cairn-test-fixtures --locked
```

Expected: PASS — all existing fixtures still parse, including the P0 cassettes that have no `metric_category`.

- [ ] **Step 8: Commit**

```bash
git add crates/cairn-test-fixtures/src/replay.rs
git commit -m "feat(replay): add MetricCategory enum and per-action metric_category tag

Optional Option<MetricCategory> with serde(default) on each ReplayAction
variant and on ReplaySearchAction. Existing cassettes parse unchanged
because the field defaults to None. Coherence scoring (issue #137) reads
the tag to bucket actions per metric.

Refs #137"
```

---

## Task 2: Add `stale_record_ids` to `ReplaySearchAction`

**Files:**
- Modify: `crates/cairn-test-fixtures/src/replay.rs`

This task lets cassettes declare records that must *not* appear in a search result. Coherence scoring auto-classifies such actions as `StaleAvoidance`.

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests { ... }` block:

```rust
    #[test]
    fn stale_record_ids_round_trip() {
        let raw = serde_json::json!({
            "verb": "search",
            "story": "RESEARCH_STALE_AVOIDANCE",
            "mode": "keyword",
            "query": "lattice biology",
            "limit": 5,
            "expected": { "status": "hits", "record_ids": ["01HQZX9F5N00000000000000R5"] },
            "stale_record_ids": ["01HQZX9F5N00000000000000R7"]
        });
        let action: ReplayAction = serde_json::from_value(raw).expect("parse action");
        let ReplayAction::Search(search) = action else {
            panic!("expected Search variant");
        };
        assert_eq!(search.stale_record_ids, vec!["01HQZX9F5N00000000000000R7".to_owned()]);
    }

    #[test]
    fn stale_record_ids_absent_defaults_to_empty() {
        let raw = serde_json::json!({
            "verb": "search",
            "story": "RESEARCH_SEARCH_RELEVANCE",
            "mode": "keyword",
            "query": "lattice biology drift marker",
            "limit": 1,
            "expected": { "status": "hits", "record_ids": ["01HQZX9F5N00000000000000R5"] }
        });
        let action: ReplayAction = serde_json::from_value(raw).expect("parse action");
        let ReplayAction::Search(search) = action else {
            panic!("expected Search variant");
        };
        assert!(search.stale_record_ids.is_empty());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo nextest run -p cairn-test-fixtures --locked stale_record_ids
```

Expected: compile error — `stale_record_ids` field missing.

- [ ] **Step 3: Add the field**

In `crates/cairn-test-fixtures/src/replay.rs`, modify `ReplaySearchAction` to add the field after `metric_category`:

```rust
    /// Records that must NOT appear in the result. When non-empty, coherence
    /// scoring auto-classifies the action as `StaleAvoidance`.
    #[serde(default)]
    pub stale_record_ids: Vec<String>,
```

- [ ] **Step 4: Run tests**

```bash
cargo nextest run -p cairn-test-fixtures --locked stale_record_ids
cargo nextest run -p cairn-test-fixtures --locked
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-test-fixtures/src/replay.rs
git commit -m "feat(replay): add stale_record_ids to ReplaySearchAction

Optional Vec<String> defaulting to empty. Non-empty marks the search
action as a stale-leak check; coherence scoring (issue #137) auto-
promotes such actions to MetricCategory::StaleAvoidance regardless of
any explicit metric_category tag.

Refs #137"
```

---

## Task 3: Backfill `metric_category` and add `StaleAvoidance` searches in cassettes

**Files:**
- Modify: `fixtures/v0/replay/research_domain.json`
- Modify: `fixtures/v0/replay/engineering_domain.json`
- Modify: `fixtures/v0/replay/support_domain.json`

Per the design (§5.1 default categorization table), tag every existing action with its default category and add one new `StaleAvoidance` search per domain. Use the existing "unrelated note" records (R7/E7/S7) as the stale id in each cassette.

- [ ] **Step 1: Backfill `research_domain.json`**

Replace the `actions` array in `fixtures/v0/replay/research_domain.json` with:

```json
  "actions": [
    {
      "verb": "capture_trace",
      "story": "RESEARCH_CAPTURE_LONG_HORIZON",
      "session_id": "research-literature",
      "expected_trace_events": ["user_message", "agent_message", "user_message", "agent_message", "turn_summary"],
      "metric_category": "recall_precision"
    },
    {
      "verb": "retrieve_session",
      "story": "RESEARCH_MULTI_SESSION_COHERENCE",
      "session_id": "research-literature",
      "expected_turn_ids": ["1", "2"],
      "expected_trace_events": ["user_message", "agent_message", "user_message", "agent_message", "turn_summary"],
      "metric_category": "recall_precision"
    },
    {
      "verb": "retrieve_turn",
      "story": "RESEARCH_RETRIEVE_LATER_TURN",
      "session_id": "research-literature",
      "turn_id": "2",
      "expected_trace_events": ["user_message", "agent_message"],
      "metric_category": "recall_precision"
    },
    {
      "verb": "summarize",
      "story": "RESEARCH_SUMMARY_GOLDEN",
      "session_id": "research-literature",
      "expected_record_ids": ["01HQZX9F5N00000000000000R4"],
      "metric_category": "summary_quality"
    },
    {
      "verb": "search",
      "story": "RESEARCH_SEARCH_RELEVANCE",
      "mode": "keyword",
      "query": "lattice biology drift marker",
      "limit": 1,
      "expected": {
        "status": "hits",
        "record_ids": ["01HQZX9F5N00000000000000R5"]
      },
      "metric_category": "search_usefulness"
    },
    {
      "verb": "search",
      "story": "RESEARCH_STALE_AVOIDANCE",
      "mode": "keyword",
      "query": "lattice biology",
      "limit": 5,
      "expected": {
        "status": "hits",
        "record_ids": ["01HQZX9F5N00000000000000R5"]
      },
      "stale_record_ids": ["01HQZX9F5N00000000000000R7"]
    },
    {
      "verb": "forget_record",
      "story": "RESEARCH_PRIVACY_FORGET",
      "record_id": "01HQZX9F5N00000000000000R6",
      "followup_query": "embargoed reviewer identity delta fox",
      "expected_absent_from_search": true,
      "metric_category": "forget_completeness"
    }
  ]
```

Note: the new `RESEARCH_STALE_AVOIDANCE` action has no explicit `metric_category` — auto-classification by `stale_record_ids` handles it.

The existing `RESEARCH_SEARCH_RELEVANCE` action remains as-is but now gets the explicit `search_usefulness` tag. The `expected.record_ids` golden assertion still holds.

- [ ] **Step 2: Backfill `engineering_domain.json`**

Replace the `actions` array with the same shape, using the engineering IDs and adding `ENGINEERING_STALE_AVOIDANCE`:

```json
  "actions": [
    {
      "verb": "capture_trace",
      "story": "ENGINEERING_CAPTURE_LONG_HORIZON",
      "session_id": "engineering-deadlock",
      "expected_trace_events": ["user_message", "agent_message", "user_message", "agent_message", "turn_summary"],
      "metric_category": "recall_precision"
    },
    {
      "verb": "retrieve_session",
      "story": "ENGINEERING_MULTI_SESSION_COHERENCE",
      "session_id": "engineering-deadlock",
      "expected_turn_ids": ["1", "2"],
      "expected_trace_events": ["user_message", "agent_message", "user_message", "agent_message", "turn_summary"],
      "metric_category": "recall_precision"
    },
    {
      "verb": "retrieve_turn",
      "story": "ENGINEERING_RETRIEVE_LATER_TURN",
      "session_id": "engineering-deadlock",
      "turn_id": "2",
      "expected_trace_events": ["user_message", "agent_message"],
      "metric_category": "recall_precision"
    },
    {
      "verb": "summarize",
      "story": "ENGINEERING_SUMMARY_GOLDEN",
      "session_id": "engineering-deadlock",
      "expected_record_ids": ["01HQZX9F5N00000000000000E4"],
      "metric_category": "summary_quality"
    },
    {
      "verb": "search",
      "story": "ENGINEERING_SEARCH_RELEVANCE",
      "mode": "keyword",
      "query": "replay ledger lock before wal writer",
      "limit": 1,
      "expected": {
        "status": "hits",
        "record_ids": ["01HQZX9F5N00000000000000E5"]
      },
      "metric_category": "search_usefulness"
    },
    {
      "verb": "search",
      "story": "ENGINEERING_STALE_AVOIDANCE",
      "mode": "keyword",
      "query": "wal deadlock",
      "limit": 5,
      "expected": {
        "status": "hits",
        "record_ids": ["01HQZX9F5N00000000000000E5"]
      },
      "stale_record_ids": ["01HQZX9F5N00000000000000E7"]
    },
    {
      "verb": "forget_record",
      "story": "ENGINEERING_PRIVACY_FORGET",
      "record_id": "01HQZX9F5N00000000000000E6",
      "followup_query": "temporary debug credential amber key",
      "expected_absent_from_search": true,
      "metric_category": "forget_completeness"
    }
  ]
```

- [ ] **Step 3: Backfill `support_domain.json`**

Replace the `actions` array:

```json
  "actions": [
    {
      "verb": "capture_trace",
      "story": "SUPPORT_CAPTURE_LONG_HORIZON",
      "session_id": "support-acme-outage",
      "expected_trace_events": ["user_message", "agent_message", "user_message", "agent_message", "turn_summary"],
      "metric_category": "recall_precision"
    },
    {
      "verb": "retrieve_session",
      "story": "SUPPORT_MULTI_SESSION_COHERENCE",
      "session_id": "support-acme-outage",
      "expected_turn_ids": ["1", "2"],
      "expected_trace_events": ["user_message", "agent_message", "user_message", "agent_message", "turn_summary"],
      "metric_category": "recall_precision"
    },
    {
      "verb": "retrieve_turn",
      "story": "SUPPORT_RETRIEVE_LATER_TURN",
      "session_id": "support-acme-outage",
      "turn_id": "2",
      "expected_trace_events": ["user_message", "agent_message"],
      "metric_category": "recall_precision"
    },
    {
      "verb": "summarize",
      "story": "SUPPORT_SUMMARY_GOLDEN",
      "session_id": "support-acme-outage",
      "expected_record_ids": ["01HQZX9F5N00000000000000S4"],
      "metric_category": "summary_quality"
    },
    {
      "verb": "search",
      "story": "SUPPORT_SEARCH_RELEVANCE",
      "mode": "keyword",
      "query": "acme billing outage remediation webhook",
      "limit": 1,
      "expected": {
        "status": "hits",
        "record_ids": ["01HQZX9F5N00000000000000S5"]
      },
      "metric_category": "search_usefulness"
    },
    {
      "verb": "search",
      "story": "SUPPORT_STALE_AVOIDANCE",
      "mode": "keyword",
      "query": "acme billing outage",
      "limit": 5,
      "expected": {
        "status": "hits",
        "record_ids": ["01HQZX9F5N00000000000000S5"]
      },
      "stale_record_ids": ["01HQZX9F5N00000000000000S7"]
    },
    {
      "verb": "forget_record",
      "story": "SUPPORT_PRIVACY_FORGET",
      "record_id": "01HQZX9F5N00000000000000S6",
      "followup_query": "sensitive support contact violet pager",
      "expected_absent_from_search": true,
      "metric_category": "forget_completeness"
    }
  ]
```

- [ ] **Step 4: Verify cassettes still parse and replay tests still pass**

```bash
cargo nextest run -p cairn-test-fixtures --locked replay_harness
```

Expected: PASS — the existing `replay_harness.rs` tests do not consume `metric_category` or `stale_record_ids` and continue to validate the original golden checks.

The `RESEARCH_STALE_AVOIDANCE` / `ENGINEERING_STALE_AVOIDANCE` / `SUPPORT_STALE_AVOIDANCE` actions assert `expected.record_ids` with `limit: 5`; the cassette author has chosen a top-1 record that is dominant for the query, so the existing harness's exact-match assertion may fail if other records also rank in the top 5. Re-run and observe — if any of the three new actions fail at the existing harness layer, drop `expected.record_ids` to a permissive `["..."]` only if the absent-from-stale check still passes; otherwise tighten the query.

If a stale_record_ids action fails the existing exact-match check:
- the failure is informational, not a coherence regression
- in that case, change the new action's `expected` to skip strict record matching by reducing `limit` to 5 and leaving the `record_ids` as is — but verify both: (a) the dominant record is still at top-1; (b) the stale id is absent.

If both fail, change the query string to something more discriminative within the cassette's vocabulary.

- [ ] **Step 5: Commit**

```bash
git add fixtures/v0/replay/research_domain.json fixtures/v0/replay/engineering_domain.json fixtures/v0/replay/support_domain.json
git commit -m "test(replay): tag extended cassettes with metric_category for coherence

Backfills metric_category on every action in the three #136 extended
domain cassettes and adds one stale_record_ids search per domain so
StaleAvoidance has non-empty coverage. Existing golden expectations
unchanged.

Refs #137"
```

---

## Task 4: Coherence module skeleton (`mod.rs`, `category.rs`, `lib.rs`)

**Files:**
- Create: `crates/cairn-bench/src/coherence/mod.rs`
- Create: `crates/cairn-bench/src/coherence/category.rs`
- Modify: `crates/cairn-bench/src/lib.rs`
- Modify: `crates/cairn-bench/Cargo.toml`

This task creates the module shell. No public API yet beyond the re-export. Tests prove the module compiles.

- [ ] **Step 1: Add `proptest` dev-dep**

In `crates/cairn-bench/Cargo.toml`, find the `[dev-dependencies]` section and add at the end:

```toml
proptest = { workspace = true }
```

The `[dev-dependencies]` block after the change should contain (other lines unchanged):

```toml
[dev-dependencies]
cairn-cli = { path = "../cairn-cli" }
cairn-test-fixtures = { workspace = true }
criterion = { workspace = true }
insta = { workspace = true, features = ["json", "yaml"] }
jsonschema = { workspace = true }
proptest = { workspace = true }
```

- [ ] **Step 2: Create the module directory and `category.rs`**

```bash
mkdir -p crates/cairn-bench/src/coherence
```

Create `crates/cairn-bench/src/coherence/category.rs`:

```rust
//! Re-exports `MetricCategory` from `cairn-test-fixtures` and adds a
//! `Display` impl used by the coherence report renderer.
//!
//! The enum itself lives in the replay schema so cassette authors can tag
//! actions without depending on `cairn-bench`. Keeping the canonical
//! definition there means the schema (`fixtures/v0/replay/*.json`) and the
//! scorer share one source of truth.

use std::fmt;

pub use cairn_test_fixtures::replay::MetricCategory;

/// All five categories in display order. Used by the report renderer to
/// iterate deterministically and by the smoke test to assert coverage.
pub const ALL: [MetricCategory; 5] = [
    MetricCategory::RecallPrecision,
    MetricCategory::StaleAvoidance,
    MetricCategory::SummaryQuality,
    MetricCategory::SearchUsefulness,
    MetricCategory::ForgetCompleteness,
];

/// Render a category as its `snake_case` wire string. Matches the
/// `serde(rename_all = "snake_case")` shape in the replay schema.
#[must_use]
pub fn as_str(category: MetricCategory) -> &'static str {
    match category {
        MetricCategory::RecallPrecision => "recall_precision",
        MetricCategory::StaleAvoidance => "stale_avoidance",
        MetricCategory::SummaryQuality => "summary_quality",
        MetricCategory::SearchUsefulness => "search_usefulness",
        MetricCategory::ForgetCompleteness => "forget_completeness",
    }
}

/// Newtype wrapper so callers can `write!("{}", DisplayCategory(c))`
/// without depending on the third-party `Display` trait being implemented
/// on the upstream enum.
pub struct DisplayCategory(pub MetricCategory);

impl fmt::Display for DisplayCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(as_str(self.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_categories_listed_in_canonical_order() {
        let names: Vec<&'static str> = ALL.iter().copied().map(as_str).collect();
        assert_eq!(
            names,
            vec![
                "recall_precision",
                "stale_avoidance",
                "summary_quality",
                "search_usefulness",
                "forget_completeness",
            ]
        );
    }

    #[test]
    fn display_matches_as_str() {
        for category in ALL {
            assert_eq!(
                format!("{}", DisplayCategory(category)),
                as_str(category)
            );
        }
    }
}
```

- [ ] **Step 3: Create the module entry point `mod.rs`**

Create `crates/cairn-bench/src/coherence/mod.rs`:

```rust
//! Coherence release-gate (issue #137).
//!
//! See `docs/design/2026-05-24-coherence-benchmarks-design.md` for the
//! contract. Public API: [`run_coherence_gate`] (added in Task 9).

pub mod category;

pub use category::{ALL as ALL_CATEGORIES, DisplayCategory, MetricCategory, as_str};
```

- [ ] **Step 4: Wire the module into `lib.rs`**

In `crates/cairn-bench/src/lib.rs`, add `pub mod coherence;` after the existing `pub mod cache;` line (keep alphabetical order — insert between `cached_embedder` and `fixture`):

```rust
pub mod adapter;
pub mod all;
pub mod cache;
pub mod cached_embedder;
pub mod coherence;
pub mod fixture;
pub mod gates;
pub mod latency;
pub mod memory;
pub mod metrics;
pub mod privacy;
pub mod report;
pub mod scorecard;
pub mod sre;
```

- [ ] **Step 5: Build and run module tests**

```bash
cargo check -p cairn-bench --locked
cargo nextest run -p cairn-bench --locked coherence::category
```

Expected: PASS — two tests in `category::tests` green.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-bench/Cargo.toml crates/cairn-bench/src/lib.rs crates/cairn-bench/src/coherence/mod.rs crates/cairn-bench/src/coherence/category.rs
git commit -m "feat(bench): coherence module skeleton + category re-export

Wires the new coherence module into cairn-bench and re-exports
MetricCategory from cairn-test-fixtures with a Display helper. No
public API yet beyond the enum; subsequent commits add score,
threshold, trend, report, and the orchestrator.

Refs #137"
```

---

## Task 5: Score aggregation (`score.rs`)

**Files:**
- Create: `crates/cairn-bench/src/coherence/score.rs`
- Modify: `crates/cairn-bench/src/coherence/mod.rs`

Takes a parallel slice of `(ReplayAction, ReplayCheckReport)` pairs and produces per-category counts + scores.

- [ ] **Step 1: Write the failing tests**

Create `crates/cairn-bench/src/coherence/score.rs`:

```rust
//! Aggregate `ReplayReport` outcomes into per-category coherence scores.
//!
//! Inputs: parallel slices of `ReplayAction` (carries `metric_category` +
//! `stale_record_ids`) and `ReplayCheckReport` (carries `actual` Value).
//! Outputs: a `CategoryScores` map keyed by `MetricCategory`.

use std::collections::BTreeMap;

use cairn_test_fixtures::replay::{
    MetricCategory, ReplayAction, ReplayCheckReport, ReplaySearchAction,
};
use serde::Serialize;
use serde_json::Value;

use super::category::ALL as ALL_CATEGORIES;

/// One category's aggregated result over a run.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct CategoryScore {
    /// Number of actions tagged into this category whose coherence-pass
    /// condition held.
    pub passed: u32,
    /// Total actions tagged into this category.
    pub total: u32,
    /// `passed / total`, or 1.0 if `total == 0` (vacuous pass per design §5.4).
    pub score: f64,
}

impl CategoryScore {
    /// Vacuous-pass for an empty category.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            passed: 0,
            total: 0,
            score: 1.0,
        }
    }
}

/// All five category scores, keyed in deterministic order.
pub type CategoryScores = BTreeMap<MetricCategory, CategoryScore>;

/// Errors arising from parallel-slice misalignment.
#[derive(Debug, thiserror::Error)]
pub enum ScoreError {
    /// Actions and reports must be the same length and same order.
    #[error("actions ({actions}) and reports ({reports}) have different lengths")]
    LengthMismatch { actions: usize, reports: usize },
}

/// Aggregate a parallel slice of (action, check report) into per-category
/// scores. Untagged actions are ignored.
///
/// # Errors
/// Returns `ScoreError::LengthMismatch` if the two slices are not the same
/// length.
pub fn aggregate(
    actions: &[ReplayAction],
    reports: &[ReplayCheckReport],
) -> Result<CategoryScores, ScoreError> {
    if actions.len() != reports.len() {
        return Err(ScoreError::LengthMismatch {
            actions: actions.len(),
            reports: reports.len(),
        });
    }

    let mut buckets: BTreeMap<MetricCategory, (u32, u32)> = BTreeMap::new();
    for category in ALL_CATEGORIES {
        buckets.insert(category, (0, 0));
    }

    for (action, report) in actions.iter().zip(reports.iter()) {
        let Some(category) = classify(action) else {
            continue;
        };
        let pass = action_passed(action, report);
        let entry = buckets.entry(category).or_insert((0, 0));
        entry.1 += 1;
        if pass {
            entry.0 += 1;
        }
    }

    Ok(buckets
        .into_iter()
        .map(|(category, (passed, total))| {
            let score = if total == 0 {
                1.0
            } else {
                f64::from(passed) / f64::from(total)
            };
            (category, CategoryScore { passed, total, score })
        })
        .collect())
}

/// Compute the deterministic category for a given action, applying the
/// stale-avoidance auto-promote rule.
#[must_use]
pub fn classify(action: &ReplayAction) -> Option<MetricCategory> {
    if let ReplayAction::Search(search) = action
        && !search.stale_record_ids.is_empty()
    {
        return Some(MetricCategory::StaleAvoidance);
    }
    explicit_category(action)
}

fn explicit_category(action: &ReplayAction) -> Option<MetricCategory> {
    match action {
        ReplayAction::Search(search) => search.metric_category,
        ReplayAction::AssembleHot { metric_category, .. }
        | ReplayAction::CaptureTrace { metric_category, .. }
        | ReplayAction::Summarize { metric_category, .. }
        | ReplayAction::Lint { metric_category, .. }
        | ReplayAction::RetrieveSession { metric_category, .. }
        | ReplayAction::RetrieveTurn { metric_category, .. }
        | ReplayAction::RecordPresent { metric_category, .. }
        | ReplayAction::ForgetRecord { metric_category, .. } => *metric_category,
    }
}

/// Per-category pass condition.
///
/// `StaleAvoidance` (search-only): the cassette action passed AND the
/// returned `record_ids` are disjoint from `stale_record_ids`.
///
/// Everything else: the cassette action passed (i.e. golden expectation
/// held).
fn action_passed(action: &ReplayAction, report: &ReplayCheckReport) -> bool {
    if let ReplayAction::Search(search) = action
        && !search.stale_record_ids.is_empty()
    {
        return report.passed && disjoint_from_stale(&report.actual, &search.stale_record_ids);
    }
    report.passed
}

fn disjoint_from_stale(actual: &Value, stale_ids: &[String]) -> bool {
    let Some(arr) = actual.get("record_ids").and_then(Value::as_array) else {
        return false;
    };
    let returned: Vec<&str> = arr.iter().filter_map(Value::as_str).collect();
    !stale_ids.iter().any(|id| returned.contains(&id.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_test_fixtures::replay::{
        ReplayExpectation, ReplaySearchAction, ReplaySearchMode,
    };
    use serde_json::json;

    fn report(scenario: &str, story: &str, verb: &str, passed: bool) -> ReplayCheckReport {
        ReplayCheckReport {
            scenario_id: scenario.to_owned(),
            story: story.to_owned(),
            verb: verb.to_owned(),
            query: None,
            expected: Value::Null,
            actual: Value::Null,
            passed,
            message: None,
        }
    }

    fn summarize_action(category: Option<MetricCategory>) -> ReplayAction {
        ReplayAction::Summarize {
            story: "S".to_owned(),
            session_id: "s".to_owned(),
            expected_record_ids: vec![],
            metric_category: category,
        }
    }

    #[test]
    fn empty_slices_produce_five_vacuous_passes() {
        let scores = aggregate(&[], &[]).expect("aggregate");
        assert_eq!(scores.len(), 5);
        for category in ALL_CATEGORIES {
            let s = scores[&category];
            assert_eq!(s.total, 0);
            assert!((s.score - 1.0).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn untagged_actions_excluded_from_scoring() {
        let actions = vec![summarize_action(None)];
        let reports = vec![report("c", "S", "summarize", true)];
        let scores = aggregate(&actions, &reports).expect("aggregate");
        assert_eq!(scores[&MetricCategory::SummaryQuality].total, 0);
    }

    #[test]
    fn partial_pass_scores_correctly() {
        let actions = vec![
            summarize_action(Some(MetricCategory::SummaryQuality)),
            summarize_action(Some(MetricCategory::SummaryQuality)),
            summarize_action(Some(MetricCategory::SummaryQuality)),
        ];
        let reports = vec![
            report("c", "S", "summarize", true),
            report("c", "S", "summarize", true),
            report("c", "S", "summarize", false),
        ];
        let scores = aggregate(&actions, &reports).expect("aggregate");
        let s = scores[&MetricCategory::SummaryQuality];
        assert_eq!(s.passed, 2);
        assert_eq!(s.total, 3);
        assert!((s.score - (2.0 / 3.0)).abs() < 1e-9);
    }

    #[test]
    fn length_mismatch_returns_error() {
        let err = aggregate(&[summarize_action(None)], &[]).unwrap_err();
        assert!(matches!(err, ScoreError::LengthMismatch { .. }));
    }

    #[test]
    fn stale_avoidance_auto_classifies_search() {
        let action = ReplayAction::Search(ReplaySearchAction {
            story: "stale".to_owned(),
            mode: ReplaySearchMode::Keyword,
            query: "q".to_owned(),
            limit: 5,
            expected: ReplayExpectation::Hits { record_ids: vec![] },
            metric_category: Some(MetricCategory::SearchUsefulness), // should be overridden
            stale_record_ids: vec!["stale-id".to_owned()],
        });
        assert_eq!(classify(&action), Some(MetricCategory::StaleAvoidance));
    }

    #[test]
    fn stale_avoidance_pass_requires_disjoint_and_golden() {
        let action = ReplayAction::Search(ReplaySearchAction {
            story: "stale".to_owned(),
            mode: ReplaySearchMode::Keyword,
            query: "q".to_owned(),
            limit: 5,
            expected: ReplayExpectation::Hits { record_ids: vec![] },
            metric_category: None,
            stale_record_ids: vec!["stale-id".to_owned()],
        });
        // golden passed, returns no stale → pass
        let mut r = report("c", "stale", "search", true);
        r.actual = json!({ "record_ids": ["clean-id"] });
        assert!(action_passed(&action, &r));
        // golden passed, but stale-id present → fail
        r.actual = json!({ "record_ids": ["clean-id", "stale-id"] });
        assert!(!action_passed(&action, &r));
        // golden failed, even if disjoint → fail
        r.passed = false;
        r.actual = json!({ "record_ids": ["clean-id"] });
        assert!(!action_passed(&action, &r));
    }
}
```

- [ ] **Step 2: Wire `score` into `mod.rs`**

In `crates/cairn-bench/src/coherence/mod.rs`, add `pub mod score;` and re-export the public items:

```rust
//! Coherence release-gate (issue #137).
//!
//! See `docs/design/2026-05-24-coherence-benchmarks-design.md`.

pub mod category;
pub mod score;

pub use category::{ALL as ALL_CATEGORIES, DisplayCategory, MetricCategory, as_str};
pub use score::{CategoryScore, CategoryScores, ScoreError, aggregate, classify};
```

- [ ] **Step 3: Build and run tests**

```bash
cargo nextest run -p cairn-bench --locked coherence::score
```

Expected: PASS — all six tests green.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-bench/src/coherence/mod.rs crates/cairn-bench/src/coherence/score.rs
git commit -m "feat(bench): coherence score aggregation

Buckets ReplayActions by metric_category and computes per-category
passed/total/score. StaleAvoidance is auto-classified when a Search
action carries non-empty stale_record_ids and additionally requires
disjointness from those IDs to pass.

Refs #137"
```

---

## Task 6: Threshold manifest + gate evaluator (`threshold.rs`)

**Files:**
- Create: `crates/cairn-bench/src/coherence/threshold.rs`
- Modify: `crates/cairn-bench/src/coherence/mod.rs`

Loads `coherence.toml`, evaluates the gate against `CategoryScores` and an optional prior `Baseline`.

- [ ] **Step 1: Write the failing tests**

Create `crates/cairn-bench/src/coherence/threshold.rs`:

```rust
//! Threshold manifest loader + gate evaluator.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::category::ALL as ALL_CATEGORIES;
use super::score::{CategoryScore, CategoryScores};
use cairn_test_fixtures::replay::MetricCategory;

/// Gate mode selected on the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateMode {
    /// Record only, never fail.
    None,
    /// Beta gate — uses `beta_min`.
    Beta,
    /// Release-candidate gate — uses `rc_min`.
    Rc,
}

/// Per-category threshold row.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct CategoryThreshold {
    pub beta_min: f64,
    pub rc_min: f64,
    pub max_drop_pct: f64,
}

/// Threshold manifest (matches `manifests/coherence.toml`).
#[derive(Debug, Clone, Deserialize)]
pub struct ThresholdManifest {
    pub schema_version: u32,
    pub recall_precision: CategoryThreshold,
    pub stale_avoidance: CategoryThreshold,
    pub summary_quality: CategoryThreshold,
    pub search_usefulness: CategoryThreshold,
    pub forget_completeness: CategoryThreshold,
}

impl ThresholdManifest {
    /// Look up the threshold row for one category.
    #[must_use]
    pub fn for_category(&self, category: MetricCategory) -> CategoryThreshold {
        match category {
            MetricCategory::RecallPrecision => self.recall_precision,
            MetricCategory::StaleAvoidance => self.stale_avoidance,
            MetricCategory::SummaryQuality => self.summary_quality,
            MetricCategory::SearchUsefulness => self.search_usefulness,
            MetricCategory::ForgetCompleteness => self.forget_completeness,
        }
    }
}

/// Prior-run baseline, used for the delta-regression check.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Baseline {
    pub schema_version: u32,
    pub captured_at: String,
    pub cairn_version: String,
    pub git_sha: String,
    pub metrics: BTreeMap<String, CategoryScore>,
}

impl Baseline {
    /// Look up the recorded score for one category. Returns `None` if the
    /// baseline does not record this category yet.
    #[must_use]
    pub fn score_for(&self, category: MetricCategory) -> Option<CategoryScore> {
        self.metrics.get(super::category::as_str(category)).copied()
    }
}

/// Outcome of evaluating one metric against the gate.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum MetricOutcome {
    Pass,
    BelowFloor { floor: f64 },
    ExceededDrop { previous: f64, drop_pct: f64 },
    GateNone,
}

impl MetricOutcome {
    #[must_use]
    pub const fn is_pass(self) -> bool {
        matches!(self, Self::Pass | Self::GateNone)
    }
}

/// Per-category result row in the final gate outcome.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct MetricResult {
    pub score: CategoryScore,
    pub outcome: MetricOutcome,
    pub delta: Option<f64>,
}

/// Errors arising from loading or evaluating the gate.
#[derive(Debug, thiserror::Error)]
pub enum ThresholdError {
    #[error("read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parse {path}: {source}")]
    Toml {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("unsupported manifest schema_version {version}")]
    UnsupportedManifestVersion { version: u32 },
}

/// Load a `coherence.toml` manifest from disk.
///
/// # Errors
/// - `Io` if the file cannot be read.
/// - `Toml` if the file is malformed.
/// - `UnsupportedManifestVersion` if `schema_version` is not 1.
pub fn load_manifest(path: &Path) -> Result<ThresholdManifest, ThresholdError> {
    let raw = fs::read_to_string(path).map_err(|source| ThresholdError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let manifest: ThresholdManifest = toml::from_str(&raw).map_err(|source| ThresholdError::Toml {
        path: path.display().to_string(),
        source,
    })?;
    if manifest.schema_version != 1 {
        return Err(ThresholdError::UnsupportedManifestVersion {
            version: manifest.schema_version,
        });
    }
    Ok(manifest)
}

/// Evaluate the gate for all five categories.
///
/// Returns a map of per-category outcomes. `baseline` is `None` on the
/// first run; in that case only the floor check applies (no delta).
#[must_use]
pub fn evaluate(
    mode: GateMode,
    scores: &CategoryScores,
    manifest: &ThresholdManifest,
    baseline: Option<&Baseline>,
) -> BTreeMap<MetricCategory, MetricResult> {
    let mut out = BTreeMap::new();
    for category in ALL_CATEGORIES {
        let score = scores
            .get(&category)
            .copied()
            .unwrap_or_else(CategoryScore::empty);
        let threshold = manifest.for_category(category);
        let previous = baseline.and_then(|b| b.score_for(category)).map(|s| s.score);
        let delta = previous.map(|prev| score.score - prev);
        let outcome = evaluate_one(mode, score.score, threshold, previous);
        out.insert(
            category,
            MetricResult {
                score,
                outcome,
                delta,
            },
        );
    }
    out
}

fn evaluate_one(
    mode: GateMode,
    score: f64,
    threshold: CategoryThreshold,
    previous: Option<f64>,
) -> MetricOutcome {
    let floor = match mode {
        GateMode::None => return MetricOutcome::GateNone,
        GateMode::Beta => threshold.beta_min,
        GateMode::Rc => threshold.rc_min,
    };
    if score < floor {
        return MetricOutcome::BelowFloor { floor };
    }
    if let Some(prev) = previous {
        let drop_pct = (prev - score) * 100.0;
        if drop_pct > threshold.max_drop_pct {
            return MetricOutcome::ExceededDrop {
                previous: prev,
                drop_pct,
            };
        }
    }
    MetricOutcome::Pass
}

/// True if every metric passed (or the gate was `None`).
#[must_use]
pub fn all_pass(results: &BTreeMap<MetricCategory, MetricResult>) -> bool {
    results.values().all(|r| r.outcome.is_pass())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn threshold(beta: f64, rc: f64, drop_pct: f64) -> CategoryThreshold {
        CategoryThreshold {
            beta_min: beta,
            rc_min: rc,
            max_drop_pct: drop_pct,
        }
    }

    #[test]
    fn gate_pass_at_floor() {
        let outcome = evaluate_one(GateMode::Beta, 0.90, threshold(0.90, 0.95, 2.0), None);
        assert_eq!(outcome, MetricOutcome::Pass);
    }

    #[test]
    fn gate_fail_below_floor() {
        let outcome = evaluate_one(GateMode::Beta, 0.89, threshold(0.90, 0.95, 2.0), None);
        assert_eq!(outcome, MetricOutcome::BelowFloor { floor: 0.90 });
    }

    #[test]
    fn gate_fail_on_drop_exceeded() {
        let outcome = evaluate_one(GateMode::Beta, 0.91, threshold(0.90, 0.95, 2.0), Some(0.95));
        match outcome {
            MetricOutcome::ExceededDrop { previous, drop_pct } => {
                assert!((previous - 0.95).abs() < f64::EPSILON);
                assert!((drop_pct - 4.0).abs() < 1e-9);
            }
            other => panic!("expected ExceededDrop, got {other:?}"),
        }
    }

    #[test]
    fn gate_skips_delta_without_baseline() {
        let outcome = evaluate_one(GateMode::Beta, 0.91, threshold(0.90, 0.95, 2.0), None);
        assert_eq!(outcome, MetricOutcome::Pass);
    }

    #[test]
    fn gate_none_never_fails() {
        let outcome = evaluate_one(GateMode::None, 0.0, threshold(0.90, 0.95, 2.0), Some(1.0));
        assert_eq!(outcome, MetricOutcome::GateNone);
    }

    #[test]
    fn forget_completeness_intolerant_under_both_gates() {
        let t = threshold(1.0, 1.0, 0.0);
        assert!(matches!(
            evaluate_one(GateMode::Beta, 0.999, t, None),
            MetricOutcome::BelowFloor { .. }
        ));
        assert!(matches!(
            evaluate_one(GateMode::Rc, 0.999, t, None),
            MetricOutcome::BelowFloor { .. }
        ));
    }
}
```

- [ ] **Step 2: Re-export from `mod.rs`**

In `crates/cairn-bench/src/coherence/mod.rs`:

```rust
//! Coherence release-gate (issue #137).
//!
//! See `docs/design/2026-05-24-coherence-benchmarks-design.md`.

pub mod category;
pub mod score;
pub mod threshold;

pub use category::{ALL as ALL_CATEGORIES, DisplayCategory, MetricCategory, as_str};
pub use score::{CategoryScore, CategoryScores, ScoreError, aggregate, classify};
pub use threshold::{
    Baseline, CategoryThreshold, GateMode, MetricOutcome, MetricResult, ThresholdError,
    ThresholdManifest, all_pass, evaluate, load_manifest,
};
```

- [ ] **Step 3: Build and run tests**

```bash
cargo nextest run -p cairn-bench --locked coherence::threshold
```

Expected: PASS — six tests green.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-bench/src/coherence/mod.rs crates/cairn-bench/src/coherence/threshold.rs
git commit -m "feat(bench): coherence threshold manifest + gate evaluator

Adds ThresholdManifest (TOML), GateMode, and the per-metric outcome
evaluator. Forget-completeness's max_drop_pct=0.0 + rc_min=1.0 binding
is enforced at the data layer; no special-case code paths.

Refs #137"
```

---

## Task 7: Trend persistence (`trend.rs`)

**Files:**
- Create: `crates/cairn-bench/src/coherence/trend.rs`
- Modify: `crates/cairn-bench/src/coherence/mod.rs`

Append-only JSONL writer with per-line `schema_version` migrator.

- [ ] **Step 1: Write the failing tests**

Create `crates/cairn-bench/src/coherence/trend.rs`:

```rust
//! Append-only JSONL trend file with per-line schema migration.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::score::CategoryScore;

/// One trend entry — one line in `coherence-trend.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrendEntry {
    pub schema_version: u32,
    pub run_id: String,
    pub ts: String,
    pub cairn_version: String,
    pub git_sha: String,
    pub gate: String,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<String>,
    pub metrics: BTreeMap<String, CategoryScore>,
}

/// Trend load/append errors.
#[derive(Debug, thiserror::Error)]
pub enum TrendError {
    #[error("read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parse line {line} of {path}: {source}")]
    Json {
        path: String,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("missing schema_version on line {line} of {path}")]
    MissingSchemaVersion { path: String, line: usize },
    #[error("unknown schema_version {version} on line {line} of {path}")]
    UnknownSchemaVersion {
        path: String,
        line: usize,
        version: u64,
    },
}

/// Load every trend entry from disk, dispatching per line on `schema_version`.
///
/// # Errors
/// - `Io` for filesystem failures (except `NotFound`, which returns an empty vec).
/// - `Json` for malformed JSON.
/// - `MissingSchemaVersion` / `UnknownSchemaVersion` if a line cannot be classified.
pub fn load(path: &Path) -> Result<Vec<TrendEntry>, TrendError> {
    let file = match OpenOptions::new().read(true).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(TrendError::Io {
                path: path.display().to_string(),
                source,
            });
        }
    };
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let raw = line.map_err(|source| TrendError::Io {
            path: path.display().to_string(),
            source,
        })?;
        if raw.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&raw).map_err(|source| TrendError::Json {
            path: path.display().to_string(),
            line: idx + 1,
            source,
        })?;
        let version =
            value
                .get("schema_version")
                .and_then(Value::as_u64)
                .ok_or_else(|| TrendError::MissingSchemaVersion {
                    path: path.display().to_string(),
                    line: idx + 1,
                })?;
        let entry = match version {
            1 => from_v1(value).map_err(|source| TrendError::Json {
                path: path.display().to_string(),
                line: idx + 1,
                source,
            })?,
            other => {
                return Err(TrendError::UnknownSchemaVersion {
                    path: path.display().to_string(),
                    line: idx + 1,
                    version: other,
                });
            }
        };
        out.push(entry);
    }
    Ok(out)
}

fn from_v1(value: Value) -> Result<TrendEntry, serde_json::Error> {
    serde_json::from_value(value)
}

/// Append a single trend entry to disk. The write is a single
/// `write_all` call after a `write` open in `append` mode, so concurrent
/// appends from sibling processes interleave at line boundaries (POSIX
/// guarantees up to `PIPE_BUF`; the rendered lines are well under it).
///
/// # Errors
/// `TrendError::Io` on any filesystem failure or write error.
pub fn append(path: &Path, entry: &TrendEntry) -> Result<(), TrendError> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|source| TrendError::Io {
            path: path.display().to_string(),
            source,
        })?;
    let mut line = serde_json::to_string(entry).map_err(|source| TrendError::Json {
        path: path.display().to_string(),
        line: 0,
        source,
    })?;
    line.push('\n');
    file.write_all(line.as_bytes())
        .map_err(|source| TrendError::Io {
            path: path.display().to_string(),
            source,
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn fixture_entry(version: u32, run_id: &str) -> TrendEntry {
        let mut metrics = BTreeMap::new();
        metrics.insert(
            "recall_precision".to_owned(),
            CategoryScore {
                passed: 5,
                total: 5,
                score: 1.0,
            },
        );
        TrendEntry {
            schema_version: version,
            run_id: run_id.to_owned(),
            ts: "2026-05-24T12:00:00Z".to_owned(),
            cairn_version: "0.0.0".to_owned(),
            git_sha: "deadbeef".to_owned(),
            gate: "beta".to_owned(),
            outcome: "pass".to_owned(),
            failures: vec![],
            metrics,
        }
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("absent.jsonl");
        assert!(load(&path).unwrap().is_empty());
    }

    #[test]
    fn append_then_load_round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("trend.jsonl");
        let entry = fixture_entry(1, "run-a");
        append(&path, &entry).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded, vec![entry]);
    }

    #[test]
    fn load_missing_schema_version_fails_closed() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("trend.jsonl");
        std::fs::write(&path, b"{\"run_id\":\"x\"}\n").unwrap();
        let err = load(&path).unwrap_err();
        assert!(matches!(err, TrendError::MissingSchemaVersion { .. }));
    }

    #[test]
    fn load_unknown_schema_version_fails_closed() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("trend.jsonl");
        std::fs::write(&path, b"{\"schema_version\":99}\n").unwrap();
        let err = load(&path).unwrap_err();
        assert!(matches!(
            err,
            TrendError::UnknownSchemaVersion { version: 99, .. }
        ));
    }

    proptest! {
        #[test]
        fn append_atomic_under_concurrent_writes(
            ids in proptest::collection::vec("[a-z0-9]{8}", 4..8)
        ) {
            let dir = tempdir().unwrap();
            let path = Arc::new(dir.path().join("trend.jsonl"));
            let mut handles = Vec::new();
            for id in &ids {
                let p = Arc::clone(&path);
                let entry = fixture_entry(1, id);
                handles.push(std::thread::spawn(move || append(&p, &entry).unwrap()));
            }
            for h in handles {
                h.join().unwrap();
            }
            let loaded = load(&path).unwrap();
            prop_assert_eq!(loaded.len(), ids.len());
            // No partial lines: every loaded entry has schema_version 1.
            for entry in loaded {
                prop_assert_eq!(entry.schema_version, 1);
            }
        }
    }
}
```

- [ ] **Step 2: Re-export from `mod.rs`**

```rust
//! Coherence release-gate (issue #137).
//!
//! See `docs/design/2026-05-24-coherence-benchmarks-design.md`.

pub mod category;
pub mod score;
pub mod threshold;
pub mod trend;

pub use category::{ALL as ALL_CATEGORIES, DisplayCategory, MetricCategory, as_str};
pub use score::{CategoryScore, CategoryScores, ScoreError, aggregate, classify};
pub use threshold::{
    Baseline, CategoryThreshold, GateMode, MetricOutcome, MetricResult, ThresholdError,
    ThresholdManifest, all_pass, evaluate, load_manifest,
};
pub use trend::{TrendEntry, TrendError, append as append_trend, load as load_trend};
```

- [ ] **Step 3: Build and run tests**

```bash
cargo nextest run -p cairn-bench --locked coherence::trend
```

Expected: PASS — five tests + one proptest green.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-bench/src/coherence/mod.rs crates/cairn-bench/src/coherence/trend.rs
git commit -m "feat(bench): coherence trend persistence

Versioned JSONL with per-line schema_version dispatch. Append uses
POSIX O_APPEND semantics — lines are well under PIPE_BUF, so concurrent
writers interleave at line boundaries. Loader fails closed on unknown
or missing versions. Proptest verifies concurrent append safety.

Refs #137"
```

---

## Task 8: Report rendering (`report.rs`)

**Files:**
- Create: `crates/cairn-bench/src/coherence/report.rs`
- Modify: `crates/cairn-bench/src/coherence/mod.rs`

Renders the gate result as human text or JSON. Snapshot tests lock the shape.

- [ ] **Step 1: Write the failing tests**

Create `crates/cairn-bench/src/coherence/report.rs`:

```rust
//! Human + JSON rendering for coherence gate results.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Serialize;
use serde_json::json;

use super::category::{ALL as ALL_CATEGORIES, as_str};
use super::score::CategoryScore;
use super::threshold::{GateMode, MetricOutcome, MetricResult};
use cairn_test_fixtures::replay::MetricCategory;

/// Compact one-shot summary of a gate run, suitable for serialising to
/// JSON or appending into the trend file.
#[derive(Debug, Clone, Serialize)]
pub struct GateReport {
    pub schema_version: u32,
    pub gate: &'static str,
    pub outcome: &'static str,
    pub failures: Vec<&'static str>,
    pub metrics: BTreeMap<String, CategoryScore>,
    pub deltas: BTreeMap<String, Option<f64>>,
    pub overall: f64,
    pub cassettes: u32,
    pub actions: u32,
}

/// Build a `GateReport` from per-metric results.
#[must_use]
pub fn build(
    mode: GateMode,
    results: &BTreeMap<MetricCategory, MetricResult>,
    cassettes: u32,
    actions: u32,
) -> GateReport {
    let mut metrics: BTreeMap<String, CategoryScore> = BTreeMap::new();
    let mut deltas: BTreeMap<String, Option<f64>> = BTreeMap::new();
    let mut failures: Vec<&'static str> = Vec::new();
    let mut sum = 0.0_f64;
    for category in ALL_CATEGORIES {
        let result = results.get(&category).copied().unwrap_or(MetricResult {
            score: CategoryScore::empty(),
            outcome: MetricOutcome::Pass,
            delta: None,
        });
        let label = as_str(category);
        metrics.insert(label.to_owned(), result.score);
        deltas.insert(label.to_owned(), result.delta);
        if !result.outcome.is_pass() {
            failures.push(label);
        }
        sum += result.score.score;
    }
    let outcome = if failures.is_empty() { "pass" } else { "fail" };
    GateReport {
        schema_version: 1,
        gate: gate_label(mode),
        outcome,
        failures,
        metrics,
        deltas,
        overall: sum / 5.0,
        cassettes,
        actions,
    }
}

const fn gate_label(mode: GateMode) -> &'static str {
    match mode {
        GateMode::None => "none",
        GateMode::Beta => "beta",
        GateMode::Rc => "rc",
    }
}

/// Render a `GateReport` as a fixed-width human-readable table.
#[must_use]
pub fn render_human(report: &GateReport, results: &BTreeMap<MetricCategory, MetricResult>) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "coherence gate={}  cassettes={}  actions={}",
        report.gate, report.cassettes, report.actions,
    );
    for category in ALL_CATEGORIES {
        let label = as_str(category);
        let result = results.get(&category).copied().unwrap_or(MetricResult {
            score: CategoryScore::empty(),
            outcome: MetricOutcome::Pass,
            delta: None,
        });
        let verdict = if result.outcome.is_pass() { "pass" } else { "fail" };
        let delta = result
            .delta
            .map_or_else(|| "  n/a ".to_owned(), |d| format!("{d:+.3}"));
        let _ = writeln!(
            out,
            "  {label:<22} {:.3}  {verdict}  ({}/{})   Δ={}",
            result.score.score, result.score.passed, result.score.total, delta,
        );
    }
    let verdict = if report.outcome == "pass" { "PASS" } else { "FAIL" };
    let _ = writeln!(out, "  overall                {:.3}  {verdict}", report.overall);
    out
}

/// Render a `GateReport` as a JSON Value for `--json` output.
#[must_use]
pub fn render_json(report: &GateReport, trend_path: &str, run_id: &str) -> serde_json::Value {
    json!({
        "schema_version": report.schema_version,
        "gate": report.gate,
        "outcome": report.outcome,
        "failures": report.failures,
        "metrics": report.metrics,
        "deltas": report.deltas,
        "overall": report.overall,
        "cassettes": report.cassettes,
        "actions": report.actions,
        "trend_path": trend_path,
        "run_id": run_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_results() -> BTreeMap<MetricCategory, MetricResult> {
        let mut m = BTreeMap::new();
        for (category, passed, total, delta) in [
            (MetricCategory::RecallPrecision, 9, 9, Some(0.005)),
            (MetricCategory::StaleAvoidance, 3, 3, Some(0.0)),
            (MetricCategory::SummaryQuality, 3, 3, Some(0.02)),
            (MetricCategory::SearchUsefulness, 3, 3, Some(-0.005)),
            (MetricCategory::ForgetCompleteness, 3, 3, Some(0.0)),
        ] {
            m.insert(
                category,
                MetricResult {
                    score: CategoryScore {
                        passed,
                        total,
                        score: f64::from(passed) / f64::from(total),
                    },
                    outcome: MetricOutcome::Pass,
                    delta,
                },
            );
        }
        m
    }

    #[test]
    fn build_marks_pass_when_no_failures() {
        let results = fixture_results();
        let report = build(GateMode::Beta, &results, 3, 21);
        assert_eq!(report.outcome, "pass");
        assert!(report.failures.is_empty());
        assert!((report.overall - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn build_marks_fail_when_any_category_fails() {
        let mut results = fixture_results();
        results.insert(
            MetricCategory::SearchUsefulness,
            MetricResult {
                score: CategoryScore {
                    passed: 1,
                    total: 3,
                    score: 1.0 / 3.0,
                },
                outcome: MetricOutcome::BelowFloor { floor: 0.85 },
                delta: Some(-0.5),
            },
        );
        let report = build(GateMode::Beta, &results, 3, 21);
        assert_eq!(report.outcome, "fail");
        assert_eq!(report.failures, vec!["search_usefulness"]);
    }

    #[test]
    fn human_output_snapshot() {
        let results = fixture_results();
        let report = build(GateMode::Beta, &results, 3, 21);
        let rendered = render_human(&report, &results);
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn json_output_snapshot() {
        let results = fixture_results();
        let report = build(GateMode::Beta, &results, 3, 21);
        let rendered = render_json(
            &report,
            "crates/cairn-bench/baselines/coherence-trend.jsonl",
            "01J000000000000000000000RUN",
        );
        insta::assert_json_snapshot!(rendered);
    }
}
```

- [ ] **Step 2: Re-export from `mod.rs`**

```rust
pub mod category;
pub mod report;
pub mod score;
pub mod threshold;
pub mod trend;

pub use category::{ALL as ALL_CATEGORIES, DisplayCategory, MetricCategory, as_str};
pub use report::{GateReport, build as build_report, render_human, render_json};
pub use score::{CategoryScore, CategoryScores, ScoreError, aggregate, classify};
pub use threshold::{
    Baseline, CategoryThreshold, GateMode, MetricOutcome, MetricResult, ThresholdError,
    ThresholdManifest, all_pass, evaluate, load_manifest,
};
pub use trend::{TrendEntry, TrendError, append as append_trend, load as load_trend};
```

- [ ] **Step 3: Build and accept snapshots**

```bash
cargo nextest run -p cairn-bench --locked coherence::report
```

Expected on first run: snapshot tests FAIL with two pending `.snap.new` files. Review and accept:

```bash
cargo insta accept --workspace
cargo nextest run -p cairn-bench --locked coherence::report
```

Expected on second run: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-bench/src/coherence/mod.rs crates/cairn-bench/src/coherence/report.rs crates/cairn-bench/src/coherence/snapshots
git commit -m "feat(bench): coherence human + JSON report rendering

Builds GateReport from per-metric results and renders either a
fixed-width terminal table or a JSON Value. Snapshots locked under
insta so downstream tooling can rely on the JSON shape.

Refs #137"
```

---

## Task 9: Orchestrator `run_coherence_gate`

**Files:**
- Modify: `crates/cairn-bench/src/coherence/mod.rs`

Wires replay → score → evaluate → render → append. Returns a typed outcome the binary turns into an exit code.

- [ ] **Step 1: Write the failing test**

Append to `crates/cairn-bench/src/coherence/mod.rs` (or extract to a sibling `tests` module if it gets long — start inline):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn run_coherence_gate_against_extended_cassettes_passes_beta() {
        let workspace = workspace_root();
        let manifest = workspace
            .join("crates/cairn-bench/manifests/coherence.toml");
        let baseline = workspace
            .join("crates/cairn-bench/baselines/coherence.json");
        let dir = tempdir().unwrap();
        let trend = dir.path().join("trend.jsonl");

        let opts = GateOptions {
            mode: GateMode::Beta,
            cassettes_dir: workspace.join("fixtures/v0/replay"),
            include: vec![
                "research_domain".to_owned(),
                "engineering_domain".to_owned(),
                "support_domain".to_owned(),
            ],
            manifest_path: manifest,
            baseline_path: Some(baseline),
            trend_path: trend.clone(),
            update_baseline: false,
            write_trend: true,
            cairn_version: env!("CARGO_PKG_VERSION").to_owned(),
            git_sha: "test".to_owned(),
            now: "2026-05-24T12:00:00Z".to_owned(),
            run_id: "01J000000000000000000000RUN".to_owned(),
        };
        let outcome = run_coherence_gate(opts).await.expect("gate run");
        assert!(outcome.gate_passed, "gate failed: {outcome:?}");
        let appended = trend::load(&trend).expect("load trend");
        assert_eq!(appended.len(), 1);
    }

    fn workspace_root() -> std::path::PathBuf {
        // CARGO_MANIFEST_DIR is .../crates/cairn-bench; go up two levels.
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("workspace root")
            .to_path_buf()
    }
}
```

- [ ] **Step 2: Add the orchestrator API**

Append to `crates/cairn-bench/src/coherence/mod.rs` (above the `#[cfg(test)]` block):

```rust
use std::path::PathBuf;

/// Knobs for one gate run. Constructed by the CLI wrapper in `main.rs`.
#[derive(Debug, Clone)]
pub struct GateOptions {
    pub mode: GateMode,
    pub cassettes_dir: PathBuf,
    pub include: Vec<String>,
    pub manifest_path: PathBuf,
    pub baseline_path: Option<PathBuf>,
    pub trend_path: PathBuf,
    pub update_baseline: bool,
    pub write_trend: bool,
    pub cairn_version: String,
    pub git_sha: String,
    pub now: String,
    pub run_id: String,
}

/// What `run_coherence_gate` returns. The caller maps `gate_passed` to a
/// process exit code.
#[derive(Debug, Clone)]
pub struct GateOutcome {
    pub gate_passed: bool,
    pub report: GateReport,
    pub human: String,
}

/// Errors surfaced from the orchestrator.
#[derive(Debug, thiserror::Error)]
pub enum GateError {
    #[error(transparent)]
    Replay(#[from] cairn_test_fixtures::replay::ReplayError),
    #[error(transparent)]
    Score(#[from] ScoreError),
    #[error(transparent)]
    Threshold(#[from] ThresholdError),
    #[error(transparent)]
    Trend(#[from] TrendError),
    #[error("baseline {path}: {source}")]
    BaselineIo {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("baseline {path}: {source}")]
    BaselineJson {
        path: String,
        #[source]
        source: serde_json::Error,
    },
}

/// Drive every included cassette through the replay engine, score per
/// category, evaluate the gate, render the report, optionally write the
/// trend line, and optionally rewrite the baseline.
///
/// # Errors
/// Returns any error from replay, scoring, threshold loading, or trend I/O.
pub async fn run_coherence_gate(opts: GateOptions) -> Result<GateOutcome, GateError> {
    let manifest = load_manifest(&opts.manifest_path)?;
    let baseline = match &opts.baseline_path {
        Some(p) if p.exists() => Some(load_baseline(p)?),
        _ => None,
    };

    let mut all_actions: Vec<cairn_test_fixtures::replay::ReplayAction> = Vec::new();
    let mut all_reports: Vec<cairn_test_fixtures::replay::ReplayCheckReport> = Vec::new();
    for cassette in &opts.include {
        let path = opts.cassettes_dir.join(format!("{cassette}.json"));
        let scenario = cairn_test_fixtures::replay::load_scenario_file(&path)?;
        let report = cairn_test_fixtures::replay::run_scenario(&scenario).await?;
        all_actions.extend(scenario.actions);
        all_reports.extend(report.checks);
    }
    let cassettes = u32::try_from(opts.include.len()).unwrap_or(u32::MAX);
    let actions = u32::try_from(all_actions.len()).unwrap_or(u32::MAX);

    let scores = aggregate(&all_actions, &all_reports)?;
    let results = evaluate(opts.mode, &scores, &manifest, baseline.as_ref());
    let gate_passed = all_pass(&results);
    let report = build_report(opts.mode, &results, cassettes, actions);
    let human = render_human(&report, &results);

    if opts.write_trend {
        let entry = TrendEntry {
            schema_version: 1,
            run_id: opts.run_id.clone(),
            ts: opts.now.clone(),
            cairn_version: opts.cairn_version.clone(),
            git_sha: opts.git_sha.clone(),
            gate: report.gate.to_owned(),
            outcome: report.outcome.to_owned(),
            failures: report.failures.iter().map(|s| (*s).to_owned()).collect(),
            metrics: report.metrics.clone(),
        };
        append_trend(&opts.trend_path, &entry)?;
    }

    if opts.update_baseline
        && let Some(path) = &opts.baseline_path
    {
        write_baseline(
            path,
            &Baseline {
                schema_version: 1,
                captured_at: opts.now.clone(),
                cairn_version: opts.cairn_version.clone(),
                git_sha: opts.git_sha.clone(),
                metrics: report.metrics.clone(),
            },
        )?;
    }

    Ok(GateOutcome {
        gate_passed,
        report,
        human,
    })
}

fn load_baseline(path: &std::path::Path) -> Result<Baseline, GateError> {
    let raw = std::fs::read_to_string(path).map_err(|source| GateError::BaselineIo {
        path: path.display().to_string(),
        source,
    })?;
    serde_json::from_str(&raw).map_err(|source| GateError::BaselineJson {
        path: path.display().to_string(),
        source,
    })
}

fn write_baseline(path: &std::path::Path, baseline: &Baseline) -> Result<(), GateError> {
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(baseline).map_err(|source| GateError::BaselineJson {
        path: path.display().to_string(),
        source,
    })?;
    std::fs::write(&tmp, body).map_err(|source| GateError::BaselineIo {
        path: tmp.display().to_string(),
        source,
    })?;
    std::fs::rename(&tmp, path).map_err(|source| GateError::BaselineIo {
        path: path.display().to_string(),
        source,
    })?;
    Ok(())
}
```

- [ ] **Step 3: Build (test will fail because baseline + manifest do not exist yet)**

```bash
cargo check -p cairn-bench --locked
```

Expected: PASS. The test will fail at runtime — the manifest/baseline fixtures don't exist yet. That is fine; Task 11 lands them. Skip the orchestrator test for now:

```bash
cargo nextest run -p cairn-bench --locked coherence --no-run
```

Expected: tests compile.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-bench/src/coherence/mod.rs
git commit -m "feat(bench): coherence orchestrator run_coherence_gate

GateOptions/GateOutcome plumb through cassette replay, score
aggregation, gate evaluation, trend append, and atomic baseline
rewrite. Baseline write uses temp-file + rename for crash safety.

Refs #137"
```

---

## Task 10: JSON Schemas for manifest, baseline, trend

**Files:**
- Create: `crates/cairn-bench/schemas/coherence-threshold.schema.json`
- Create: `crates/cairn-bench/schemas/coherence-baseline.schema.json`
- Create: `crates/cairn-bench/schemas/coherence-trend.schema.json`

These guard the shapes that downstream tooling depends on. The smoke test (Task 13) validates the live fixtures against them.

- [ ] **Step 1: Create `coherence-threshold.schema.json`**

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "Cairn coherence threshold manifest",
  "type": "object",
  "required": [
    "schema_version",
    "recall_precision",
    "stale_avoidance",
    "summary_quality",
    "search_usefulness",
    "forget_completeness"
  ],
  "properties": {
    "schema_version": { "type": "integer", "const": 1 },
    "recall_precision":    { "$ref": "#/$defs/category" },
    "stale_avoidance":     { "$ref": "#/$defs/category" },
    "summary_quality":     { "$ref": "#/$defs/category" },
    "search_usefulness":   { "$ref": "#/$defs/category" },
    "forget_completeness": { "$ref": "#/$defs/category" }
  },
  "additionalProperties": false,
  "$defs": {
    "category": {
      "type": "object",
      "required": ["beta_min", "rc_min", "max_drop_pct"],
      "properties": {
        "beta_min":     { "type": "number", "minimum": 0, "maximum": 1 },
        "rc_min":       { "type": "number", "minimum": 0, "maximum": 1 },
        "max_drop_pct": { "type": "number", "minimum": 0, "maximum": 100 }
      },
      "additionalProperties": false
    }
  }
}
```

- [ ] **Step 2: Create `coherence-baseline.schema.json`**

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "Cairn coherence baseline",
  "type": "object",
  "required": ["schema_version", "captured_at", "cairn_version", "git_sha", "metrics"],
  "properties": {
    "schema_version": { "type": "integer", "const": 1 },
    "captured_at":    { "type": "string", "format": "date-time" },
    "cairn_version":  { "type": "string" },
    "git_sha":        { "type": "string" },
    "metrics": {
      "type": "object",
      "required": [
        "recall_precision",
        "stale_avoidance",
        "summary_quality",
        "search_usefulness",
        "forget_completeness"
      ],
      "properties": {
        "recall_precision":    { "$ref": "#/$defs/score" },
        "stale_avoidance":     { "$ref": "#/$defs/score" },
        "summary_quality":     { "$ref": "#/$defs/score" },
        "search_usefulness":   { "$ref": "#/$defs/score" },
        "forget_completeness": { "$ref": "#/$defs/score" }
      },
      "additionalProperties": false
    }
  },
  "additionalProperties": false,
  "$defs": {
    "score": {
      "type": "object",
      "required": ["score", "passed", "total"],
      "properties": {
        "score":  { "type": "number", "minimum": 0, "maximum": 1 },
        "passed": { "type": "integer", "minimum": 0 },
        "total":  { "type": "integer", "minimum": 0 }
      },
      "additionalProperties": false
    }
  }
}
```

- [ ] **Step 3: Create `coherence-trend.schema.json`**

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "Cairn coherence trend entry (one per JSONL line)",
  "type": "object",
  "required": ["schema_version", "run_id", "ts", "cairn_version", "git_sha", "gate", "outcome", "metrics"],
  "properties": {
    "schema_version": { "type": "integer", "const": 1 },
    "run_id":         { "type": "string" },
    "ts":             { "type": "string", "format": "date-time" },
    "cairn_version":  { "type": "string" },
    "git_sha":        { "type": "string" },
    "gate":           { "type": "string", "enum": ["beta", "rc", "none"] },
    "outcome":        { "type": "string", "enum": ["pass", "fail"] },
    "failures":       { "type": "array", "items": { "type": "string" } },
    "metrics": {
      "type": "object",
      "additionalProperties": {
        "type": "object",
        "required": ["score", "passed", "total"],
        "properties": {
          "score":  { "type": "number", "minimum": 0, "maximum": 1 },
          "passed": { "type": "integer", "minimum": 0 },
          "total":  { "type": "integer", "minimum": 0 }
        },
        "additionalProperties": false
      }
    }
  },
  "additionalProperties": false
}
```

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-bench/schemas/coherence-threshold.schema.json crates/cairn-bench/schemas/coherence-baseline.schema.json crates/cairn-bench/schemas/coherence-trend.schema.json
git commit -m "docs(bench): JSON schemas for coherence manifest/baseline/trend

Per-file shape contract for downstream tooling and the in-process
jsonschema smoke test (issue #137).

Refs #137"
```

---

## Task 11: Threshold manifest + initial baseline + empty trend file

**Files:**
- Create: `crates/cairn-bench/manifests/coherence.toml`
- Create: `crates/cairn-bench/baselines/coherence.json`
- Create: `crates/cairn-bench/baselines/coherence-trend.jsonl`

Seeds the live data the orchestrator reads. The initial baseline is committed as a "best guess from local seed run" — Task 16 may adjust if the live scores show the placeholder is wrong.

- [ ] **Step 1: Create `manifests/coherence.toml`**

```toml
# Coherence release gate thresholds (issue #137).
# See docs/design/2026-05-24-coherence-benchmarks-design.md §6.
#
# Floors below are seed values. Adjust upward only after the trend file
# shows several runs comfortably above them. Lowering a floor requires
# explicit reviewer sign-off in the PR description.

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

- [ ] **Step 2: Seed the baseline locally**

Run the orchestrator once with `--update-baseline` to produce the real numbers from the live cassettes. (The CLI subcommand is added in Task 12; for this task, capture the same effect with a one-shot Rust script.) Skip ahead: the easiest path is to defer Step 2's content until Task 12 is in place. For now, commit a placeholder baseline that matches the manifest floors so the gate passes:

Create `crates/cairn-bench/baselines/coherence.json`:

```json
{
  "schema_version": 1,
  "captured_at": "2026-05-24T00:00:00Z",
  "cairn_version": "0.0.0",
  "git_sha": "placeholder",
  "metrics": {
    "recall_precision":    { "score": 0.90, "passed": 0, "total": 0 },
    "stale_avoidance":     { "score": 0.95, "passed": 0, "total": 0 },
    "summary_quality":     { "score": 0.85, "passed": 0, "total": 0 },
    "search_usefulness":   { "score": 0.85, "passed": 0, "total": 0 },
    "forget_completeness": { "score": 1.00, "passed": 0, "total": 0 }
  }
}
```

This placeholder will be rewritten in Task 16 after the binary is wired and we run `--update-baseline` against the live cassettes.

- [ ] **Step 3: Create the empty trend file**

Create `crates/cairn-bench/baselines/coherence-trend.jsonl` as a zero-byte file:

```bash
: > crates/cairn-bench/baselines/coherence-trend.jsonl
```

- [ ] **Step 4: Verify the orchestrator unit test now passes**

```bash
cargo nextest run -p cairn-bench --locked coherence::tests::run_coherence_gate_against_extended_cassettes_passes_beta
```

Expected: PASS — the test from Task 9 now finds its manifest and baseline.

If the test fails because real coherence scores exceed the placeholder floor by less than `max_drop_pct` (false-positive `ExceededDrop`), that means the placeholder baseline's `0.90` etc. is *higher* than the actual run's score. Verify the live numbers by inspecting the test failure output, then update `coherence.json` placeholder values to match (or set placeholder scores to the gate floor so delta = 0).

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-bench/manifests/coherence.toml crates/cairn-bench/baselines/coherence.json crates/cairn-bench/baselines/coherence-trend.jsonl
git commit -m "feat(bench): seed coherence threshold manifest + placeholder baseline

Manifest seeds beta/rc floors and the 2% regression delta per design.
Baseline is a placeholder set to the floors; Task 16 rewrites it with
real seed numbers once the CLI is wired and a local --update-baseline
run produces them. Empty trend file lands so CI doesn't have to handle
the missing-file edge case on first run.

Refs #137"
```

---

## Task 12: CLI subcommand wiring (`main.rs`)

**Files:**
- Modify: `crates/cairn-bench/src/main.rs`
- Modify: `crates/cairn-bench/src/coherence/mod.rs` (add CLI args struct + run helper)

Adds `cairn-bench coherence run --gate beta|rc|none [flags]`.

- [ ] **Step 1: Add `CoherenceArgs` + dispatch helper to `coherence/mod.rs`**

Append to `crates/cairn-bench/src/coherence/mod.rs` (above the `#[cfg(test)]` block):

```rust
use clap::{Args, ValueEnum};

/// CLI args block for the `coherence` subcommand.
#[derive(Debug, Args)]
pub struct CoherenceArgs {
    #[command(subcommand)]
    pub cmd: CoherenceCmd,
}

#[derive(Debug, clap::Subcommand)]
pub enum CoherenceCmd {
    /// Run the coherence gate over the configured cassettes.
    Run(CoherenceRunArgs),
}

/// Arguments for `cairn-bench coherence run`.
#[derive(Debug, Args)]
pub struct CoherenceRunArgs {
    /// Gate mode.
    #[arg(long, value_enum, default_value_t = CliGate::Beta)]
    pub gate: CliGate,
    /// Cassettes directory.
    #[arg(long, default_value = "fixtures/v0/replay")]
    pub cassettes: PathBuf,
    /// Cassettes to include (default: extended #136 cassettes).
    #[arg(long = "include", num_args = 1.., default_values_t = default_includes())]
    pub include: Vec<String>,
    /// Threshold manifest path.
    #[arg(long, default_value = "crates/cairn-bench/manifests/coherence.toml")]
    pub manifest: PathBuf,
    /// Baseline path.
    #[arg(long, default_value = "crates/cairn-bench/baselines/coherence.json")]
    pub baseline: PathBuf,
    /// Trend file path.
    #[arg(long, default_value = "crates/cairn-bench/baselines/coherence-trend.jsonl")]
    pub trend: PathBuf,
    /// Overwrite the baseline with this run's scores.
    #[arg(long)]
    pub update_baseline: bool,
    /// Skip appending to the trend file.
    #[arg(long)]
    pub no_trend_write: bool,
    /// Machine-readable output on stdout.
    #[arg(long)]
    pub json: bool,
}

/// `clap` value-enum for `--gate`.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliGate {
    Beta,
    Rc,
    None,
}

impl From<CliGate> for GateMode {
    fn from(v: CliGate) -> Self {
        match v {
            CliGate::Beta => Self::Beta,
            CliGate::Rc => Self::Rc,
            CliGate::None => Self::None,
        }
    }
}

fn default_includes() -> Vec<String> {
    vec![
        "research_domain".to_owned(),
        "engineering_domain".to_owned(),
        "support_domain".to_owned(),
    ]
}

/// Dispatch the `coherence` subcommand. Returns the process exit code.
///
/// # Errors
/// Returns any orchestrator error (replay, score, threshold, trend, baseline I/O).
pub async fn dispatch(args: CoherenceArgs) -> Result<u8, GateError> {
    match args.cmd {
        CoherenceCmd::Run(run) => {
            let opts = GateOptions {
                mode: run.gate.into(),
                cassettes_dir: run.cassettes,
                include: run.include,
                manifest_path: run.manifest,
                baseline_path: Some(run.baseline),
                trend_path: run.trend,
                update_baseline: run.update_baseline,
                write_trend: !run.no_trend_write,
                cairn_version: env!("CARGO_PKG_VERSION").to_owned(),
                git_sha: std::env::var("GIT_SHA").unwrap_or_else(|_| "unknown".to_owned()),
                now: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                run_id: ulid_like(),
            };
            let outcome = run_coherence_gate(opts).await?;
            if run.json {
                let value = render_json(
                    &outcome.report,
                    "crates/cairn-bench/baselines/coherence-trend.jsonl",
                    "ulid-placeholder",
                );
                println!("{}", serde_json::to_string_pretty(&value).unwrap_or_default());
            } else {
                println!("{}", outcome.human);
            }
            Ok(if outcome.gate_passed { 0 } else { 69 })
        }
    }
}

fn ulid_like() -> String {
    // Lightweight monotonic-ish id; full ULID would pull in a dependency.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("run-{nanos:032x}")
}
```

- [ ] **Step 2: Wire the subcommand in `main.rs`**

Modify `crates/cairn-bench/src/main.rs`:

```rust
#![forbid(unsafe_code)]
//! cairn-bench binary entry point — subcommand dispatcher.

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "cairn-bench",
    about = "Cairn bench harness: scorecard + release gates."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// `BrainBench` retrieval-quality scorecard runner.
    Scorecard(cairn_bench::scorecard::ScorecardArgs),
    /// Latency regression gate: runs criterion, compares to baseline, writes report.
    Latency(cairn_bench::latency::LatencyArgs),
    /// Memory budget gate: measures binary + embedding model against manifest budget.
    Memory(cairn_bench::memory::MemoryArgs),
    /// Privacy leakage gate: parse fixtures and optionally run them.
    Privacy(cairn_bench::privacy::PrivacyArgs),
    /// SRE release gate: writes `sre.json` for `cairn admin sre report` import.
    Sre(cairn_bench::sre::SreArgs),
    /// Coherence release gate: scores extended cassettes against the threshold manifest.
    Coherence(cairn_bench::coherence::CoherenceArgs),
    /// Run latency + memory + privacy + SRE and exit non-zero on any failure.
    All {
        /// Skip one or more gates by name (latency, memory, privacy, sre).
        #[arg(long)]
        skip: Vec<String>,
    },
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Scorecard(args) => cairn_bench::scorecard::run(args).await,
        Cmd::Latency(args) => {
            let outcome = cairn_bench::latency::run(&args)?;
            std::process::exit(outcome.exit_code().into());
        }
        Cmd::Memory(args) => {
            let outcome = cairn_bench::memory::run(&args)?;
            std::process::exit(outcome.exit_code().into());
        }
        Cmd::Privacy(args) => {
            let outcome = cairn_bench::privacy::run(&args)?;
            std::process::exit(outcome.exit_code().into());
        }
        Cmd::Sre(args) => {
            let outcome = cairn_bench::sre::run(&args)?;
            std::process::exit(outcome.exit_code().into());
        }
        Cmd::Coherence(args) => {
            let code = cairn_bench::coherence::dispatch(args).await?;
            std::process::exit(code.into());
        }
        Cmd::All { skip } => {
            let outcome = cairn_bench::all::run(&cairn_bench::all::AllArgs { skip })?;
            std::process::exit(outcome.exit_code().into());
        }
    }
}
```

- [ ] **Step 3: Check that `chrono` is already a dep**

```bash
grep '^chrono' crates/cairn-bench/Cargo.toml
```

Expected: a line matching `chrono = { workspace = true, ... }`. It is already pulled in for adapter.rs.

- [ ] **Step 4: Build and smoke-test the binary**

```bash
cargo build -p cairn-bench --locked
./target/debug/cairn-bench coherence run --gate none --no-trend-write
```

Expected: process exits 0; prints a human table with `gate=none` and all `pass`.

- [ ] **Step 5: Try a failing gate**

```bash
./target/debug/cairn-bench coherence run --gate rc --no-trend-write
```

Expected: depends on real scores. If `forget_completeness == 1.0` and all other scores `>= rc_min`, exits 0. Otherwise exits 69 with one or more `fail` rows.

If unexpectedly fails: the placeholder baseline in Task 11 may be too high. Skip ahead to Task 16's recalibration; do not lower thresholds in this commit.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-bench/src/coherence/mod.rs crates/cairn-bench/src/main.rs
git commit -m "feat(bench): cairn-bench coherence run subcommand

Adds CoherenceArgs + dispatch under the existing cairn-bench binary.
Exit code 0 on pass, 69 (EX_UNAVAILABLE) on gate failure per the
brief's sysexits convention. Default --include set is the three #136
extended cassettes; --gate defaults to beta.

Refs #137"
```

---

## Task 13: Integration smoke tests (`tests/coherence_smoke.rs`)

**Files:**
- Create: `crates/cairn-bench/tests/coherence_smoke.rs`

Cross-module integration: real cassettes through the orchestrator + schema validation.

- [ ] **Step 1: Create the smoke test file**

Create `crates/cairn-bench/tests/coherence_smoke.rs`:

```rust
//! Integration smoke tests for the coherence release gate.

use std::path::{Path, PathBuf};

use cairn_bench::coherence::{
    ALL_CATEGORIES, GateMode, GateOptions, MetricCategory, aggregate, classify, run_coherence_gate,
};
use cairn_test_fixtures::replay::{load_scenario_file, run_scenario};
use jsonschema::Validator;
use tempfile::tempdir;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn manifest_path() -> PathBuf {
    workspace_root().join("crates/cairn-bench/manifests/coherence.toml")
}

fn baseline_path() -> PathBuf {
    workspace_root().join("crates/cairn-bench/baselines/coherence.json")
}

fn schema(path: &str) -> Validator {
    let raw = std::fs::read_to_string(workspace_root().join(path)).expect("read schema");
    let value: serde_json::Value = serde_json::from_str(&raw).expect("parse schema");
    Validator::new(&value).expect("compile schema")
}

#[tokio::test]
async fn extended_cassettes_pass_beta_gate() {
    let dir = tempdir().unwrap();
    let opts = GateOptions {
        mode: GateMode::Beta,
        cassettes_dir: workspace_root().join("fixtures/v0/replay"),
        include: vec![
            "research_domain".to_owned(),
            "engineering_domain".to_owned(),
            "support_domain".to_owned(),
        ],
        manifest_path: manifest_path(),
        baseline_path: Some(baseline_path()),
        trend_path: dir.path().join("trend.jsonl"),
        update_baseline: false,
        write_trend: true,
        cairn_version: env!("CARGO_PKG_VERSION").to_owned(),
        git_sha: "smoke".to_owned(),
        now: "2026-05-24T12:00:00Z".to_owned(),
        run_id: "01J000000000000000000000RUN".to_owned(),
    };
    let outcome = run_coherence_gate(opts).await.expect("gate run");
    assert!(
        outcome.gate_passed,
        "beta gate must pass against extended cassettes: {}",
        outcome.human
    );
}

#[tokio::test]
async fn untagged_actions_excluded_from_scoring() {
    // P0 cassettes (no metric_category tags) should yield empty buckets.
    let scenario = load_scenario_file(
        &workspace_root().join("fixtures/v0/replay/p0_stories.json"),
    )
    .expect("load p0_stories");
    let report = run_scenario(&scenario).await.expect("run p0_stories");
    let scores = aggregate(&scenario.actions, &report.checks).expect("aggregate");
    for category in ALL_CATEGORIES {
        let s = scores[&category];
        assert_eq!(
            s.total, 0,
            "category {:?} should be empty for untagged cassette",
            category
        );
    }
}

#[tokio::test]
async fn extended_cassettes_cover_all_five_categories() {
    let mut covered = std::collections::BTreeSet::<MetricCategory>::new();
    for cassette in ["research_domain", "engineering_domain", "support_domain"] {
        let scenario = load_scenario_file(
            &workspace_root().join(format!("fixtures/v0/replay/{cassette}.json")),
        )
        .expect("load cassette");
        for action in &scenario.actions {
            if let Some(c) = classify(action) {
                covered.insert(c);
            }
        }
    }
    for category in ALL_CATEGORIES {
        assert!(
            covered.contains(&category),
            "extended cassettes must cover {:?}; got {:?}",
            category,
            covered
        );
    }
}

#[test]
fn live_manifest_validates_against_schema() {
    let validator = schema("crates/cairn-bench/schemas/coherence-threshold.schema.json");
    // toml -> json
    let raw_toml = std::fs::read_to_string(manifest_path()).expect("read manifest");
    let parsed: toml::Value = toml::from_str(&raw_toml).expect("parse toml");
    let as_json = serde_json::to_value(&parsed).expect("toml->json");
    validator
        .validate(&as_json)
        .expect("manifest schema validation");
}

#[test]
fn live_baseline_validates_against_schema() {
    let validator = schema("crates/cairn-bench/schemas/coherence-baseline.schema.json");
    let raw = std::fs::read_to_string(baseline_path()).expect("read baseline");
    let value: serde_json::Value = serde_json::from_str(&raw).expect("parse baseline json");
    validator
        .validate(&value)
        .expect("baseline schema validation");
}

#[tokio::test]
async fn trend_line_validates_against_schema() {
    let dir = tempdir().unwrap();
    let trend = dir.path().join("trend.jsonl");
    let opts = GateOptions {
        mode: GateMode::None,
        cassettes_dir: workspace_root().join("fixtures/v0/replay"),
        include: vec!["research_domain".to_owned()],
        manifest_path: manifest_path(),
        baseline_path: None,
        trend_path: trend.clone(),
        update_baseline: false,
        write_trend: true,
        cairn_version: env!("CARGO_PKG_VERSION").to_owned(),
        git_sha: "smoke".to_owned(),
        now: "2026-05-24T12:00:00Z".to_owned(),
        run_id: "01J000000000000000000000RUN".to_owned(),
    };
    let _ = run_coherence_gate(opts).await.expect("gate run");
    let body = std::fs::read_to_string(&trend).expect("read trend");
    let line = body.lines().next().expect("at least one trend line");
    let value: serde_json::Value = serde_json::from_str(line).expect("parse trend line");
    let validator = schema("crates/cairn-bench/schemas/coherence-trend.schema.json");
    validator
        .validate(&value)
        .expect("trend line schema validation");
}

#[tokio::test]
async fn cli_exit_code_69_on_failing_gate() {
    // Construct a manifest that forces failure (recall_precision floor = 1.0).
    let dir = tempdir().unwrap();
    let fake_manifest = dir.path().join("coherence.toml");
    std::fs::write(
        &fake_manifest,
        r#"schema_version = 1
[recall_precision]
beta_min = 1.001
rc_min = 1.001
max_drop_pct = 0.0
[stale_avoidance]
beta_min = 0.0
rc_min = 0.0
max_drop_pct = 100.0
[summary_quality]
beta_min = 0.0
rc_min = 0.0
max_drop_pct = 100.0
[search_usefulness]
beta_min = 0.0
rc_min = 0.0
max_drop_pct = 100.0
[forget_completeness]
beta_min = 0.0
rc_min = 0.0
max_drop_pct = 100.0
"#,
    )
    .unwrap();
    let opts = GateOptions {
        mode: GateMode::Beta,
        cassettes_dir: workspace_root().join("fixtures/v0/replay"),
        include: vec!["research_domain".to_owned()],
        manifest_path: fake_manifest,
        baseline_path: None,
        trend_path: dir.path().join("trend.jsonl"),
        update_baseline: false,
        write_trend: false,
        cairn_version: env!("CARGO_PKG_VERSION").to_owned(),
        git_sha: "smoke".to_owned(),
        now: "2026-05-24T12:00:00Z".to_owned(),
        run_id: "01J000000000000000000000RUN".to_owned(),
    };
    let outcome = run_coherence_gate(opts).await.expect("gate run");
    assert!(!outcome.gate_passed, "gate should have failed");
    assert!(outcome.report.failures.contains(&"recall_precision"));
}

```

- [ ] **Step 2: Run the smoke tests**

```bash
cargo nextest run -p cairn-bench --locked --test coherence_smoke
```

Expected: PASS — six smoke tests green.

If any of the schema-validation tests fails: the data layer is correct, and the schema needs to relax a constraint. Investigate the validation error message and update the matching schema file under `crates/cairn-bench/schemas/` — do not loosen the data.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-bench/tests/coherence_smoke.rs
git commit -m "test(bench): coherence smoke tests

Integration: real cassettes through the orchestrator, P0 cassette
exclusion, all-five-categories coverage, schema validation of live
manifest/baseline/trend, and CLI exit-code-69-on-failure.

Refs #137"
```

---

## Task 14: CI job `coherence-gate`

**Files:**
- Modify: `.github/workflows/ci.yml`

Mirrors the existing `gates` job shape. Runs once per PR and on `main`.

- [ ] **Step 1: Add the job to `ci.yml`**

In `.github/workflows/ci.yml`, after the existing `gates:` job (around line 165 — find it by searching for `name: bench-reports`), insert a new job:

```yaml
  coherence-gate:
    name: gates / coherence (§15, §18 #5, #137)
    needs: gates
    runs-on: ubuntu-latest
    steps:
      - name: Checkout
        uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd # v6.0.2
      - name: Install toolchain (rust-toolchain.toml)
        run: rustup show active-toolchain || rustup toolchain install
      - name: Cache cargo build
        uses: Swatinem/rust-cache@c19371144df3bb44fab255c43d04cbc2ab54d1c4 # v2.9.1
        with:
          shared-key: coherence-gate
          cache-bin: "false"
          save-if: ${{ github.ref == 'refs/heads/main' }}
      - name: Build cairn + cairn-bench binaries
        run: |
          cargo build --release --locked -p cairn-cli --bin cairn
          cargo build --release --locked -p cairn-bench
      - name: Run coherence gate
        run: |
          ./target/release/cairn-bench coherence run \
            --gate ${{ startsWith(github.ref, 'refs/heads/release/') && 'rc' || 'beta' }}
        env:
          CAIRN_MOCK_EMBEDDER: "1"
          CAIRN_KEYSTORE: "file"
          GIT_SHA: ${{ github.sha }}
      - name: Upload coherence report
        if: always()
        uses: actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02 # v4.6.2
        with:
          name: coherence-report
          path: crates/cairn-bench/baselines/coherence-trend.jsonl
          retention-days: 14
```

- [ ] **Step 2: Lint the YAML**

```bash
yq '.jobs | keys' .github/workflows/ci.yml
```

Expected: list contains `coherence-gate`. If `yq` is unavailable, eyeball the file for indentation correctness.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: add coherence-gate job (issue #137)

New required check. PR + main → --gate beta; release/* → --gate rc.
Uploads coherence-trend.jsonl as an artifact for post-run analysis.

Refs #137"
```

---

## Task 15: Documentation updates

**Files:**
- Modify: `docs/ci.md`
- Modify: `CLAUDE.md`
- Modify: `docs/design/traceability.md`

- [ ] **Step 1: Add a paragraph to `docs/ci.md` under the bench/gates section**

Find the line in `docs/ci.md` that reads:

```
| `gates / latency + memory + privacy` (`ci.yml`) | ✅ required | Brief §15 SL...
```

Immediately below it, add a new row:

```
| `gates / coherence (§15, §18 #5, #137)` (`ci.yml`) | ✅ required | Brief §15 multi-session coherence gate. Five-metric scoring of extended replay cassettes against `crates/cairn-bench/manifests/coherence.toml`; per-metric floor + 2 % regression delta from `crates/cairn-bench/baselines/coherence.json`. PR + main → `--gate beta`; `release/*` → `--gate rc`. Exit 69 on regression. |
```

Then find the verification-checklist code block (around line 80) that lists:

```
# gates — latency SLO + 2% regression, memory budget, privacy leakage fixtures
cargo run -p cairn-bench --release --locked -- all
```

Add a new line below `all`:

```
# gates — coherence (5 metrics × 3 extended cassettes, §15, #137)
cargo run -p cairn-bench --release --locked -- coherence run --gate beta
```

- [ ] **Step 2: Add the verification line to `CLAUDE.md` §8**

In `CLAUDE.md`, find the line:

```
cargo run -p cairn-bench --release --locked -- all
```

Insert below it:

```
cargo run -p cairn-bench --release --locked -- coherence run --gate beta
```

- [ ] **Step 3: Reference the design doc in `docs/design/traceability.md`**

In `docs/design/traceability.md`, the §15 row currently reads:

```
| §15 Evaluation | #18, #24, #31, #97–#100, #116–#118, #136–#137 | #138 (docs freeze) | P0 replay and gates, P1 bench and SRE, v0.4 cassette and doc freeze. |
```

Update the Decisions/docs cell to also reference the new design doc:

```
| §15 Evaluation | #18, #24, #31, #97–#100, #116–#118, #136–#137 | #138 (docs freeze), `docs/design/2026-05-24-coherence-benchmarks-design.md` | P0 replay and gates, P1 bench and SRE, v0.4 cassette and doc freeze, v0.4 coherence gate (#137). |
```

- [ ] **Step 4: Commit**

```bash
git add docs/ci.md CLAUDE.md docs/design/traceability.md
git commit -m "docs: document coherence-gate CI job and verification step

Adds the new gate to docs/ci.md, the verification checklist in
CLAUDE.md §8, and links the design doc from the traceability matrix.

Refs #137"
```

---

## Task 16: Seed real baseline + final verification

**Files:**
- Modify: `crates/cairn-bench/baselines/coherence.json`
- Modify: `crates/cairn-bench/baselines/coherence-trend.jsonl` (truncate)

Replace the placeholder baseline with real numbers from a local run, then run the full verification checklist.

- [ ] **Step 1: Truncate the trend file**

The CI run will produce the first real trend line. Truncate the empty file so the first real run is clean:

```bash
: > crates/cairn-bench/baselines/coherence-trend.jsonl
```

- [ ] **Step 2: Generate the real baseline**

```bash
cargo build --release --locked -p cairn-bench
./target/release/cairn-bench coherence run --gate none --update-baseline --no-trend-write
```

Expected output: human table with real scores from all 21 actions across the 3 cassettes. `crates/cairn-bench/baselines/coherence.json` is rewritten.

- [ ] **Step 3: Inspect the new baseline**

```bash
cat crates/cairn-bench/baselines/coherence.json
```

Verify:
- `schema_version: 1`
- Each metric has a real `score`, `passed`, `total`.
- `forget_completeness.score == 1.0` and `total == 3`.

If `forget_completeness < 1.0`: there is a real bug — investigate before continuing. Do not commit a baseline that lets the privacy gate slip below 1.0.

If any metric scores significantly lower than the manifest floor: there is either a fixture bug or the floor is too aggressive. The conservative move is to leave the floor in the manifest and treat the failing metric as a real regression to fix before merge.

- [ ] **Step 4: Re-run with the real baseline under `--gate beta`**

```bash
./target/release/cairn-bench coherence run --gate beta
```

Expected: PASS, exits 0, appends one line to `coherence-trend.jsonl`.

- [ ] **Step 5: Re-run with `--gate rc`**

```bash
./target/release/cairn-bench coherence run --gate rc
```

Expected: PASS, exits 0, appends another line.

If `rc` fails because a real score is just below `rc_min`: this is fine pre-merge for placeholder thresholds. Open a follow-up issue to tighten `rc_min` after several runs land in the trend file.

- [ ] **Step 6: Truncate the trend file again before committing**

The trend file is meant to be empty in the repo — CI fills it. Strip the two lines we just appended:

```bash
: > crates/cairn-bench/baselines/coherence-trend.jsonl
```

- [ ] **Step 7: Run the full verification checklist (CLAUDE.md §8)**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
cargo run -p cairn-bench --release --locked -- all
cargo run -p cairn-bench --release --locked -- coherence run --gate beta
```

Expected: every command exits 0. If any fails, investigate the specific failure — do not skip a check.

- [ ] **Step 8: Commit the seeded baseline**

```bash
git add crates/cairn-bench/baselines/coherence.json crates/cairn-bench/baselines/coherence-trend.jsonl
git commit -m "feat(bench): seed real coherence baseline from extended cassettes

Replaces the floor-equal placeholder with real scores from the three
extended cassettes (#136). Trend file is left empty; CI fills it on
first run.

Refs #137"
```

- [ ] **Step 9: Open the PR**

```bash
gh pr create --title "feat: coherence benchmarks + release gate (#137)" --body "$(cat <<'EOF'
## Summary
- Add the coherence release gate that scores the three extended replay
  cassettes (#136) along five metrics (recall_precision,
  stale_avoidance, summary_quality, search_usefulness,
  forget_completeness), with per-metric beta/rc floors plus a 2 %
  regression delta vs. the committed baseline.
- New `cairn-bench coherence run` subcommand + CI job `coherence-gate`
  (required on `main`, runs `--gate rc` on `release/*`).
- Versioned JSONL trend with per-line `schema_version` migrator so
  benchmark schema changes don't break historical data.
- Forget-completeness is bound at score 1.0 with 0 % drop tolerance,
  binding the gate to brief §18 #4 (Privacy).

Design doc: `docs/design/2026-05-24-coherence-benchmarks-design.md`
Plan: `docs/superpowers/plans/2026-05-24-coherence-benchmarks.md`

## Test plan
- [x] `cargo nextest run --workspace --locked --no-fail-fast`
- [x] `cargo run -p cairn-bench --release --locked -- coherence run --gate beta`
- [x] `cargo run -p cairn-bench --release --locked -- coherence run --gate rc`
- [x] Live manifest, baseline, and trend line all validate against the
  JSON schemas in `crates/cairn-bench/schemas/`.
- [x] Full CLAUDE.md §8 verification checklist passes locally.

Refs #137, depends on #136 (closed).
EOF
)"
```

---

## Self-review checklist

Before opening the PR (or after, in another commit), tick each item:

- [ ] **Spec coverage:** Every section of the spec maps to a task:
  - §4 architecture → Tasks 4–9
  - §5 metric definitions → Tasks 1, 2, 3, 5
  - §6 thresholds → Tasks 6, 11
  - §7 trend persistence → Task 7
  - §8 CLI surface → Task 12
  - §9 CI wiring → Task 14
  - §10 testing → unit tests in Tasks 1, 2, 5, 6, 7, 8 + integration in Task 13
  - §14 traceability → Task 15
- [ ] **No placeholders left in code:** the only placeholder is the baseline JSON, which Task 16 rewrites with real numbers before commit.
- [ ] **Type consistency:** `MetricCategory` originates in `cairn-test-fixtures::replay` and is re-exported from `cairn-bench::coherence::category` throughout; `ReplayCheckReport.actual` is read in both Task 5 (`disjoint_from_stale`) and Task 13 (smoke test schema validation).
- [ ] **No half-finished implementations:** each task ends with a commit that leaves the workspace in a buildable, test-passing state. Task 11 commits a placeholder baseline that Task 16 rewrites; in between, the orchestrator test from Task 9 may fail if the placeholder doesn't match real scores — that risk is called out at the end of Task 11 Step 4.
- [ ] **Frequent commits:** sixteen commits total, one per task, each scoped to one logical change.
