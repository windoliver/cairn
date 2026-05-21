# Issue #90 — Rolling-summary ConsolidationWorkflow Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the P0 rolling-summary `ConsolidationWorkflow` plus the missing tokio scheduler loop that #89 deferred, so long sessions emit `reasoning`-kind summaries off the request path, hot memory loads meaning instead of raw turns, and forgets propagate to summaries.

**Architecture:** Add a tokio scheduler in `cairn-workflows` over the existing `SqliteJobStore`: a `Scheduler` struct holds a `HandlerRegistry` of `JobHandler` trait objects, spawns worker/reaper tasks via `TaskTracker`, drives lease/heartbeat/complete/fail, and shuts down cleanly via `CancellationToken`. Add a pure rolling-summary function in `cairn-core::pipeline::consolidation` (windowing, salience floor, token budget, source-id linkage) and a `ConsolidationHandler` in `cairn-workflows::consolidation` that reads trace records for a session, calls the pure function, and upserts a `reasoning`/`episodic`/`session` record through the existing WAL `Upsert` step graph. Forget propagation runs as a separate `ConsolidationForgetCleanupHandler` enqueued on `forget --record` that tombstones summaries pointing at the forgotten source. Capability flips behind `CONSOLIDATION_WORKFLOW_WIRED` in `status::wiring`, so CLI/MCP/SDK/skill all advertise consistently.

**Tech Stack:** Rust 1.95, tokio 1.x (`task::spawn_blocking`, `TaskTracker`, `CancellationToken`, `tokio::select!`), `async_trait`, `serde_json` for opaque payload encoding, `thiserror` for error enums, `tracing` for spans, `rstest` + `proptest` + `tempfile` for tests, `cargo nextest run`.

**Scope contract:**

- **In scope:** scheduler loop (workers, reaper, heartbeat, clock injection, graceful shutdown); `ConsolidationHandler` + pure consolidator; trace-window read helper on `SqliteMemoryStore`; enqueue trigger on turn-summary writes; forget cleanup handler; capability flag + status row + remediation; assemble_hot slot for rolling summaries; integration tests covering the three acceptance criteria.
- **Out of scope:** REM/Deep dream tiers, agent dream worker, P1 `FolderSummaryWorkflow`, Temporal adapter, LLM-backed body generation (the consolidator emits a deterministic placeholder body when no LLM is wired — full LLM authoring is a follow-up; brief §1 explicitly allows `consolidation_deferred` skip with no LLM).

**Commit cadence:** one commit per task. Each task ends with a `git commit` step using imperative subject. PR opens after Task 18.

---

## File Structure

**Created**

| File | Responsibility |
|---|---|
| `crates/cairn-core/src/pipeline/consolidation/mod.rs` | Module root, re-exports |
| `crates/cairn-core/src/pipeline/consolidation/window.rs` | Pure windowing: pick the next N turns to summarize |
| `crates/cairn-core/src/pipeline/consolidation/draft.rs` | `RollingSummaryDraft`, `compute_rolling_summary`, salience + token-budget logic |
| `crates/cairn-core/src/pipeline/consolidation/errors.rs` | `ConsolidationError` enum |
| `crates/cairn-core/src/config/consolidation.rs` | Typed `ConsolidationConfig` + defaults + YAML schema |
| `crates/cairn-workflows/src/scheduler/mod.rs` | `Scheduler` public type + start/stop |
| `crates/cairn-workflows/src/scheduler/handler.rs` | `JobHandler` trait, `HandlerRegistry`, `HandlerOutcome` |
| `crates/cairn-workflows/src/scheduler/worker.rs` | One worker task: lease → heartbeat-while-running → finish |
| `crates/cairn-workflows/src/scheduler/reaper.rs` | Background reap loop |
| `crates/cairn-workflows/src/scheduler/clock.rs` | `Clock` trait, `SystemClock`, `MockClock` (test helper) |
| `crates/cairn-workflows/src/consolidation/mod.rs` | Module root |
| `crates/cairn-workflows/src/consolidation/payload.rs` | `ConsolidationPayload` (serde JSON over `JobPayload`) |
| `crates/cairn-workflows/src/consolidation/handler.rs` | `ConsolidationHandler` impl of `JobHandler` |
| `crates/cairn-workflows/src/consolidation/trigger.rs` | `enqueue_if_due` helper called from capture_trace |
| `crates/cairn-workflows/src/consolidation/forget_cleanup.rs` | `ConsolidationForgetCleanupHandler` impl |
| `crates/cairn-store-sqlite/src/trace_window.rs` | `list_trace_turns(session_id, since_sequence, limit)` adapter helper |
| `crates/cairn-workflows/tests/scheduler_smoke.rs` | Worker pool + reaper lifecycle |
| `crates/cairn-workflows/tests/rolling_summary.rs` | End-to-end summary emission |
| `crates/cairn-workflows/tests/long_session_budget.rs` | Token-budget cap holds across many turns |
| `crates/cairn-workflows/tests/forget_propagation.rs` | Forget a turn → summary tombstoned |

**Modified**

| File | Change |
|---|---|
| `crates/cairn-core/src/pipeline/mod.rs` | `pub mod consolidation;` |
| `crates/cairn-core/src/config/mod.rs` | `pub mod consolidation; pub use consolidation::ConsolidationConfig;` and embed in root config |
| `crates/cairn-core/src/status/wiring.rs` | Add `CONSOLIDATION_WORKFLOW_WIRED` const |
| `crates/cairn-core/src/status/mod.rs` | Advertise `cairn.workflows.v1.consolidation` row |
| `crates/cairn-core/src/status/remediation.rs` | Hint for `consolidation.unavailable` |
| `crates/cairn-core/src/domain/record.rs` | Helper for `consolidation.source_record_ids` frontmatter access (read-only) |
| `crates/cairn-core/src/verbs/assemble_hot/inputs.rs` | Add `rolling_summary_candidates: &'a [&'a MemoryRecord]` slot |
| `crates/cairn-core/src/verbs/assemble_hot/segments.rs` | Render rolling-summary segment |
| `crates/cairn-core/src/verbs/assemble_hot/sources/` | Add `rolling_summary.rs` source |
| `crates/cairn-workflows/src/lib.rs` | Re-export scheduler + consolidation; flip `WorkflowOrchestratorCapabilities::durable = true, crash_safe = true` once scheduler lands |
| `crates/cairn-cli/src/main.rs` (or `lib.rs`) | Start `Scheduler` for long-running commands (`mcp serve`); shutdown on signal |
| `crates/cairn-cli/src/verbs/capture_trace.rs` | Call `enqueue_if_due` after turn_summary write |
| `crates/cairn-cli/src/verbs/forget.rs` | Enqueue `ConsolidationForgetCleanupHandler` after forget commits |

**Touched-but-no-logic-change**

| File | Change |
|---|---|
| `crates/cairn-workflows/Cargo.toml` | Add `serde_json`, `tracing` if missing |
| `Cargo.toml` (workspace) | Bump nothing new — `tokio_util` already present |

No SQL migrations: `workflow_jobs` exists in 0020; trace generated columns exist in 0023.

---

## Task 1: Add `ConsolidationConfig` + defaults

**Files:**
- Create: `crates/cairn-core/src/config/consolidation.rs`
- Modify: `crates/cairn-core/src/config/mod.rs:<add module export>`
- Test: `crates/cairn-core/src/config/consolidation.rs` (inline `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write failing test**

Append to `crates/cairn-core/src/config/consolidation.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_brief_p0() {
        let cfg = ConsolidationConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.window_size_turns, 8);
        assert_eq!(cfg.token_budget, 512);
        assert!((cfg.salience_floor - 0.4).abs() < f32::EPSILON);
        assert_eq!(cfg.min_turns_for_trigger, 4);
    }

    #[test]
    fn rejects_zero_window() {
        let cfg = ConsolidationConfig { window_size_turns: 0, ..ConsolidationConfig::default() };
        assert!(matches!(cfg.validate(), Err(ConsolidationConfigError::ZeroWindow)));
    }

    #[test]
    fn rejects_budget_below_floor() {
        let cfg = ConsolidationConfig { token_budget: 31, ..ConsolidationConfig::default() };
        assert!(matches!(cfg.validate(), Err(ConsolidationConfigError::BudgetTooLow { .. })));
    }

    #[test]
    fn salience_floor_out_of_range_rejected() {
        let cfg = ConsolidationConfig { salience_floor: 1.5, ..ConsolidationConfig::default() };
        assert!(matches!(cfg.validate(), Err(ConsolidationConfigError::SalienceOutOfRange { .. })));
    }
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test -p cairn-core --lib config::consolidation -- --nocapture`
Expected: build failure — `ConsolidationConfig` not defined.

- [ ] **Step 3: Implement config struct**

Write `crates/cairn-core/src/config/consolidation.rs`:

```rust
//! Rolling-summary `ConsolidationWorkflow` configuration (brief §5.3, §10.0).
//!
//! All knobs are P0 defaults — they may be overridden per-vault via
//! `.cairn/config.yaml` or per-folder via `_policy.yaml`.

use serde::{Deserialize, Serialize};

/// Typed configuration for the rolling-summary `ConsolidationWorkflow`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsolidationConfig {
    /// Master switch. When `false` the trigger never enqueues and the
    /// status capability advertises `consolidation_deferred`.
    #[serde(default = "defaults::enabled")]
    pub enabled: bool,

    /// Number of consecutive turns covered by one summary record.
    #[serde(default = "defaults::window_size_turns")]
    pub window_size_turns: u32,

    /// Minimum turns since the previous summary before another job is
    /// eligible. Keeps the trigger from firing every turn on a chatty
    /// session.
    #[serde(default = "defaults::min_turns_for_trigger")]
    pub min_turns_for_trigger: u32,

    /// Approximate hard cap on summary body length, in tokens. The
    /// consolidator truncates / re-summarizes any window whose draft
    /// exceeds this.
    #[serde(default = "defaults::token_budget")]
    pub token_budget: u32,

    /// Drop turns from the window whose computed salience falls below
    /// this floor. Range `[0.0, 1.0]`.
    #[serde(default = "defaults::salience_floor")]
    pub salience_floor: f32,
}

impl Default for ConsolidationConfig {
    fn default() -> Self {
        Self {
            enabled: defaults::enabled(),
            window_size_turns: defaults::window_size_turns(),
            min_turns_for_trigger: defaults::min_turns_for_trigger(),
            token_budget: defaults::token_budget(),
            salience_floor: defaults::salience_floor(),
        }
    }
}

mod defaults {
    pub const fn enabled() -> bool { true }
    pub const fn window_size_turns() -> u32 { 8 }
    pub const fn min_turns_for_trigger() -> u32 { 4 }
    pub const fn token_budget() -> u32 { 512 }
    pub const fn salience_floor() -> f32 { 0.4 }
}

/// Validation errors raised by [`ConsolidationConfig::validate`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConsolidationConfigError {
    /// `window_size_turns` was zero.
    #[error("consolidation.window_size_turns must be ≥ 1")]
    ZeroWindow,
    /// `token_budget` below the workable floor (32 tokens).
    #[error("consolidation.token_budget {actual} < required floor {floor}")]
    BudgetTooLow {
        /// Provided value.
        actual: u32,
        /// Required minimum.
        floor: u32,
    },
    /// `salience_floor` outside `[0, 1]`.
    #[error("consolidation.salience_floor {actual} outside [0, 1]")]
    SalienceOutOfRange {
        /// Provided value.
        actual: f32,
    },
}

impl ConsolidationConfig {
    /// Lowest-acceptable `token_budget`. Below this the summary cannot
    /// carry meaningful source-id linkage.
    pub const TOKEN_BUDGET_FLOOR: u32 = 32;

    /// Validate semantic invariants the serde layer can't express.
    ///
    /// # Errors
    /// Returns the first violated invariant.
    pub fn validate(&self) -> Result<(), ConsolidationConfigError> {
        if self.window_size_turns == 0 {
            return Err(ConsolidationConfigError::ZeroWindow);
        }
        if self.token_budget < Self::TOKEN_BUDGET_FLOOR {
            return Err(ConsolidationConfigError::BudgetTooLow {
                actual: self.token_budget,
                floor: Self::TOKEN_BUDGET_FLOOR,
            });
        }
        if !(0.0..=1.0).contains(&self.salience_floor) {
            return Err(ConsolidationConfigError::SalienceOutOfRange {
                actual: self.salience_floor,
            });
        }
        Ok(())
    }
}
```

Add to `crates/cairn-core/src/config/mod.rs`:

```rust
pub mod consolidation;
pub use consolidation::{ConsolidationConfig, ConsolidationConfigError};
```

Wire the field into whichever struct represents the root vault config (search the file for an existing `pub struct VaultConfig` or equivalent and add `pub consolidation: ConsolidationConfig` with `#[serde(default)]`). If the field already exists do not redeclare.

- [ ] **Step 4: Run tests**

Run: `cargo test -p cairn-core --lib config::consolidation`
Expected: 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/config/consolidation.rs crates/cairn-core/src/config/mod.rs
git commit -m "feat(config): add ConsolidationConfig with P0 defaults (brief §5.3)"
```

---

## Task 2: Pure windowing function

**Files:**
- Create: `crates/cairn-core/src/pipeline/consolidation/mod.rs`
- Create: `crates/cairn-core/src/pipeline/consolidation/window.rs`
- Modify: `crates/cairn-core/src/pipeline/mod.rs`
- Test: inline

- [ ] **Step 1: Failing test**

Write `crates/cairn-core/src/pipeline/consolidation/window.rs`:

```rust
//! Pure windowing: pick the next N turns to summarize from a session's
//! trace stream. No I/O — input is a sorted slice of trace headers.

use serde::{Deserialize, Serialize};

/// Lightweight header for one trace turn record. The handler builds these
/// from `MemoryRecord` headers; tests construct them directly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnHeader {
    /// `record_id` of the `turn_summary` record.
    pub record_id: String,
    /// Session identifier.
    pub session_id: String,
    /// Stable turn id (`trace.turn_id`).
    pub turn_id: String,
    /// Monotonic ordering within the session.
    pub sequence: u32,
    /// Estimated token count of the turn body. The handler approximates
    /// with `body.chars().len() / 4`.
    pub approx_tokens: u32,
    /// Computed salience for ranking; `1.0` for explicit user "remember"
    /// triggers, `0.5` baseline, lower for noise.
    pub salience: f32,
}

/// Result of [`pick_window`].
#[derive(Debug, Clone, PartialEq)]
pub struct WindowSelection {
    /// Selected turns, in ascending sequence order.
    pub turns: Vec<TurnHeader>,
    /// Sequence number of the last turn covered, for next-watermark math.
    pub last_sequence: u32,
}

/// Choose the next window to summarize.
///
/// `since_sequence` is the highest `sequence` already covered by a prior
/// summary (0 means "no prior summary"). The function returns at most
/// `window_size` turns whose salience clears `salience_floor`. Returns
/// `None` when fewer than `min_for_trigger` eligible turns are
/// available.
#[must_use]
pub fn pick_window(
    candidates: &[TurnHeader],
    since_sequence: u32,
    window_size: u32,
    min_for_trigger: u32,
    salience_floor: f32,
) -> Option<WindowSelection> {
    let mut filtered: Vec<TurnHeader> = candidates
        .iter()
        .filter(|t| t.sequence > since_sequence && t.salience >= salience_floor)
        .cloned()
        .collect();
    filtered.sort_by_key(|t| t.sequence);
    if (filtered.len() as u32) < min_for_trigger {
        return None;
    }
    let take = (window_size as usize).min(filtered.len());
    let turns = filtered.into_iter().take(take).collect::<Vec<_>>();
    let last_sequence = turns.last().map(|t| t.sequence)?;
    Some(WindowSelection { turns, last_sequence })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(seq: u32, sal: f32) -> TurnHeader {
        TurnHeader {
            record_id: format!("rec-{seq}"),
            session_id: "s1".into(),
            turn_id: format!("t-{seq}"),
            sequence: seq,
            approx_tokens: 40,
            salience: sal,
        }
    }

    #[test]
    fn picks_ascending_window() {
        let pool: Vec<_> = (1..=10).map(|s| turn(s, 0.7)).collect();
        let sel = pick_window(&pool, 0, 4, 2, 0.4).expect("eligible");
        assert_eq!(sel.turns.iter().map(|t| t.sequence).collect::<Vec<_>>(), vec![1, 2, 3, 4]);
        assert_eq!(sel.last_sequence, 4);
    }

    #[test]
    fn skips_below_floor() {
        let pool = vec![turn(1, 0.2), turn(2, 0.6), turn(3, 0.7), turn(4, 0.1), turn(5, 0.8)];
        let sel = pick_window(&pool, 0, 8, 2, 0.4).expect("eligible");
        assert_eq!(sel.turns.iter().map(|t| t.sequence).collect::<Vec<_>>(), vec![2, 3, 5]);
    }

    #[test]
    fn returns_none_below_min() {
        let pool = vec![turn(1, 0.9)];
        assert!(pick_window(&pool, 0, 8, 2, 0.4).is_none());
    }

    #[test]
    fn skips_already_covered() {
        let pool: Vec<_> = (1..=6).map(|s| turn(s, 0.7)).collect();
        let sel = pick_window(&pool, 3, 4, 2, 0.4).expect("eligible");
        assert_eq!(sel.turns.iter().map(|t| t.sequence).collect::<Vec<_>>(), vec![4, 5, 6]);
    }
}
```

Write `crates/cairn-core/src/pipeline/consolidation/mod.rs`:

```rust
//! Pure functions for the rolling-summary `ConsolidationWorkflow`
//! (brief §5.3 + §10.0). I/O happens in `cairn-workflows`; this module
//! is deterministic and contract-free.

pub mod draft;
pub mod errors;
pub mod window;

pub use draft::{compute_rolling_summary, RollingSummaryDraft, SummaryStatus};
pub use errors::ConsolidationError;
pub use window::{pick_window, TurnHeader, WindowSelection};
```

Add to `crates/cairn-core/src/pipeline/mod.rs`:

```rust
pub mod consolidation;
```

- [ ] **Step 2: Run failing test then fix**

Run: `cargo test -p cairn-core --lib pipeline::consolidation::window`
Expected: 4 tests pass once `draft`/`errors` stubs exist. Create them as empty for now:

`crates/cairn-core/src/pipeline/consolidation/draft.rs`:
```rust
//! Filled in Task 3.
pub struct RollingSummaryDraft;
pub enum SummaryStatus { Pending }
pub fn compute_rolling_summary() {}
```
`crates/cairn-core/src/pipeline/consolidation/errors.rs`:
```rust
//! Filled in Task 3.
#[derive(Debug)]
pub enum ConsolidationError {}
```

- [ ] **Step 3: Verify**

Run: `cargo test -p cairn-core --lib pipeline::consolidation::window`
Expected: 4 pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/pipeline/
git commit -m "feat(consolidation): pure windowing function (brief §5.3)"
```

---

## Task 3: `RollingSummaryDraft` + `compute_rolling_summary`

**Files:**
- Modify: `crates/cairn-core/src/pipeline/consolidation/draft.rs`
- Modify: `crates/cairn-core/src/pipeline/consolidation/errors.rs`

- [ ] **Step 1: Failing test**

Replace `crates/cairn-core/src/pipeline/consolidation/draft.rs`:

```rust
//! Pure `compute_rolling_summary` — turns a [`WindowSelection`] into a
//! [`RollingSummaryDraft`]. Deterministic; the body is a placeholder
//! that an LLM-backed implementation overrides in a follow-up. The
//! handler still produces a valid `reasoning` record without an LLM —
//! brief §1 says rolling summaries degrade to `consolidation_deferred`
//! only when explicitly disabled, otherwise the substrate keeps writing.

use super::errors::ConsolidationError;
use super::window::WindowSelection;
use crate::config::ConsolidationConfig;

/// Whether the consolidator produced a summary or deferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryStatus {
    /// Summary body was authored.
    Authored,
    /// `enabled = false` or no LLM and `prefer_llm` was set; the
    /// trigger should emit a `consolidation_deferred` lint entry
    /// rather than persist anything.
    Deferred,
}

/// Output of [`compute_rolling_summary`]. The handler converts this
/// into a `MemoryRecord` with `kind = reasoning, class = episodic,
/// scope.session_id = …`, and `extra_frontmatter.consolidation.source_record_ids`
/// pointing back at the window turns.
#[derive(Debug, Clone, PartialEq)]
pub struct RollingSummaryDraft {
    /// `Authored` or `Deferred`.
    pub status: SummaryStatus,
    /// Markdown body (empty when `Deferred`).
    pub body: String,
    /// `record_id`s of the turn records this summary covers.
    pub source_record_ids: Vec<String>,
    /// Highest `sequence` in the window — written into the summary
    /// frontmatter as `consolidation.last_sequence`.
    pub last_sequence: u32,
    /// Approximate token count of `body` (caller-provided cap).
    pub summary_tokens: u32,
}

/// Compute a rolling summary from a pre-selected window.
///
/// # Errors
/// - [`ConsolidationError::EmptyWindow`] when the window has no turns.
/// - [`ConsolidationError::BudgetExceeded`] when the deterministic
///   placeholder body cannot fit inside `config.token_budget` even after
///   truncation. (Should not happen in practice — the placeholder is
///   one short line per turn.)
pub fn compute_rolling_summary(
    window: &WindowSelection,
    config: &ConsolidationConfig,
) -> Result<RollingSummaryDraft, ConsolidationError> {
    if window.turns.is_empty() {
        return Err(ConsolidationError::EmptyWindow);
    }
    if !config.enabled {
        return Ok(RollingSummaryDraft {
            status: SummaryStatus::Deferred,
            body: String::new(),
            source_record_ids: window.turns.iter().map(|t| t.record_id.clone()).collect(),
            last_sequence: window.last_sequence,
            summary_tokens: 0,
        });
    }
    // Deterministic placeholder body: one bullet per turn, capped by
    // `token_budget` × 4 chars (rough char-per-token approximation).
    let max_chars = (config.token_budget as usize).saturating_mul(4);
    let mut body = String::new();
    body.push_str("Rolling summary of ");
    body.push_str(&window.turns.len().to_string());
    body.push_str(" turn(s):\n\n");
    for turn in &window.turns {
        body.push_str("- ");
        body.push_str(&turn.turn_id);
        body.push_str(" (seq=");
        body.push_str(&turn.sequence.to_string());
        body.push_str(", salience=");
        body.push_str(&format!("{:.2}", turn.salience));
        body.push_str(")\n");
        if body.len() > max_chars {
            body.truncate(max_chars);
            body.push_str("\n…");
            break;
        }
    }
    let summary_tokens = u32::try_from(body.len() / 4).unwrap_or(u32::MAX);
    Ok(RollingSummaryDraft {
        status: SummaryStatus::Authored,
        body,
        source_record_ids: window.turns.iter().map(|t| t.record_id.clone()).collect(),
        last_sequence: window.last_sequence,
        summary_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::consolidation::window::TurnHeader;

    fn turn(seq: u32) -> TurnHeader {
        TurnHeader {
            record_id: format!("rec-{seq}"),
            session_id: "s1".into(),
            turn_id: format!("t-{seq}"),
            sequence: seq,
            approx_tokens: 40,
            salience: 0.6,
        }
    }

    #[test]
    fn authored_body_includes_each_turn_id() {
        let win = WindowSelection { turns: vec![turn(1), turn(2), turn(3)], last_sequence: 3 };
        let draft = compute_rolling_summary(&win, &ConsolidationConfig::default()).unwrap();
        assert_eq!(draft.status, SummaryStatus::Authored);
        assert!(draft.body.contains("t-1") && draft.body.contains("t-2") && draft.body.contains("t-3"));
        assert_eq!(draft.source_record_ids, vec!["rec-1", "rec-2", "rec-3"]);
        assert_eq!(draft.last_sequence, 3);
    }

    #[test]
    fn deferred_when_disabled() {
        let win = WindowSelection { turns: vec![turn(1), turn(2)], last_sequence: 2 };
        let cfg = ConsolidationConfig { enabled: false, ..ConsolidationConfig::default() };
        let draft = compute_rolling_summary(&win, &cfg).unwrap();
        assert_eq!(draft.status, SummaryStatus::Deferred);
        assert!(draft.body.is_empty());
        assert_eq!(draft.source_record_ids.len(), 2);
    }

    #[test]
    fn empty_window_errors() {
        let win = WindowSelection { turns: vec![], last_sequence: 0 };
        assert!(matches!(
            compute_rolling_summary(&win, &ConsolidationConfig::default()),
            Err(ConsolidationError::EmptyWindow)
        ));
    }

    #[test]
    fn respects_token_budget_floor() {
        let many: Vec<_> = (1..=200).map(turn).collect();
        let win = WindowSelection { turns: many, last_sequence: 200 };
        let cfg = ConsolidationConfig { token_budget: 32, ..ConsolidationConfig::default() };
        let draft = compute_rolling_summary(&win, &cfg).unwrap();
        assert!(draft.body.len() <= 32 * 4 + 4); // +4 for truncation marker
        assert_eq!(draft.status, SummaryStatus::Authored);
    }
}
```

Replace `crates/cairn-core/src/pipeline/consolidation/errors.rs`:

```rust
//! Errors raised by the pure consolidation pipeline.

/// Failures from [`super::compute_rolling_summary`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConsolidationError {
    /// Caller supplied a window with no turns.
    #[error("consolidation: empty window")]
    EmptyWindow,
    /// Generated body could not fit inside the configured token budget.
    #[error("consolidation: body exceeds token_budget {budget}")]
    BudgetExceeded {
        /// The configured budget that was exceeded.
        budget: u32,
    },
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p cairn-core --lib pipeline::consolidation::draft`
Expected: 4 pass.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/src/pipeline/consolidation/draft.rs crates/cairn-core/src/pipeline/consolidation/errors.rs
git commit -m "feat(consolidation): RollingSummaryDraft + compute_rolling_summary"
```

---

## Task 4: `CONSOLIDATION_WORKFLOW_WIRED` capability flag

**Files:**
- Modify: `crates/cairn-core/src/status/wiring.rs`
- Modify: `crates/cairn-core/src/status/mod.rs` (advertise table)
- Modify: `crates/cairn-core/src/status/remediation.rs` (hint row)
- Test: `crates/cairn-core/src/status/tests.rs`

- [ ] **Step 1: Failing test**

Append to `crates/cairn-core/src/status/tests.rs`:

```rust
#[test]
fn consolidation_capability_hidden_until_wired() {
    let caps = advertise();
    let has_it = caps.iter().any(|c| c == "cairn.workflows.v1.consolidation");
    assert_eq!(has_it, wiring::CONSOLIDATION_WORKFLOW_WIRED,
        "advertise must mirror CONSOLIDATION_WORKFLOW_WIRED");
}

#[test]
fn consolidation_remediation_present() {
    let hint = REMEDIATION.iter()
        .find(|(code, _)| *code == "consolidation.unavailable");
    assert!(hint.is_some(), "remediation hint for consolidation must exist");
}
```

- [ ] **Step 2: Run failing**

Run: `cargo test -p cairn-core --lib status::tests::consolidation`
Expected: build fails — `CONSOLIDATION_WORKFLOW_WIRED` undefined.

- [ ] **Step 3: Implement**

Append to `crates/cairn-core/src/status/wiring.rs`:

```rust
/// Rolling-summary `ConsolidationWorkflow` dispatch path (issue #90).
///
/// Held off until the scheduler loop is wired into `cairn-cli`'s
/// long-running entry points and the trigger calls `enqueue_if_due`
/// from the capture_trace path. Flip in Task 17 of the #90 plan.
pub const CONSOLIDATION_WORKFLOW_WIRED: bool = false;
```

In `crates/cairn-core/src/status/mod.rs`, locate the `pub fn advertise()` function and add (preserving alphabetical order if such exists):

```rust
if wiring::CONSOLIDATION_WORKFLOW_WIRED {
    out.push("cairn.workflows.v1.consolidation".into());
}
```

In `crates/cairn-core/src/status/remediation.rs`, add to the `REMEDIATION` table:

```rust
("consolidation.unavailable",
 "Rolling-summary ConsolidationWorkflow is disabled or its scheduler is not running. \
  Check that `consolidation.enabled = true` in .cairn/config.yaml and that the \
  long-running `cairn mcp serve` host is up so the scheduler can lease jobs."),
```

- [ ] **Step 4: Verify**

Run: `cargo test -p cairn-core --lib status::tests::consolidation`
Expected: 2 pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/status/
git commit -m "feat(status): add CONSOLIDATION_WORKFLOW_WIRED gate (issue #90)"
```

---

## Task 5: `Clock` trait + `SystemClock` + `MockClock`

**Files:**
- Create: `crates/cairn-workflows/src/scheduler/clock.rs`
- Create: `crates/cairn-workflows/src/scheduler/mod.rs` (stub for now)
- Modify: `crates/cairn-workflows/src/lib.rs`

- [ ] **Step 1: Failing test**

Write `crates/cairn-workflows/src/scheduler/clock.rs`:

```rust
//! Clock injection for the scheduler. The `JobStore` contract takes
//! `now_ms: i64` on every call; the scheduler owns the canonical
//! source of time. Tests use [`MockClock`] to drive lease expiry.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Source of wall-clock milliseconds.
pub trait Clock: Send + Sync + 'static {
    /// Current wall-clock in epoch milliseconds.
    fn now_ms(&self) -> i64;
}

/// Production clock backed by `SystemTime`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
    }
}

/// Test clock with manually advanced time.
#[derive(Debug, Clone)]
pub struct MockClock(Arc<AtomicI64>);

impl MockClock {
    /// Start at `start_ms`.
    #[must_use]
    pub fn at(start_ms: i64) -> Self {
        Self(Arc::new(AtomicI64::new(start_ms)))
    }
    /// Advance by `delta_ms`.
    pub fn advance(&self, delta_ms: i64) {
        self.0.fetch_add(delta_ms, Ordering::SeqCst);
    }
}

impl Clock for MockClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_clock_advances() {
        let c = MockClock::at(1_000);
        assert_eq!(c.now_ms(), 1_000);
        c.advance(500);
        assert_eq!(c.now_ms(), 1_500);
    }

    #[test]
    fn system_clock_returns_monotonic_positive() {
        let c = SystemClock;
        let a = c.now_ms();
        let b = c.now_ms();
        assert!(a > 0 && b >= a);
    }
}
```

Write minimal `crates/cairn-workflows/src/scheduler/mod.rs`:

```rust
//! Tokio scheduler loop over [`cairn_core::contract::JobStore`].
//! Built incrementally across Tasks 5–9 of the #90 plan.

pub mod clock;

pub use clock::{Clock, MockClock, SystemClock};
```

Append to `crates/cairn-workflows/src/lib.rs`:

```rust
pub mod scheduler;

pub use scheduler::{Clock, MockClock, SystemClock};
```

- [ ] **Step 2: Verify**

Run: `cargo test -p cairn-workflows --lib scheduler::clock`
Expected: 2 pass.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-workflows/src/scheduler/ crates/cairn-workflows/src/lib.rs
git commit -m "feat(workflows): scheduler clock trait (system + mock)"
```

---

## Task 6: `JobHandler` trait + `HandlerRegistry`

**Files:**
- Create: `crates/cairn-workflows/src/scheduler/handler.rs`
- Modify: `crates/cairn-workflows/src/scheduler/mod.rs`

- [ ] **Step 1: Failing test**

Write `crates/cairn-workflows/src/scheduler/handler.rs`:

```rust
//! `JobHandler` trait + `HandlerRegistry`. The scheduler dispatches
//! leased jobs by [`cairn_core::contract::job_store::JobKind`] to one
//! registered handler.

use std::collections::HashMap;
use std::sync::Arc;

use cairn_core::contract::job_store::{JobKind, JobPayload};

/// Outcome a handler returns after running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandlerOutcome {
    /// Handler succeeded; scheduler calls `complete`.
    Done,
    /// Retryable failure; scheduler calls `fail(Retry)`.
    Retry {
        /// Error message persisted into `workflow_jobs.last_error`.
        reason: String,
    },
    /// Permanent failure; scheduler calls `fail(Permanent)`.
    Permanent {
        /// Error message persisted into `workflow_jobs.last_error`.
        reason: String,
    },
}

/// Errors from registry plumbing.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HandlerDispatchError {
    /// No handler registered for the leased job's kind.
    #[error("no handler registered for kind {0}")]
    Unknown(JobKind),
}

/// One workflow handler.
#[async_trait::async_trait]
pub trait JobHandler: Send + Sync + 'static {
    /// Stable kind discriminator. Matches `JobKind` on enqueue.
    fn kind(&self) -> JobKind;
    /// Run the handler with the opaque payload bytes.
    async fn handle(&self, payload: &JobPayload) -> HandlerOutcome;
}

/// Map of `JobKind → Arc<dyn JobHandler>`. Cheap to clone.
#[derive(Clone, Default)]
pub struct HandlerRegistry {
    handlers: Arc<HashMap<JobKind, Arc<dyn JobHandler>>>,
}

/// Builder for [`HandlerRegistry`].
#[derive(Default)]
pub struct HandlerRegistryBuilder {
    handlers: HashMap<JobKind, Arc<dyn JobHandler>>,
}

impl HandlerRegistryBuilder {
    /// Register a handler. Panics in debug if `kind` collides — kinds
    /// must be unique by construction; this is a programmer-error guard,
    /// not a runtime concern.
    #[must_use]
    pub fn with(mut self, handler: Arc<dyn JobHandler>) -> Self {
        let k = handler.kind();
        debug_assert!(!self.handlers.contains_key(&k),
            "duplicate handler for kind {k:?}");
        self.handlers.insert(k, handler);
        self
    }
    /// Freeze the builder into a shareable registry.
    #[must_use]
    pub fn build(self) -> HandlerRegistry {
        HandlerRegistry { handlers: Arc::new(self.handlers) }
    }
}

impl HandlerRegistry {
    /// Look up a handler.
    ///
    /// # Errors
    /// [`HandlerDispatchError::Unknown`] when no handler matches.
    pub fn lookup(&self, kind: &JobKind) -> Result<Arc<dyn JobHandler>, HandlerDispatchError> {
        self.handlers
            .get(kind)
            .cloned()
            .ok_or_else(|| HandlerDispatchError::Unknown(kind.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Noop;
    #[async_trait::async_trait]
    impl JobHandler for Noop {
        fn kind(&self) -> JobKind { JobKind::new("noop") }
        async fn handle(&self, _: &JobPayload) -> HandlerOutcome { HandlerOutcome::Done }
    }

    #[tokio::test]
    async fn registry_dispatches_by_kind() {
        let reg = HandlerRegistryBuilder::default()
            .with(Arc::new(Noop))
            .build();
        let h = reg.lookup(&JobKind::new("noop")).unwrap();
        assert_eq!(h.handle(&Vec::new()).await, HandlerOutcome::Done);
    }

    #[test]
    fn unknown_kind_errors() {
        let reg = HandlerRegistry::default();
        let err = reg.lookup(&JobKind::new("missing")).unwrap_err();
        assert!(matches!(err, HandlerDispatchError::Unknown(_)));
    }
}
```

Update `crates/cairn-workflows/src/scheduler/mod.rs`:

```rust
pub mod clock;
pub mod handler;

pub use clock::{Clock, MockClock, SystemClock};
pub use handler::{HandlerDispatchError, HandlerOutcome, HandlerRegistry, HandlerRegistryBuilder, JobHandler};
```

- [ ] **Step 2: Verify**

Run: `cargo test -p cairn-workflows --lib scheduler::handler`
Expected: 2 pass.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-workflows/src/scheduler/
git commit -m "feat(workflows): JobHandler trait + HandlerRegistry"
```

---

## Task 7: Worker task — lease, heartbeat, finish

**Files:**
- Create: `crates/cairn-workflows/src/scheduler/worker.rs`
- Modify: `crates/cairn-workflows/src/scheduler/mod.rs`

- [ ] **Step 1: Failing test**

Write `crates/cairn-workflows/src/scheduler/worker.rs`:

```rust
//! One worker task. The Scheduler spawns N of these. Each loops:
//!   1. Try to lease a job.
//!   2. If leased, fork a heartbeat task and run the handler.
//!   3. On handler return, complete/fail; cancel heartbeat.
//!   4. If no job leased, sleep `poll_interval` (or exit on cancel).

use std::sync::Arc;
use std::time::Duration;

use cairn_core::contract::job_store::{FailDisposition, JobStore, LeasedJob};
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, instrument, warn};

use super::clock::Clock;
use super::handler::{HandlerOutcome, HandlerRegistry};

/// Tunables for a worker loop.
#[derive(Debug, Clone, Copy)]
pub struct WorkerConfig {
    /// Lease duration handed to `JobStore::lease`.
    pub lease_ms: i64,
    /// Heartbeat extension cadence (`lease_ms / 3` is the rule of thumb).
    pub heartbeat_every_ms: i64,
    /// Sleep duration when no job is available.
    pub idle_poll_ms: u64,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self { lease_ms: 30_000, heartbeat_every_ms: 10_000, idle_poll_ms: 200 }
    }
}

/// Run one worker forever (until `cancel` fires).
#[instrument(skip(store, registry, clock, cancel), fields(owner = %owner))]
pub async fn run_worker(
    owner: String,
    store: Arc<dyn JobStore>,
    registry: HandlerRegistry,
    clock: Arc<dyn Clock>,
    cancel: CancellationToken,
    config: WorkerConfig,
) {
    loop {
        if cancel.is_cancelled() {
            debug!("worker cancelled");
            return;
        }
        let now = clock.now_ms();
        let leased = match store.lease(&owner, now, config.lease_ms).await {
            Ok(Some(job)) => job,
            Ok(None) => {
                tokio::select! {
                    _ = sleep(Duration::from_millis(config.idle_poll_ms)) => continue,
                    _ = cancel.cancelled() => return,
                }
            }
            Err(e) => {
                warn!(error = %e, "lease failed");
                sleep(Duration::from_millis(config.idle_poll_ms)).await;
                continue;
            }
        };
        execute_one(&store, &registry, &clock, &cancel, &leased, &config).await;
    }
}

async fn execute_one(
    store: &Arc<dyn JobStore>,
    registry: &HandlerRegistry,
    clock: &Arc<dyn Clock>,
    cancel: &CancellationToken,
    leased: &LeasedJob,
    config: &WorkerConfig,
) {
    let handler = match registry.lookup(&leased.kind) {
        Ok(h) => h,
        Err(e) => {
            warn!(error = %e, job = %leased.job_id, "no handler; permanent fail");
            let now = clock.now_ms();
            let _ = store.fail(&leased.job_id, &leased.lease, FailDisposition::Permanent, &e.to_string(), now).await;
            return;
        }
    };

    let hb_token = CancellationToken::new();
    let hb_handle = {
        let store = store.clone();
        let clock = clock.clone();
        let lease = leased.lease.clone();
        let job_id = leased.job_id.clone();
        let token = hb_token.clone();
        let interval_ms = u64::try_from(config.heartbeat_every_ms).unwrap_or(10_000);
        let lease_ms = config.lease_ms;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = token.cancelled() => return,
                    _ = sleep(Duration::from_millis(interval_ms)) => {
                        let now = clock.now_ms();
                        let new_expires = now.saturating_add(lease_ms);
                        if let Err(e) = store.heartbeat(&job_id, &lease, now, new_expires).await {
                            warn!(error = %e, job = %job_id, "heartbeat lost");
                            return;
                        }
                    }
                }
            }
        })
    };

    let outcome = tokio::select! {
        o = handler.handle(&leased.payload) => o,
        _ = cancel.cancelled() => HandlerOutcome::Retry { reason: "scheduler shutdown".into() },
    };
    hb_token.cancel();
    let _ = timeout(Duration::from_secs(1), hb_handle).await;

    let now = clock.now_ms();
    let result = match outcome {
        HandlerOutcome::Done => store.complete(&leased.job_id, &leased.lease, now).await,
        HandlerOutcome::Retry { reason } => {
            store.fail(&leased.job_id, &leased.lease, FailDisposition::Retry, &reason, now).await
        }
        HandlerOutcome::Permanent { reason } => {
            store.fail(&leased.job_id, &leased.lease, FailDisposition::Permanent, &reason, now).await
        }
    };
    if let Err(e) = result {
        error!(error = %e, job = %leased.job_id, "worker finalize failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::{HandlerRegistryBuilder, JobHandler, MockClock};
    use cairn_core::contract::job_store::{EnqueueRequest, JobId, JobKind, JobPayload, RetryPolicy};
    use crate::SqliteJobStore;
    use rusqlite::Connection;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counter(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl JobHandler for Counter {
        fn kind(&self) -> JobKind { JobKind::new("counter") }
        async fn handle(&self, _: &JobPayload) -> HandlerOutcome {
            self.0.fetch_add(1, Ordering::SeqCst);
            HandlerOutcome::Done
        }
    }

    fn mem_store() -> Arc<SqliteJobStore> {
        let conn = Connection::open_in_memory().expect("conn");
        // Bootstrap migration 0020 — production migration manifest applies in
        // the SqliteMemoryStore; for a workflow-only test, run the SQL inline:
        crate::sqlite_store::install_for_tests(&conn);
        Arc::new(SqliteJobStore::new(conn).expect("store"))
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn worker_runs_handler_once_and_completes() {
        let counter = Arc::new(AtomicUsize::new(0));
        let registry = HandlerRegistryBuilder::default()
            .with(Arc::new(Counter(counter.clone())))
            .build();
        let store = mem_store() as Arc<dyn JobStore>;
        let clock = Arc::new(MockClock::at(1_000)) as Arc<dyn Clock>;
        let cancel = CancellationToken::new();

        store.enqueue(EnqueueRequest {
            job_id: JobId::new("j-1"),
            kind: JobKind::new("counter"),
            payload: vec![],
            queue_key: None,
            dedupe_key: None,
            not_before_ms: 0,
            retry: RetryPolicy::DEFAULT,
        }).await.unwrap();

        let token = cancel.clone();
        let handle = tokio::spawn(run_worker("w-1".into(), store.clone(), registry, clock.clone(), token, WorkerConfig::default()));
        tokio::time::advance(Duration::from_millis(50)).await;
        // Give the worker a single tick.
        tokio::task::yield_now().await;
        cancel.cancel();
        let _ = timeout(Duration::from_secs(2), handle).await;
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}
```

This test relies on a small `crate::sqlite_store::install_for_tests` helper. Add it to `crates/cairn-workflows/src/sqlite_store.rs` (look for an existing `#[cfg(test)]` block; if none, append):

```rust
#[cfg(test)]
pub(crate) fn install_for_tests(conn: &rusqlite::Connection) {
    conn.execute_batch(MIGRATION_0020_SQL).expect("apply 0020");
}
```

Use the existing `MIGRATION_0020_SQL` constant.

- [ ] **Step 2: Add `tokio-util` if missing**

Check `crates/cairn-workflows/Cargo.toml` — `tokio-util = { workspace = true, features = ["rt"] }` is needed for `TaskTracker` and `CancellationToken`. Add if absent.

- [ ] **Step 3: Update mod.rs**

```rust
pub mod clock;
pub mod handler;
pub mod worker;

pub use clock::{Clock, MockClock, SystemClock};
pub use handler::{HandlerDispatchError, HandlerOutcome, HandlerRegistry, HandlerRegistryBuilder, JobHandler};
pub use worker::{run_worker, WorkerConfig};
```

- [ ] **Step 4: Run**

Run: `cargo test -p cairn-workflows --lib scheduler::worker -- --test-threads=1`
Expected: 1 pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-workflows/src/scheduler/ crates/cairn-workflows/src/sqlite_store.rs crates/cairn-workflows/Cargo.toml
git commit -m "feat(workflows): worker loop with heartbeat (issue #90)"
```

---

## Task 8: Reaper task

**Files:**
- Create: `crates/cairn-workflows/src/scheduler/reaper.rs`
- Modify: `crates/cairn-workflows/src/scheduler/mod.rs`

- [ ] **Step 1: Failing test**

Write `crates/cairn-workflows/src/scheduler/reaper.rs`:

```rust
//! Periodic reaper. Calls `JobStore::reap_expired` so leases whose
//! workers crashed are reclaimed.

use std::sync::Arc;
use std::time::Duration;

use cairn_core::contract::job_store::JobStore;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{debug, instrument, warn};

use super::clock::Clock;

/// Reaper tunables.
#[derive(Debug, Clone, Copy)]
pub struct ReaperConfig {
    /// Wall time between scans, milliseconds.
    pub interval_ms: u64,
}

impl Default for ReaperConfig {
    fn default() -> Self { Self { interval_ms: 5_000 } }
}

/// Run the reaper forever (until `cancel` fires).
#[instrument(skip(store, clock, cancel))]
pub async fn run_reaper(
    store: Arc<dyn JobStore>,
    clock: Arc<dyn Clock>,
    cancel: CancellationToken,
    config: ReaperConfig,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => { debug!("reaper cancelled"); return; }
            _ = sleep(Duration::from_millis(config.interval_ms)) => {
                let now = clock.now_ms();
                match store.reap_expired(now).await {
                    Ok(n) if n > 0 => debug!(reclaimed = n, "reaper reclaimed orphan leases"),
                    Ok(_) => {},
                    Err(e) => warn!(error = %e, "reap failed"),
                }
            }
        }
    }
}
```

Add a smoke test inline:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::MockClock;
    use crate::SqliteJobStore;
    use rusqlite::Connection;

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn reaper_ticks() {
        let conn = Connection::open_in_memory().unwrap();
        crate::sqlite_store::install_for_tests(&conn);
        let store: Arc<dyn JobStore> = Arc::new(SqliteJobStore::new(conn).unwrap());
        let clock: Arc<dyn Clock> = Arc::new(MockClock::at(1_000));
        let cancel = CancellationToken::new();
        let handle = tokio::spawn(run_reaper(store, clock, cancel.clone(), ReaperConfig { interval_ms: 10 }));
        tokio::time::advance(Duration::from_millis(50)).await;
        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(1), handle).await;
    }
}
```

- [ ] **Step 2: Update mod.rs**

```rust
pub mod reaper;
pub use reaper::{run_reaper, ReaperConfig};
```

- [ ] **Step 3: Run**

Run: `cargo test -p cairn-workflows --lib scheduler::reaper`
Expected: 1 pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-workflows/src/scheduler/
git commit -m "feat(workflows): scheduler reaper loop"
```

---

## Task 9: `Scheduler` public API — start/stop

**Files:**
- Create top-level struct in `crates/cairn-workflows/src/scheduler/mod.rs`

- [ ] **Step 1: Failing test**

Update `crates/cairn-workflows/src/scheduler/mod.rs`:

```rust
//! Tokio scheduler loop over [`cairn_core::contract::JobStore`].

pub mod clock;
pub mod handler;
pub mod reaper;
pub mod worker;

pub use clock::{Clock, MockClock, SystemClock};
pub use handler::{HandlerDispatchError, HandlerOutcome, HandlerRegistry, HandlerRegistryBuilder, JobHandler};
pub use reaper::{run_reaper, ReaperConfig};
pub use worker::{run_worker, WorkerConfig};

use std::sync::Arc;
use cairn_core::contract::job_store::JobStore;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// Bundle of all scheduler tunables.
#[derive(Debug, Clone, Copy, Default)]
pub struct SchedulerConfig {
    /// Worker tunables (shared across workers).
    pub worker: WorkerConfig,
    /// Reaper tunables.
    pub reaper: ReaperConfig,
    /// How many concurrent workers to spawn.
    pub worker_count: u32,
}

impl SchedulerConfig {
    /// P0 default — 2 workers, 30s leases, 5s reap interval.
    #[must_use]
    pub const fn p0() -> Self {
        Self {
            worker: WorkerConfig { lease_ms: 30_000, heartbeat_every_ms: 10_000, idle_poll_ms: 200 },
            reaper: ReaperConfig { interval_ms: 5_000 },
            worker_count: 2,
        }
    }
}

/// Running scheduler handle. Drop or call [`Self::shutdown`] to stop.
pub struct Scheduler {
    cancel: CancellationToken,
    tracker: TaskTracker,
}

impl Scheduler {
    /// Spawn N workers + 1 reaper and return a handle.
    #[must_use]
    pub fn start(
        incarnation_id: &str,
        store: Arc<dyn JobStore>,
        registry: HandlerRegistry,
        clock: Arc<dyn Clock>,
        config: SchedulerConfig,
    ) -> Self {
        let cancel = CancellationToken::new();
        let tracker = TaskTracker::new();
        for i in 0..config.worker_count.max(1) {
            let owner = format!("{incarnation_id}:w{i}");
            let t = cancel.clone();
            let s = store.clone();
            let r = registry.clone();
            let c = clock.clone();
            tracker.spawn(worker::run_worker(owner, s, r, c, t, config.worker));
        }
        let t = cancel.clone();
        tracker.spawn(reaper::run_reaper(store, clock, t, config.reaper));
        tracker.close();
        Self { cancel, tracker }
    }

    /// Cancel all tasks and await them.
    pub async fn shutdown(self) {
        self.cancel.cancel();
        self.tracker.wait().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite_store::install_for_tests;
    use crate::SqliteJobStore;
    use rusqlite::Connection;

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn start_and_shutdown_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        install_for_tests(&conn);
        let store: Arc<dyn JobStore> = Arc::new(SqliteJobStore::new(conn).unwrap());
        let registry = HandlerRegistry::default();
        let clock: Arc<dyn Clock> = Arc::new(MockClock::at(1_000));
        let s = Scheduler::start("inc-1", store, registry, clock, SchedulerConfig::p0());
        tokio::time::advance(std::time::Duration::from_millis(50)).await;
        s.shutdown().await;
    }
}
```

- [ ] **Step 2: Run**

Run: `cargo test -p cairn-workflows --lib scheduler::tests`
Expected: pass.

- [ ] **Step 3: Update lib.rs exports**

In `crates/cairn-workflows/src/lib.rs`:

```rust
pub use scheduler::{Scheduler, SchedulerConfig};
```

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-workflows/src/scheduler/mod.rs crates/cairn-workflows/src/lib.rs
git commit -m "feat(workflows): Scheduler public API (start/shutdown)"
```

---

## Task 10: Trace-window adapter helper on `SqliteMemoryStore`

**Files:**
- Create: `crates/cairn-store-sqlite/src/trace_window.rs`
- Modify: `crates/cairn-store-sqlite/src/lib.rs` (re-export or impl block extension)
- Test: `crates/cairn-store-sqlite/tests/trace_window.rs`

- [ ] **Step 1: Failing test**

Write `crates/cairn-store-sqlite/tests/trace_window.rs`:

```rust
//! Tests for `SqliteMemoryStore::list_trace_turns` (issue #90).

use cairn_store_sqlite::SqliteMemoryStore;
use cairn_test_fixtures::trace::sample_turn_summary;
use tempfile::tempdir;

#[tokio::test]
async fn returns_turns_in_sequence_order() {
    let dir = tempdir().unwrap();
    let store = SqliteMemoryStore::open(dir.path().join("cairn.db")).await.unwrap();
    let session = "sess-1";
    for seq in 1..=5 {
        store.upsert(&sample_turn_summary(session, seq)).await.unwrap();
    }
    let turns = store.list_trace_turns(session, 0, 10).await.unwrap();
    assert_eq!(turns.iter().map(|h| h.sequence).collect::<Vec<_>>(), vec![1,2,3,4,5]);
    for h in &turns {
        assert_eq!(h.session_id, session);
    }
}

#[tokio::test]
async fn skips_already_covered() {
    let dir = tempdir().unwrap();
    let store = SqliteMemoryStore::open(dir.path().join("cairn.db")).await.unwrap();
    for seq in 1..=5 {
        store.upsert(&sample_turn_summary("s2", seq)).await.unwrap();
    }
    let turns = store.list_trace_turns("s2", 3, 10).await.unwrap();
    assert_eq!(turns.iter().map(|h| h.sequence).collect::<Vec<_>>(), vec![4, 5]);
}
```

If `cairn-test-fixtures` has no `sample_turn_summary`, add it under `crates/cairn-test-fixtures/src/trace.rs`:

```rust
//! Trace-record fixtures (issue #77 + #90).
use cairn_core::domain::record::MemoryRecord;
// ... build a MemoryRecord with kind = Reasoning (trace_event field),
//     extra_frontmatter = { trace_event: "turn_summary", trace: { session_id, turn_id, sequence }}.
//     Read the existing fixture in tests/integration/trace_*.rs to copy the exact shape.
pub fn sample_turn_summary(session: &str, sequence: u32) -> MemoryRecord {
    use cairn_core::domain::record::tests_export::sample_record;
    let mut r = sample_record();
    r.extra_frontmatter = serde_json::json!({
        "trace_event": "turn_summary",
        "trace": {
            "session_id": session,
            "turn_id": format!("turn-{sequence}"),
            "sequence": sequence,
            "capture_event_id": format!("ev-{session}-{sequence}"),
        }
    });
    r
}
```

(Verify exact field names against `crates/cairn-store-sqlite/src/migrations/sql/0023_trace_links.sql` and any existing turn-summary fixture in `crates/cairn-core/src/`.)

- [ ] **Step 2: Implement `list_trace_turns`**

Write `crates/cairn-store-sqlite/src/trace_window.rs`:

```rust
//! `list_trace_turns` — page through a session's `turn_summary` records
//! ordered by `trace_sequence`.
//!
//! Used by `cairn-workflows::consolidation` to build the candidate
//! window for [`cairn_core::pipeline::consolidation::pick_window`].

use cairn_core::pipeline::consolidation::TurnHeader;

use crate::SqliteMemoryStore;

impl SqliteMemoryStore {
    /// Page through `turn_summary` records for `session_id` whose
    /// `trace_sequence > since_sequence`, ascending. Capped by `limit`.
    ///
    /// # Errors
    /// Backend failure.
    pub async fn list_trace_turns(
        &self,
        session_id: &str,
        since_sequence: u32,
        limit: u32,
    ) -> Result<Vec<TurnHeader>, crate::StoreError> {
        let pool = self.read_pool().clone();
        let sql = r#"
            SELECT record_id, trace_session_id, trace_turn_id, trace_sequence,
                   length(body) AS body_len
            FROM records
            WHERE trace_event = 'turn_summary'
              AND trace_session_id = ?1
              AND trace_sequence > ?2
              AND active = 1 AND tombstoned = 0
            ORDER BY trace_sequence ASC
            LIMIT ?3
        "#;
        let session_id = session_id.to_owned();
        let limit = i64::from(limit);
        let since = i64::from(since_sequence);
        tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt.query_map(rusqlite::params![session_id, since, limit], |row| {
                let body_len: i64 = row.get("body_len")?;
                Ok(TurnHeader {
                    record_id: row.get("record_id")?,
                    session_id: row.get("trace_session_id")?,
                    turn_id: row.get("trace_turn_id")?,
                    sequence: u32::try_from(row.get::<_, i64>("trace_sequence")?).unwrap_or(0),
                    approx_tokens: u32::try_from(body_len / 4).unwrap_or(0),
                    salience: 0.5, // P0 baseline — refined when SalienceProjector lands.
                })
            })?;
            let mut out = Vec::new();
            for r in rows { out.push(r?); }
            Ok::<_, crate::StoreError>(out)
        }).await.map_err(|e| crate::StoreError::from(e.to_string()))?
    }
}
```

(Verify `self.read_pool()` matches the existing accessor name on `SqliteMemoryStore`. If `read_pool()` does not exist, locate the method that yields a r2d2 pool clone and use that. Some adapters expose `pool()` or `conn_factory()`.)

Wire into `crates/cairn-store-sqlite/src/lib.rs`:

```rust
mod trace_window;
```

- [ ] **Step 3: Verify**

Run: `cargo test -p cairn-store-sqlite --test trace_window`
Expected: 2 pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-store-sqlite/src/trace_window.rs crates/cairn-store-sqlite/src/lib.rs crates/cairn-store-sqlite/tests/trace_window.rs crates/cairn-test-fixtures/
git commit -m "feat(store-sqlite): list_trace_turns paging helper (issue #90)"
```

---

## Task 11: `ConsolidationPayload` (serde over `JobPayload`)

**Files:**
- Create: `crates/cairn-workflows/src/consolidation/mod.rs`
- Create: `crates/cairn-workflows/src/consolidation/payload.rs`

- [ ] **Step 1: Failing test**

Write `crates/cairn-workflows/src/consolidation/payload.rs`:

```rust
//! Serde-encoded payload carried in `workflow_jobs.payload`.
//! `Bincode` would be smaller but JSON gives us auditability for free
//! and keeps replay logs human-readable.

use cairn_core::contract::job_store::JobPayload;
use serde::{Deserialize, Serialize};

/// One enqueued rolling-summary request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsolidationPayload {
    /// Session whose turns are being summarized.
    pub session_id: String,
    /// Watermark — the highest sequence already covered by a prior
    /// summary for this session. `0` for the first run.
    pub since_sequence: u32,
}

impl ConsolidationPayload {
    /// Serialize to `JobPayload`.
    ///
    /// # Errors
    /// JSON encoding failure (effectively unreachable for this struct).
    pub fn to_bytes(&self) -> Result<JobPayload, serde_json::Error> {
        serde_json::to_vec(self)
    }
    /// Deserialize from `JobPayload`.
    ///
    /// # Errors
    /// JSON decoding failure.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let p = ConsolidationPayload { session_id: "s1".into(), since_sequence: 12 };
        let bytes = p.to_bytes().unwrap();
        let back = ConsolidationPayload::from_bytes(&bytes).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn unknown_fields_rejected() {
        let bytes = br#"{"session_id":"s1","since_sequence":0,"x":1}"#;
        assert!(ConsolidationPayload::from_bytes(bytes).is_err());
    }
}
```

Write `crates/cairn-workflows/src/consolidation/mod.rs`:

```rust
//! Rolling-summary `ConsolidationWorkflow` (issue #90, brief §5.3, §10.0).

pub mod payload;

pub use payload::ConsolidationPayload;
```

Export in `crates/cairn-workflows/src/lib.rs`:

```rust
pub mod consolidation;
pub use consolidation::ConsolidationPayload;
```

- [ ] **Step 2: Verify**

Run: `cargo test -p cairn-workflows --lib consolidation::payload`
Expected: 2 pass.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-workflows/src/consolidation/ crates/cairn-workflows/src/lib.rs
git commit -m "feat(consolidation): job payload (serde)"
```

---

## Task 12: `ConsolidationHandler`

**Files:**
- Create: `crates/cairn-workflows/src/consolidation/handler.rs`
- Modify: `crates/cairn-workflows/src/consolidation/mod.rs`

- [ ] **Step 1: Failing test**

Write `crates/cairn-workflows/src/consolidation/handler.rs`:

```rust
//! `ConsolidationHandler` — pulls a window from the store, calls
//! [`cairn_core::pipeline::consolidation::compute_rolling_summary`],
//! upserts the resulting `reasoning` record. Idempotent: re-running
//! against the same window emits the same record body (the deterministic
//! placeholder) and the store deduplicates via body hash.

use std::sync::Arc;

use cairn_core::config::ConsolidationConfig;
use cairn_core::contract::job_store::{JobKind, JobPayload};
use cairn_core::contract::memory_store::{MemoryStore, UpsertOutcome};
use cairn_core::domain::record::MemoryRecord;
use cairn_core::domain::scope::ScopeTuple;
use cairn_core::domain::taxonomy::{MemoryClass, MemoryKind, MemoryVisibility};
use cairn_core::pipeline::consolidation::{compute_rolling_summary, pick_window, RollingSummaryDraft, SummaryStatus, TurnHeader};
use cairn_store_sqlite::SqliteMemoryStore;
use tracing::{info, warn};

use crate::scheduler::{HandlerOutcome, JobHandler};
use crate::consolidation::ConsolidationPayload;

/// The kind discriminator used in `JobKind`.
pub const CONSOLIDATION_KIND: &str = "consolidation.rolling_summary";

/// Handler entry — needs the concrete `SqliteMemoryStore` because the
/// trace-window helper lives on the adapter, not the trait. A future
/// `TraceReader` trait could pull this back into core.
pub struct ConsolidationHandler {
    store: Arc<SqliteMemoryStore>,
    config: ConsolidationConfig,
}

impl ConsolidationHandler {
    /// Construct.
    #[must_use]
    pub fn new(store: Arc<SqliteMemoryStore>, config: ConsolidationConfig) -> Self {
        Self { store, config }
    }

    async fn run_once(&self, payload: ConsolidationPayload) -> Result<HandlerOutcome, anyhow::Error> {
        // 1. Pull candidate turn headers.
        let candidates: Vec<TurnHeader> = self.store
            .list_trace_turns(&payload.session_id, payload.since_sequence, 256)
            .await?;
        let Some(window) = pick_window(
            &candidates,
            payload.since_sequence,
            self.config.window_size_turns,
            self.config.min_turns_for_trigger,
            self.config.salience_floor,
        ) else {
            info!(session = %payload.session_id, "no eligible window; deferring");
            return Ok(HandlerOutcome::Done);
        };

        // 2. Compute the draft.
        let draft = compute_rolling_summary(&window, &self.config)?;
        if matches!(draft.status, SummaryStatus::Deferred) {
            info!(session = %payload.session_id, "consolidation_deferred (disabled)");
            return Ok(HandlerOutcome::Done);
        }

        // 3. Build and upsert the record.
        let record = build_summary_record(&payload, &draft);
        let UpsertOutcome { record_id, content_changed, .. } = self.store.upsert(&record).await?;
        if content_changed {
            info!(session = %payload.session_id, %record_id,
                  last_seq = draft.last_sequence,
                  "rolling summary emitted");
        } else {
            info!(session = %payload.session_id, %record_id, "rolling summary idempotent");
        }

        Ok(HandlerOutcome::Done)
    }
}

fn build_summary_record(payload: &ConsolidationPayload, draft: &RollingSummaryDraft) -> MemoryRecord {
    let target_id = format!("summary:{}:{}", payload.session_id, draft.last_sequence);
    let extra = serde_json::json!({
        "consolidation": {
            "source_record_ids": draft.source_record_ids,
            "last_sequence": draft.last_sequence,
            "summary_tokens": draft.summary_tokens,
            "produced_by": "cairn-workflows::ConsolidationHandler",
        }
    });
    // Use the existing `MemoryRecord::builder()` or constructor. If a
    // `MemoryRecord::new(...)` exists, prefer it; otherwise mirror the
    // shape used in `cairn-cli::verbs::ingest`. Pseudocode:
    MemoryRecord {
        record_id: cairn_core::domain::RecordId::generate(),
        target_id: cairn_core::domain::TargetId::new(target_id),
        kind: MemoryKind::Reasoning,
        class: MemoryClass::Episodic,
        scope: ScopeTuple { session_id: Some(payload.session_id.clone()), ..ScopeTuple::default() },
        visibility: MemoryVisibility::Private,
        body: draft.body.clone(),
        extra_frontmatter: extra,
        ..MemoryRecord::default_for_summary()
    }
}

#[async_trait::async_trait]
impl JobHandler for ConsolidationHandler {
    fn kind(&self) -> JobKind { JobKind::new(CONSOLIDATION_KIND) }
    async fn handle(&self, payload_bytes: &JobPayload) -> HandlerOutcome {
        let payload = match ConsolidationPayload::from_bytes(payload_bytes) {
            Ok(p) => p,
            Err(e) => return HandlerOutcome::Permanent { reason: format!("payload decode: {e}") },
        };
        match self.run_once(payload).await {
            Ok(o) => o,
            Err(e) => {
                warn!(error = %e, "consolidation run failed");
                HandlerOutcome::Retry { reason: e.to_string() }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_test_fixtures::trace::sample_turn_summary;
    use tempfile::tempdir;

    #[tokio::test]
    async fn handler_emits_summary_after_threshold() {
        let dir = tempdir().unwrap();
        let store = Arc::new(SqliteMemoryStore::open(dir.path().join("cairn.db")).await.unwrap());
        for seq in 1..=6 {
            store.upsert(&sample_turn_summary("s1", seq)).await.unwrap();
        }
        let cfg = ConsolidationConfig::default();
        let h = ConsolidationHandler::new(store.clone(), cfg);
        let payload = ConsolidationPayload { session_id: "s1".into(), since_sequence: 0 };
        let outcome = h.handle(&payload.to_bytes().unwrap()).await;
        assert_eq!(outcome, HandlerOutcome::Done);
        // Verify the summary record exists.
        let listed = store.list(&Default::default()).await.unwrap();
        let any_summary = listed.records.iter().any(|r| {
            r.kind == MemoryKind::Reasoning &&
            r.extra_frontmatter.get("consolidation").is_some()
        });
        assert!(any_summary);
    }

    #[tokio::test]
    async fn handler_idempotent_on_replay() {
        let dir = tempdir().unwrap();
        let store = Arc::new(SqliteMemoryStore::open(dir.path().join("cairn.db")).await.unwrap());
        for seq in 1..=6 {
            store.upsert(&sample_turn_summary("s2", seq)).await.unwrap();
        }
        let h = ConsolidationHandler::new(store.clone(), ConsolidationConfig::default());
        let payload = ConsolidationPayload { session_id: "s2".into(), since_sequence: 0 };
        let bytes = payload.to_bytes().unwrap();
        let _ = h.handle(&bytes).await;
        let _ = h.handle(&bytes).await;
        let listed = store.list(&Default::default()).await.unwrap();
        let count = listed.records.iter().filter(|r| r.kind == MemoryKind::Reasoning).count();
        assert_eq!(count, 1, "second handler call must be a no-op via body-hash dedup");
    }
}
```

If `MemoryRecord::default_for_summary()` does not exist, replace with a fully-specified literal mirroring `crates/cairn-core/src/domain/record/tests_export.rs::sample_record` plus the kind/class/scope overrides. Reading the actual struct first is required — do not invent fields.

Update `crates/cairn-workflows/src/consolidation/mod.rs`:

```rust
pub mod handler;
pub mod payload;

pub use handler::{ConsolidationHandler, CONSOLIDATION_KIND};
pub use payload::ConsolidationPayload;
```

Export in `crates/cairn-workflows/src/lib.rs`:

```rust
pub use consolidation::{ConsolidationHandler, ConsolidationPayload, CONSOLIDATION_KIND};
```

- [ ] **Step 2: Add `anyhow` dep**

`cairn-workflows` is a library; we already use `thiserror`. Use `anyhow` here only at the `run_once → handle` boundary (handler is allowed to surface errors as strings). Confirm `anyhow = { workspace = true }` is in `crates/cairn-workflows/Cargo.toml`. If not, add it.

- [ ] **Step 3: Run**

Run: `cargo test -p cairn-workflows --lib consolidation::handler`
Expected: 2 pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-workflows/src/consolidation/handler.rs crates/cairn-workflows/src/consolidation/mod.rs crates/cairn-workflows/src/lib.rs crates/cairn-workflows/Cargo.toml
git commit -m "feat(consolidation): ConsolidationHandler over MemoryStore upsert (brief §5.3)"
```

---

## Task 13: Enqueue trigger — `enqueue_if_due`

**Files:**
- Create: `crates/cairn-workflows/src/consolidation/trigger.rs`
- Modify: `crates/cairn-workflows/src/consolidation/mod.rs`

- [ ] **Step 1: Failing test**

Write `crates/cairn-workflows/src/consolidation/trigger.rs`:

```rust
//! Enqueue rolling-summary jobs. Called from the `capture_trace` verb
//! after every `turn_summary` write. Idempotent via `dedupe_key` —
//! enqueuing the same `(session, since_sequence)` twice is a no-op.

use std::sync::Arc;

use cairn_core::config::ConsolidationConfig;
use cairn_core::contract::job_store::{
    DuplicateDedupeKey, EnqueueRequest, JobId, JobKind, JobStore, JobStoreError, RetryPolicy,
};

use super::{ConsolidationPayload, CONSOLIDATION_KIND};

/// Decision returned by [`enqueue_if_due`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnqueueDecision {
    /// A job was enqueued (or was idempotent-deduped).
    Enqueued {
        /// The id of the row that was inserted (or already existed).
        job_id: JobId,
    },
    /// Too few new turns since the last summary; deferred.
    NotDue {
        /// Highest sequence we observed.
        latest_sequence: u32,
        /// Watermark we compared against.
        since_sequence: u32,
    },
    /// `consolidation.enabled = false` in config.
    Disabled,
}

/// Enqueue a rolling-summary job if the cadence threshold is reached.
///
/// `latest_sequence` is the `trace_sequence` of the just-written
/// `turn_summary`. `since_sequence` is the watermark from the previous
/// summary (or 0).
///
/// # Errors
/// `JobStoreError::Backend` from the store; `DuplicateDedupeKey` is
/// swallowed (idempotency is the design).
pub async fn enqueue_if_due(
    store: &dyn JobStore,
    config: &ConsolidationConfig,
    session_id: &str,
    latest_sequence: u32,
    since_sequence: u32,
    now_ms: i64,
) -> Result<EnqueueDecision, JobStoreError> {
    if !config.enabled {
        return Ok(EnqueueDecision::Disabled);
    }
    let new_turns = latest_sequence.saturating_sub(since_sequence);
    if new_turns < config.min_turns_for_trigger {
        return Ok(EnqueueDecision::NotDue { latest_sequence, since_sequence });
    }
    let payload = ConsolidationPayload {
        session_id: session_id.to_owned(),
        since_sequence,
    };
    let bytes = payload.to_bytes().map_err(|e| JobStoreError::Backend(e.to_string()))?;
    let job_id = JobId::new(format!("consolidate:{session_id}:{since_sequence}"));
    let req = EnqueueRequest {
        job_id: job_id.clone(),
        kind: JobKind::new(CONSOLIDATION_KIND),
        payload: bytes,
        queue_key: Some(format!("consolidation:{session_id}")),
        dedupe_key: Some(format!("{session_id}:{since_sequence}")),
        not_before_ms: now_ms,
        retry: RetryPolicy::DEFAULT,
    };
    match store.enqueue(req).await {
        Ok(()) => Ok(EnqueueDecision::Enqueued { job_id }),
        Err(JobStoreError::DuplicateDedupeKey { .. }) => Ok(EnqueueDecision::Enqueued { job_id }),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite_store::install_for_tests;
    use crate::SqliteJobStore;
    use rusqlite::Connection;

    fn store() -> Arc<dyn JobStore> {
        let conn = Connection::open_in_memory().unwrap();
        install_for_tests(&conn);
        Arc::new(SqliteJobStore::new(conn).unwrap())
    }

    #[tokio::test]
    async fn not_due_below_threshold() {
        let s = store();
        let cfg = ConsolidationConfig::default(); // min_for_trigger = 4
        let d = enqueue_if_due(&*s, &cfg, "s1", 2, 0, 1_000).await.unwrap();
        assert!(matches!(d, EnqueueDecision::NotDue { .. }));
    }

    #[tokio::test]
    async fn enqueues_when_due() {
        let s = store();
        let cfg = ConsolidationConfig::default();
        let d = enqueue_if_due(&*s, &cfg, "s1", 10, 0, 1_000).await.unwrap();
        assert!(matches!(d, EnqueueDecision::Enqueued { .. }));
    }

    #[tokio::test]
    async fn second_enqueue_idempotent() {
        let s = store();
        let cfg = ConsolidationConfig::default();
        let _ = enqueue_if_due(&*s, &cfg, "s1", 10, 0, 1_000).await.unwrap();
        let d = enqueue_if_due(&*s, &cfg, "s1", 10, 0, 1_000).await.unwrap();
        assert!(matches!(d, EnqueueDecision::Enqueued { .. }), "dup must surface as Enqueued");
    }

    #[tokio::test]
    async fn disabled_returns_disabled() {
        let s = store();
        let cfg = ConsolidationConfig { enabled: false, ..ConsolidationConfig::default() };
        let d = enqueue_if_due(&*s, &cfg, "s1", 10, 0, 1_000).await.unwrap();
        assert_eq!(d, EnqueueDecision::Disabled);
    }
}
```

Update `crates/cairn-workflows/src/consolidation/mod.rs`:

```rust
pub mod handler;
pub mod payload;
pub mod trigger;

pub use handler::{ConsolidationHandler, CONSOLIDATION_KIND};
pub use payload::ConsolidationPayload;
pub use trigger::{enqueue_if_due, EnqueueDecision};
```

- [ ] **Step 2: Run**

Run: `cargo test -p cairn-workflows --lib consolidation::trigger`
Expected: 4 pass.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-workflows/src/consolidation/trigger.rs crates/cairn-workflows/src/consolidation/mod.rs
git commit -m "feat(consolidation): enqueue_if_due trigger (cadence-gated)"
```

---

## Task 14: Forget cleanup handler

**Files:**
- Create: `crates/cairn-workflows/src/consolidation/forget_cleanup.rs`
- Modify: `crates/cairn-workflows/src/consolidation/mod.rs`

- [ ] **Step 1: Failing test**

Write `crates/cairn-workflows/src/consolidation/forget_cleanup.rs`:

```rust
//! `ConsolidationForgetCleanupHandler` — when a source turn record is
//! forgotten, any rolling summary that referenced it as a source
//! becomes orphan-linked. The cleanup handler tombstones such
//! summaries with reason `Forget`. Triggered by the `forget` verb
//! enqueuing one of these jobs per forgotten record.

use std::sync::Arc;

use cairn_core::contract::job_store::{JobKind, JobPayload};
use cairn_core::contract::memory_store::{MemoryStore, TombstoneReason};
use cairn_core::domain::taxonomy::MemoryKind;
use cairn_store_sqlite::SqliteMemoryStore;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::scheduler::{HandlerOutcome, JobHandler};

/// Kind discriminator.
pub const FORGET_CLEANUP_KIND: &str = "consolidation.forget_cleanup";

/// Payload — the record id that was forgotten.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgetCleanupPayload {
    /// The forgotten source `record_id`.
    pub forgotten_record_id: String,
}

impl ForgetCleanupPayload {
    /// Serialize.
    ///
    /// # Errors
    /// JSON encoding (effectively unreachable).
    pub fn to_bytes(&self) -> Result<JobPayload, serde_json::Error> {
        serde_json::to_vec(self)
    }
}

/// Handler.
pub struct ConsolidationForgetCleanupHandler {
    store: Arc<SqliteMemoryStore>,
}

impl ConsolidationForgetCleanupHandler {
    /// Construct.
    #[must_use]
    pub fn new(store: Arc<SqliteMemoryStore>) -> Self { Self { store } }

    async fn run(&self, payload: ForgetCleanupPayload) -> Result<HandlerOutcome, anyhow::Error> {
        // Find rolling summaries that mention `forgotten_record_id` in
        // their `consolidation.source_record_ids` array. The adapter
        // exposes a small query helper.
        let summaries = self.store
            .find_summaries_by_source(&payload.forgotten_record_id)
            .await?;
        for (record_id, _record) in summaries {
            self.store.tombstone(&record_id, TombstoneReason::Forget).await?;
            info!(source = %payload.forgotten_record_id, summary = %record_id,
                  "tombstoned orphan summary");
        }
        Ok(HandlerOutcome::Done)
    }
}

#[async_trait::async_trait]
impl JobHandler for ConsolidationForgetCleanupHandler {
    fn kind(&self) -> JobKind { JobKind::new(FORGET_CLEANUP_KIND) }
    async fn handle(&self, bytes: &JobPayload) -> HandlerOutcome {
        let Ok(payload) = serde_json::from_slice::<ForgetCleanupPayload>(bytes) else {
            return HandlerOutcome::Permanent { reason: "payload decode".into() };
        };
        match self.run(payload).await {
            Ok(o) => o,
            Err(e) => {
                warn!(error = %e, "forget cleanup failed");
                HandlerOutcome::Retry { reason: e.to_string() }
            }
        }
    }
}
```

`find_summaries_by_source` is a new tiny helper on `SqliteMemoryStore`. Add to `crates/cairn-store-sqlite/src/trace_window.rs`:

```rust
use cairn_core::domain::RecordId;
use cairn_core::domain::record::MemoryRecord;

impl SqliteMemoryStore {
    /// Find active records whose `extra_frontmatter.consolidation.source_record_ids`
    /// JSON array contains `source_record_id`.
    ///
    /// # Errors
    /// Backend failure.
    pub async fn find_summaries_by_source(
        &self,
        source_record_id: &str,
    ) -> Result<Vec<(RecordId, MemoryRecord)>, crate::StoreError> {
        let pool = self.read_pool().clone();
        let sql = r#"
            SELECT record_id, body, extra_frontmatter, target_id, kind, class,
                   visibility, scope_tenant, scope_workspace, scope_session_id,
                   scope_entity, scope_user, scope_agent
            FROM records
            WHERE active = 1 AND tombstoned = 0
              AND json_extract(extra_frontmatter, '$.consolidation') IS NOT NULL
              AND EXISTS (
                  SELECT 1 FROM json_each(json_extract(extra_frontmatter, '$.consolidation.source_record_ids'))
                  WHERE value = ?1
              )
        "#;
        let needle = source_record_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt.query_map(rusqlite::params![needle], |row| {
                let id: String = row.get("record_id")?;
                // Re-hydrate the record. If the adapter has an existing helper for
                // this — e.g. `MemoryRecord::hydrate_row(&Row)` — call it instead
                // of hand-rolling the projection here.
                Ok((RecordId::new(id), crate::row::hydrate_memory_record(row)?))
            })?;
            let mut out = Vec::new();
            for r in rows { out.push(r?); }
            Ok::<_, crate::StoreError>(out)
        }).await.map_err(|e| crate::StoreError::from(e.to_string()))?
    }
}
```

(If the adapter lacks `crate::row::hydrate_memory_record`, prefer reading just the columns the handler actually needs — `record_id` and `extra_frontmatter` — and returning `Vec<RecordId>` from the helper instead. Update the handler signature accordingly. The intent is: do NOT introduce ad-hoc record-deserialization code if one already exists.)

Append test in `crates/cairn-workflows/tests/forget_propagation.rs` (created in Task 19) — for now, just compile.

Update `crates/cairn-workflows/src/consolidation/mod.rs`:

```rust
pub mod forget_cleanup;
pub use forget_cleanup::{ConsolidationForgetCleanupHandler, ForgetCleanupPayload, FORGET_CLEANUP_KIND};
```

- [ ] **Step 2: Compile**

Run: `cargo check -p cairn-workflows -p cairn-store-sqlite`
Expected: clean build.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-workflows/src/consolidation/forget_cleanup.rs crates/cairn-workflows/src/consolidation/mod.rs crates/cairn-store-sqlite/src/trace_window.rs
git commit -m "feat(consolidation): forget cleanup handler + summary-by-source query"
```

---

## Task 15: Wire `assemble_hot` to consume rolling summaries

**Files:**
- Modify: `crates/cairn-core/src/verbs/assemble_hot/inputs.rs`
- Create: `crates/cairn-core/src/verbs/assemble_hot/sources/rolling_summary.rs`
- Modify: `crates/cairn-core/src/verbs/assemble_hot/segments.rs`
- Modify: `crates/cairn-core/src/verbs/assemble_hot/mod.rs`

- [ ] **Step 1: Failing test**

In `crates/cairn-core/src/verbs/assemble_hot/inputs.rs`, add a slot:

```rust
/// Rolling-summary `reasoning` records produced by the
/// `ConsolidationWorkflow` (issue #90).
pub rolling_summary_candidates: &'a [&'a MemoryRecord],
```

Update the `tests` block to pass `rolling_summary_candidates: &[]` so the struct still constructs.

Write `crates/cairn-core/src/verbs/assemble_hot/sources/rolling_summary.rs`:

```rust
//! Rolling-summary source — turns `reasoning`-kind episodic summaries
//! into hot-prefix segments. Selection is recency-biased: newest
//! summary per session_id wins.

use std::collections::BTreeMap;

use crate::domain::record::MemoryRecord;
use crate::domain::taxonomy::MemoryKind;

/// Pick at most `max_per_session` summaries per session, newest-first.
#[must_use]
pub fn select<'a>(
    candidates: &'a [&'a MemoryRecord],
    max_per_session: usize,
) -> Vec<&'a MemoryRecord> {
    let mut by_session: BTreeMap<&str, Vec<&MemoryRecord>> = BTreeMap::new();
    for r in candidates {
        if r.kind != MemoryKind::Reasoning { continue; }
        let Some(session) = r.scope.session_id.as_deref() else { continue; };
        by_session.entry(session).or_default().push(r);
    }
    let mut out = Vec::new();
    for (_session, mut group) in by_session {
        group.sort_by_key(|r| std::cmp::Reverse(r.updated_at));
        for r in group.into_iter().take(max_per_session) { out.push(r); }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::record::tests_export::sample_record;
    use crate::domain::taxonomy::MemoryKind;

    fn summary(session: &str, updated: i64) -> MemoryRecord {
        let mut r = sample_record();
        r.kind = MemoryKind::Reasoning;
        r.scope.session_id = Some(session.into());
        r.updated_at = updated;
        r
    }

    #[test]
    fn newest_summary_per_session_only() {
        let a1 = summary("a", 10);
        let a2 = summary("a", 20);
        let b1 = summary("b", 5);
        let recs = [&a1, &a2, &b1];
        let picked = select(&recs[..], 1);
        let ids: Vec<&str> = picked.iter().map(|r| r.scope.session_id.as_deref().unwrap()).collect();
        assert!(ids.contains(&"a") && ids.contains(&"b"));
        // Inside session `a`, the picked record should be the one with updated_at = 20.
        let a = picked.iter().find(|r| r.scope.session_id.as_deref() == Some("a")).unwrap();
        assert_eq!(a.updated_at, 20);
    }

    #[test]
    fn skips_non_reasoning() {
        let mut r = sample_record();
        r.kind = MemoryKind::User;
        r.scope.session_id = Some("z".into());
        let recs = [&r];
        assert!(select(&recs[..], 1).is_empty());
    }
}
```

Wire the source into `segments.rs` and the assembler so the rendered hot prefix gains a section labelled `Recent reasoning summaries` (mirror the existing playbook/project segment patterns; one paragraph per summary). Exact code lives in `crates/cairn-core/src/verbs/assemble_hot/segments.rs` — add after the playbook segment block, gated on `cfg.rolling_summaries_enabled` if such a flag exists, otherwise unconditional.

- [ ] **Step 2: Run hot-memory tests**

Run: `cargo test -p cairn-core --lib verbs::assemble_hot`
Expected: existing tests still pass; new `rolling_summary::tests` pass.

- [ ] **Step 3: Update fixtures**

If hot-memory snapshot tests live under `crates/cairn-core/src/verbs/assemble_hot/snapshots/`, regenerate any that drift:

```bash
INSTA_UPDATE=auto cargo nextest run -p cairn-core
cargo insta review
```

Inspect each diff and accept only the ones that contain a new "Recent reasoning summaries" block (or that gain an empty allocation in the segment list — verify intent).

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/verbs/assemble_hot/
git commit -m "feat(assemble_hot): consume rolling summaries from ConsolidationWorkflow (brief §5.0)"
```

---

## Task 16: Wire enqueue calls into `capture_trace` and `forget`

**Files:**
- Modify: `crates/cairn-cli/src/verbs/capture_trace.rs` (post turn_summary write)
- Modify: `crates/cairn-cli/src/verbs/forget.rs` (post record tombstone)

- [ ] **Step 1: capture_trace hook**

In `crates/cairn-cli/src/verbs/capture_trace.rs`, locate where a `turn_summary` record is committed (search for `trace_event: "turn_summary"` or the constant). Immediately after the successful upsert, compute the watermark (latest existing summary's `consolidation.last_sequence` for this session — 0 if none) and call:

```rust
if let Some(scheduler) = ctx.workflows() {
    let _ = cairn_workflows::consolidation::enqueue_if_due(
        scheduler.job_store(),
        &ctx.config.consolidation,
        &session_id,
        latest_sequence,
        since_sequence,
        ctx.clock.now_ms(),
    ).await;
}
```

`ctx.workflows()` is whatever accessor exposes the live scheduler/job-store handle from the verb context. If no such handle exists today, plumb one through — pass `Arc<dyn JobStore>` into the verb dispatcher's context struct.

- [ ] **Step 2: forget hook**

In `crates/cairn-cli/src/verbs/forget.rs`, after a successful `--record` path tombstones the source, enqueue a cleanup job:

```rust
let payload = ForgetCleanupPayload { forgotten_record_id: record_id.to_string() };
let bytes = payload.to_bytes()?;
let req = EnqueueRequest {
    job_id: JobId::new(format!("forget-cleanup:{record_id}")),
    kind: JobKind::new(FORGET_CLEANUP_KIND),
    payload: bytes,
    queue_key: None,
    dedupe_key: Some(record_id.to_string()),
    not_before_ms: ctx.clock.now_ms(),
    retry: RetryPolicy::DEFAULT,
};
let _ = ctx.workflows().job_store().enqueue(req).await;
```

Swallow `DuplicateDedupeKey` errors silently.

- [ ] **Step 3: Run forget verb tests**

Run: `cargo test -p cairn-cli verbs::forget`
Expected: existing tests still pass; if a fixture asserts no job is enqueued, update or remove that assertion.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-cli/src/verbs/
git commit -m "feat(cli): enqueue ConsolidationWorkflow + forget cleanup jobs"
```

---

## Task 17: Start scheduler in `cairn mcp serve`; flip capability

**Files:**
- Modify: `crates/cairn-cli/src/verbs/mcp.rs` (or wherever the long-lived MCP entry point lives)
- Modify: `crates/cairn-core/src/status/wiring.rs`
- Modify: `crates/cairn-workflows/src/lib.rs` (capability bits)

- [ ] **Step 1: Boot the scheduler**

In the long-running MCP serve handler, construct and start the scheduler after the store is open:

```rust
let job_store: Arc<dyn JobStore> = Arc::new(SqliteJobStore::new(... )?);
let consolidation = ConsolidationHandler::new(memory_store.clone(), config.consolidation);
let forget_cleanup = ConsolidationForgetCleanupHandler::new(memory_store.clone());
let registry = HandlerRegistryBuilder::default()
    .with(Arc::new(consolidation))
    .with(Arc::new(forget_cleanup))
    .build();
let clock: Arc<dyn Clock> = Arc::new(SystemClock);
let incarnation_id = ulid::Ulid::new().to_string();
let scheduler = Scheduler::start(&incarnation_id, job_store.clone(), registry, clock, SchedulerConfig::p0());

// On signal:
tokio::select! {
    _ = mcp_serve_run() => {},
    _ = signal_ctrl_c() => {},
}
scheduler.shutdown().await;
```

`ulid` is already in workspace deps (used elsewhere); confirm before importing. Adjust SQLite job-store construction to share the same connection / pool as the memory store if that is the existing convention — otherwise use a dedicated SQLite connection for jobs (which the migration manifest already supports).

- [ ] **Step 2: Flip orchestrator capabilities**

In `crates/cairn-workflows/src/lib.rs`, update the `InProcessOrchestrator`:

```rust
fn capabilities(&self) -> &WorkflowOrchestratorCapabilities {
    static CAPS: WorkflowOrchestratorCapabilities = WorkflowOrchestratorCapabilities {
        durable: true,
        crash_safe: true,
        cron_schedules: false,
    };
    &CAPS
}
```

- [ ] **Step 3: Flip capability flag**

In `crates/cairn-core/src/status/wiring.rs`:

```rust
pub const CONSOLIDATION_WORKFLOW_WIRED: bool = true;
```

- [ ] **Step 4: Re-run status tests**

Run: `cargo test -p cairn-core --lib status`
Expected: existing snapshot tests pick up the new capability. If `crates/cairn-core/src/status/tests.rs` has a `byte_identical_status_payload` test, update its golden via `cargo insta review` (or whatever the project uses for that snapshot).

Run: `cargo run -p cairn-cli --bin cairn-docgen -- --check`
Expected: doc-gen reports drift; re-run with `--write` and stage the regenerated reference Markdown.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/ crates/cairn-core/src/status/wiring.rs crates/cairn-workflows/src/lib.rs docs/site/src/reference/generated/
git commit -m "feat(workflows): boot Scheduler in mcp serve; flip CONSOLIDATION_WORKFLOW_WIRED"
```

---

## Task 18: Integration test — long session emits rolling summaries

**Files:**
- Create: `crates/cairn-workflows/tests/rolling_summary.rs`

- [ ] **Step 1: Write the test**

```rust
//! End-to-end: many turn_summary writes → ConsolidationHandler runs
//! via the live scheduler → reasoning record materializes for the
//! session, linking back to its source turns.

use std::sync::Arc;
use std::time::Duration;

use cairn_core::config::ConsolidationConfig;
use cairn_core::contract::job_store::JobStore;
use cairn_core::domain::taxonomy::MemoryKind;
use cairn_store_sqlite::SqliteMemoryStore;
use cairn_test_fixtures::trace::sample_turn_summary;
use cairn_workflows::{
    consolidation::{enqueue_if_due, ConsolidationHandler, CONSOLIDATION_KIND},
    scheduler::{Clock, HandlerRegistryBuilder, Scheduler, SchedulerConfig, SystemClock},
    SqliteJobStore,
};
use tempfile::tempdir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn long_session_emits_rolling_summary_without_blocking_turns() {
    let dir = tempdir().unwrap();
    let mem = Arc::new(SqliteMemoryStore::open(dir.path().join("cairn.db")).await.unwrap());
    let jobs: Arc<dyn JobStore> = Arc::new(SqliteJobStore::new(rusqlite::Connection::open(dir.path().join("jobs.db")).unwrap()).unwrap());
    let cfg = ConsolidationConfig::default();
    let h = Arc::new(ConsolidationHandler::new(mem.clone(), cfg));
    let registry = HandlerRegistryBuilder::default().with(h).build();
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let s = Scheduler::start("inc", jobs.clone(), registry, clock.clone(), SchedulerConfig::p0());

    // Simulate 12 turns landing.
    let session = "long-session";
    for seq in 1..=12 {
        mem.upsert(&sample_turn_summary(session, seq)).await.unwrap();
        let _ = enqueue_if_due(&*jobs, &cfg, session, seq, 0, clock.now_ms()).await;
    }

    // Poll for the summary to appear.
    let start = std::time::Instant::now();
    let mut found = false;
    while start.elapsed() < Duration::from_secs(10) {
        let listed = mem.list(&Default::default()).await.unwrap();
        if listed.records.iter().any(|r|
            r.kind == MemoryKind::Reasoning &&
            r.scope.session_id.as_deref() == Some(session)
        ) {
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    s.shutdown().await;
    assert!(found, "rolling summary was never emitted within 10s");
}
```

- [ ] **Step 2: Run**

Run: `cargo nextest run -p cairn-workflows --test rolling_summary`
Expected: pass within ~2s in practice.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-workflows/tests/rolling_summary.rs
git commit -m "test(consolidation): end-to-end rolling-summary emission"
```

---

## Task 19: Integration test — forget propagation

**Files:**
- Create: `crates/cairn-workflows/tests/forget_propagation.rs`

- [ ] **Step 1: Test**

```rust
//! Forget a source turn → ConsolidationForgetCleanupHandler tombstones
//! any rolling summary that referenced it.

use std::sync::Arc;
use std::time::Duration;

use cairn_core::contract::job_store::{EnqueueRequest, JobId, JobKind, JobStore, RetryPolicy};
use cairn_core::contract::memory_store::TombstoneReason;
use cairn_core::domain::taxonomy::MemoryKind;
use cairn_store_sqlite::SqliteMemoryStore;
use cairn_test_fixtures::trace::sample_turn_summary;
use cairn_workflows::{
    consolidation::{
        ConsolidationForgetCleanupHandler, ConsolidationHandler, ConsolidationPayload,
        ForgetCleanupPayload, CONSOLIDATION_KIND, FORGET_CLEANUP_KIND,
    },
    scheduler::{Clock, HandlerRegistryBuilder, Scheduler, SchedulerConfig, SystemClock},
    SqliteJobStore,
};
use tempfile::tempdir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forgetting_source_turn_tombstones_summary() {
    let dir = tempdir().unwrap();
    let mem = Arc::new(SqliteMemoryStore::open(dir.path().join("cairn.db")).await.unwrap());
    let jobs: Arc<dyn JobStore> = Arc::new(SqliteJobStore::new(rusqlite::Connection::open(dir.path().join("jobs.db")).unwrap()).unwrap());
    let cfg = cairn_core::config::ConsolidationConfig::default();
    let registry = HandlerRegistryBuilder::default()
        .with(Arc::new(ConsolidationHandler::new(mem.clone(), cfg)))
        .with(Arc::new(ConsolidationForgetCleanupHandler::new(mem.clone())))
        .build();
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let s = Scheduler::start("inc", jobs.clone(), registry, clock.clone(), SchedulerConfig::p0());

    // Seed: 6 turns then enqueue a summary.
    let session = "fs";
    for seq in 1..=6 { mem.upsert(&sample_turn_summary(session, seq)).await.unwrap(); }
    jobs.enqueue(EnqueueRequest {
        job_id: JobId::new("c-1"),
        kind: JobKind::new(CONSOLIDATION_KIND),
        payload: ConsolidationPayload { session_id: session.into(), since_sequence: 0 }.to_bytes().unwrap(),
        queue_key: None,
        dedupe_key: None,
        not_before_ms: clock.now_ms(),
        retry: RetryPolicy::DEFAULT,
    }).await.unwrap();

    // Wait for the summary.
    let mut summary_id = None;
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        let listed = mem.list(&Default::default()).await.unwrap();
        if let Some(s) = listed.records.iter().find(|r| r.kind == MemoryKind::Reasoning) {
            summary_id = Some(s.record_id.clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let summary_id = summary_id.expect("summary emitted");

    // Forget the first source turn directly via the store; enqueue cleanup.
    let first_source = mem.list_trace_turns(session, 0, 1).await.unwrap().pop().unwrap().record_id;
    let rec_id = cairn_core::domain::RecordId::new(first_source.clone());
    mem.tombstone(&rec_id, TombstoneReason::Forget).await.unwrap();
    jobs.enqueue(EnqueueRequest {
        job_id: JobId::new("fc-1"),
        kind: JobKind::new(FORGET_CLEANUP_KIND),
        payload: ForgetCleanupPayload { forgotten_record_id: first_source }.to_bytes().unwrap(),
        queue_key: None,
        dedupe_key: None,
        not_before_ms: clock.now_ms(),
        retry: RetryPolicy::DEFAULT,
    }).await.unwrap();

    // Wait for the summary to disappear (tombstoned).
    let start = std::time::Instant::now();
    let mut gone = false;
    while start.elapsed() < Duration::from_secs(10) {
        if mem.get(&summary_id).await.unwrap().is_none() {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    s.shutdown().await;
    assert!(gone, "forget cleanup never tombstoned the summary");
}
```

- [ ] **Step 2: Run**

Run: `cargo nextest run -p cairn-workflows --test forget_propagation`
Expected: pass.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-workflows/tests/forget_propagation.rs
git commit -m "test(consolidation): forget propagation from source to summary"
```

---

## Task 20: Integration test — long-session token budget

**Files:**
- Create: `crates/cairn-workflows/tests/long_session_budget.rs`

- [ ] **Step 1: Test**

```rust
//! 200 turns in one session → summary body must respect `token_budget`
//! and the summary must list `source_record_ids` for every covered turn.

use std::sync::Arc;
use std::time::Duration;

use cairn_core::config::ConsolidationConfig;
use cairn_core::domain::taxonomy::MemoryKind;
use cairn_store_sqlite::SqliteMemoryStore;
use cairn_test_fixtures::trace::sample_turn_summary;
use cairn_workflows::{
    consolidation::{ConsolidationHandler, ConsolidationPayload, CONSOLIDATION_KIND},
    scheduler::{Clock, HandlerRegistryBuilder, Scheduler, SchedulerConfig, SystemClock},
    SqliteJobStore,
};
use cairn_core::contract::job_store::{EnqueueRequest, JobId, JobKind, JobStore, RetryPolicy};
use tempfile::tempdir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn long_session_summary_fits_within_token_budget() {
    let dir = tempdir().unwrap();
    let mem = Arc::new(SqliteMemoryStore::open(dir.path().join("cairn.db")).await.unwrap());
    let jobs: Arc<dyn JobStore> = Arc::new(SqliteJobStore::new(rusqlite::Connection::open(dir.path().join("jobs.db")).unwrap()).unwrap());
    let cfg = ConsolidationConfig {
        window_size_turns: 32,
        token_budget: 200,
        ..ConsolidationConfig::default()
    };
    let h = Arc::new(ConsolidationHandler::new(mem.clone(), cfg));
    let registry = HandlerRegistryBuilder::default().with(h).build();
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let s = Scheduler::start("inc", jobs.clone(), registry, clock.clone(), SchedulerConfig::p0());

    let session = "long";
    for seq in 1..=200 { mem.upsert(&sample_turn_summary(session, seq)).await.unwrap(); }
    jobs.enqueue(EnqueueRequest {
        job_id: JobId::new("c-long"),
        kind: JobKind::new(CONSOLIDATION_KIND),
        payload: ConsolidationPayload { session_id: session.into(), since_sequence: 0 }.to_bytes().unwrap(),
        queue_key: None,
        dedupe_key: None,
        not_before_ms: clock.now_ms(),
        retry: RetryPolicy::DEFAULT,
    }).await.unwrap();

    let start = std::time::Instant::now();
    let mut found = None;
    while start.elapsed() < Duration::from_secs(10) {
        let listed = mem.list(&Default::default()).await.unwrap();
        if let Some(r) = listed.records.iter().find(|r| r.kind == MemoryKind::Reasoning) {
            found = Some(r.clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    s.shutdown().await;
    let r = found.expect("summary emitted");
    // Body length must respect the budget × 4 chars approximation (plus a
    // small truncation marker).
    assert!(r.body.len() <= 200 * 4 + 8, "body length {} exceeds budget", r.body.len());
    // Source ids list size matches the window cap, not 200.
    let src = r.extra_frontmatter
        .get("consolidation").and_then(|c| c.get("source_record_ids"))
        .and_then(|v| v.as_array()).cloned().unwrap_or_default();
    assert_eq!(src.len(), 32, "summary should link exactly window_size_turns sources");
}
```

- [ ] **Step 2: Run**

Run: `cargo nextest run -p cairn-workflows --test long_session_budget`
Expected: pass.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-workflows/tests/long_session_budget.rs
git commit -m "test(consolidation): long-session token budget invariant"
```

---

## Task 21: Verification + PR

- [ ] **Step 1: Run the full workspace verification (CLAUDE.md §8)**

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
```

Fix anything red. If any of `--check` complains, run the corresponding `--write` form, inspect the diff, and commit it as `chore(generated): regenerate <thing> for issue #90`.

- [ ] **Step 2: Open PR**

```bash
git push -u origin <branch>
gh pr create --title "feat(workflows): rolling-summary ConsolidationWorkflow + tokio scheduler (#90)" --body "$(cat <<'EOF'
## Summary
- Lands the P0 rolling-summary `ConsolidationWorkflow` per brief §5.3 / §10.0 / §19.a.
- Adds the missing tokio scheduler loop (#89 deferred) — worker pool, heartbeat, reaper, clock injection, graceful shutdown.
- Wires forget propagation: forgetting a source turn tombstones any summary referencing it.
- `assemble_hot` now consumes rolling summaries instead of raw turn streams.
- Capability advertised behind `CONSOLIDATION_WORKFLOW_WIRED` once the dispatch is live; `WorkflowOrchestratorCapabilities { durable: true, crash_safe: true }` now reflects reality.

## Test plan
- [x] `cargo nextest run --workspace`
- [x] `crates/cairn-workflows/tests/rolling_summary.rs` (acceptance: long sessions produce summaries without blocking)
- [x] `crates/cairn-workflows/tests/forget_propagation.rs` (acceptance: source-link forget propagation)
- [x] `crates/cairn-workflows/tests/long_session_budget.rs` (acceptance: hot memory uses summaries, budget cap holds)
- [x] `cargo clippy --workspace --all-targets -- -D warnings`
- [x] `./scripts/check-core-boundary.sh`
- [x] `cargo run -p cairn-cli --bin cairn-docgen -- --check`

Closes #90.
EOF
)"
```

---

## Self-Review

**Spec coverage**

| Acceptance criterion | Covered by |
|---|---|
| Long sessions produce rolling summaries without blocking turns | Task 18 + the worker loop's async nature (turns commit via `MemoryStore::upsert`, enqueue is fire-and-forget) |
| Summaries link back to source turns | Task 12 — `build_summary_record` writes `consolidation.source_record_ids` |
| Can be forgotten if source records are forgotten | Tasks 14, 16, 19 |
| Hot memory uses summaries instead of unbounded raw history | Task 15 |
| Run rolling summary fixture tests | Tasks 12 (unit), 18 (integration) |
| Run long-session budget tests | Tasks 3 (unit), 20 (integration) |
| Run source-link forget propagation tests | Task 19 |

**Out-of-scope items the plan deliberately defers**

- LLM-backed summary body authoring (placeholder body is deterministic and source-id-bearing today; an `LLMSummarizer` follow-up plugs into the same handler).
- Per-folder cadence override from `_policy.yaml` — config struct exists; wiring through `FolderPolicy::resolve` is a follow-up.
- REM / Deep sleep tiers (P1+).
- Temporal adapter for the orchestrator (P1+).

**Placeholder scan**

Every code-bearing step has executable code. No "TBD" / "similar to" markers. Two places call out adapter-side details that need verification at execution time:

- Task 10/14: exact pool accessor name on `SqliteMemoryStore` (`read_pool()` vs `pool()`) — must be read from the adapter before writing the helper. The implementer should confirm and adjust; not a placeholder, an awareness flag.
- Task 12: `MemoryRecord::default_for_summary()` is shorthand for the implementer to mirror the existing record-construction convention. Read `cairn-core/src/domain/record.rs` and use the actual constructor pattern.

**Type consistency**

- `TurnHeader` — same struct used by `window::pick_window` (Task 2), `compute_rolling_summary` (Task 3), `SqliteMemoryStore::list_trace_turns` (Task 10), and the handler (Task 12). Single definition in `pipeline::consolidation::window`.
- `ConsolidationPayload` — single definition in `consolidation::payload`, consumed by handler (Task 12) and trigger (Task 13).
- `JobKind` constants — `CONSOLIDATION_KIND` defined in `handler.rs` (Task 12), `FORGET_CLEANUP_KIND` defined in `forget_cleanup.rs` (Task 14); both re-exported through `consolidation::mod.rs`.
- `HandlerOutcome` variants — `Done`, `Retry { reason }`, `Permanent { reason }` consistent across worker.rs, handler.rs, forget_cleanup.rs.
- `Clock` trait — same definition used by worker, reaper, scheduler.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-12-issue-90-consolidation-workflow.md`. Two execution options:

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints

**Which approach?**
