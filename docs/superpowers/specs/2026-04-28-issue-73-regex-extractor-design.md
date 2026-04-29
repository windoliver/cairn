# Issue #73 — RegexExtractor and Explicit Trigger Extraction

**Status:** Draft (awaiting user review)
**Issue:** [#73](https://github.com/windoliver/cairn/issues/73)
**Parent:** [#12](https://github.com/windoliver/cairn/issues/12) — Ingestion pipeline and extract/filter/classify/scope stages
**Brief sources:** §5.2.a ExtractorWorker, §11.6 Capture triggers, §18.a Progressive adoption
**Date:** 2026-04-28

---

## 1. Goal

Implement the **Extract** stage's regex/state-machine path: pattern-match a `CaptureEvent` against pre-compiled rules and emit either `MemoryDraft`s (capture) or `ForgetIntent`s (deletion). This is the always-on, zero-LLM extractor that fires before any LLM-backed extractor in the chain (§5.2.a) and that also serves as last-resort fallback when LLM extractors return `CapabilityUnavailable`.

This PR also lands the foundational types — `MemoryDraft`, `ExtractorWorker` trait, `ExtractBudget`, `ExtractOutput`, `ExtractError` — that issue #74 (`LLMExtractor`) takes as a hard dependency.

## 2. Scope

In scope:

- `MemoryDraft` domain type with provenance + audit fields.
- `ForgetIntent` domain type for forget-trigger output.
- `ExtractInput<'a>` (envelope reference + caller-resolved body slice; preserves the existing `payload_ref` / `payload_hash` privacy boundary).
- `ExtractorWorker` trait + `ExtractBudget`, `ExtractOutput`, `ExtractResult`, `TruncationReason`, `ExtractError`.
- `RegexRule` enum with four variants: `TriggerPhrase`, `ForgetPhrase`, `HookEvent`, `ToolFrame`.
- `RegexExtractor` struct implementing `ExtractorWorker`.
- Built-in default rule set covering §11.6 + §18.a triggers and the most common hook / tool-frame events.
- Schema-validated user-rule extension hook (`RuleSet::from_config`) without binding to `.cairn/config.yaml` yet.
- Per-rule wall-clock budget enforcement and hard `max_drafts` cap with a typed `TruncationReason` returned to the caller (§6).
- Unit, integration, property, and a CI-friendly latency assertion test.

Explicitly **not** modified by this PR:

- `CapturePayload`, `CaptureEvent`, `payload_ref`, `payload_hash` — the capture envelope shape is untouched. Raw user text continues to live behind `payload_ref` and is read by the caller, not duplicated into the envelope. The earlier round of this design proposed inlining a `Hook.body` field; review found that that widened the sensitive-envelope shape and created a second copy of raw prompt bytes outside the existing storage boundary, so it has been withdrawn in favor of `ExtractInput.body`.

Deferred:

- `LLMExtractor` (#74).
- The chain dispatcher that strings `regex → llm → agent` together (#74 wires it).
- Filter / Classify / Scope downstream consumption (#75 proves the consumer side).
- Wiring `RuleSet::from_config` into the actual `.cairn/config.yaml` loader (config-wiring PR).
- Built-in rules for `Voice`, `Screen`, `Clipboard`, `Ide` payloads (track with the relevant sensor PRs).
- `criterion` benchmark for hot-path measurement; CI gets a wall-clock assertion, the full benchmark is a follow-up issue.
- `entities` / `evidence` fields on `MemoryDraft` — added by #74, where the LLM extractor produces them.

Out of scope (per issue):

- Agent-mode extraction (P2).

## 3. Module layout

New module under `crates/cairn-core/src/pipeline/`:

```
pipeline/
├── mod.rs              # add `pub mod extract;`
├── filter/             # unchanged
└── extract/
    ├── mod.rs          # ExtractorWorker, ExtractInput, ExtractBudget, ExtractError, ExtractOutput, ExtractResult, TruncationReason
    ├── draft.rs        # MemoryDraft + Confidence + KindHint + TextSpan
    ├── intent.rs       # ForgetIntent
    ├── regex/
    │   ├── mod.rs      # RegexExtractor struct + impl ExtractorWorker
    │   ├── rule.rs     # RegexRule enum + RuleSet (compiled) + UserRuleConfig
    │   ├── defaults.rs # Built-in rule set (§11.6 + §18.a triggers, hook/tool-frame rules)
    │   └── dispatch.rs # Per-CapturePayload-family dispatch
    └── config.rs       # serde-derived user rule schema (no I/O)
```

## 4. Core types

### 4.1 `ExtractorWorker` trait

```rust
// pipeline/extract/mod.rs
use crate::domain::CaptureEvent;

/// Resolved input for extraction.
///
/// The caller (the chain dispatcher in #74, ultimately the verb layer) is
/// responsible for materializing `body` from the envelope's `payload_ref`
/// after verifying `payload_hash`. This keeps raw user text outside
/// `CaptureEvent` itself — the envelope remains metadata-only and the
/// privacy/replay boundary documented in `domain/capture.rs:362-377` is
/// preserved (no inline raw prompts in serialized fixtures).
///
/// `body` is `None` for events whose source family does not carry an
/// extractable text body (e.g., `Voice`, `Screen`, `Clipboard` — handled
/// by sensor-specific extractors in later issues) or whose payload is
/// non-textual.
pub struct ExtractInput<'a> {
    pub event: &'a CaptureEvent,
    pub body: Option<&'a str>,
}

pub trait ExtractorWorker: Send + Sync {
    fn name(&self) -> &'static str;
    fn budget(&self) -> ExtractBudget;
    async fn extract(
        &self,
        input: &ExtractInput<'_>,
    ) -> Result<Vec<ExtractOutput>, ExtractError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtractBudget {
    pub max_wall_ms: u32,
    pub max_drafts: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExtractOutput {
    Draft(MemoryDraft),
    Forget(ForgetIntent),
}

#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum ExtractError {
    #[error("extractor `{worker}` exceeded budget after {elapsed_ms} ms")]
    BudgetExceeded { worker: &'static str, elapsed_ms: u32 },
    #[error("invalid rule `{rule_id}`: {reason}")]
    InvalidRule { rule_id: String, reason: String },
}
```

Note: edition-2024 native `async fn` in traits + RPITIT (CLAUDE.md §6.3). Use `dyn ExtractorWorker` only at the chain boundary (#74), where boxed-future erasure is fine.

### 4.2 `MemoryDraft`

```rust
// pipeline/extract/draft.rs
use crate::domain::CaptureEventId;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemoryDraft {
    pub kind_hint: KindHint,
    pub body: String,
    pub confidence: Confidence,
    pub source_event: CaptureEventId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_span: Option<TextSpan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextSpan { pub start: u32, pub end: u32 } // byte offsets

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KindHint(String); // validated against §6 IDL kind list at construction

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Confidence(f32); // [0.0, 1.0]
```

`KindHint` wraps the existing `crate::domain::taxonomy::MemoryKind` (re-exported as a newtype to keep the extractor's vocabulary explicit and to leave room for a future "unknown / fallback" hint that doesn't map to a real `MemoryKind`). `Confidence` is a newtype with a `TryFrom<f32>` returning `Err` outside `[0.0, 1.0]`. Both `Debug` are hand-rolled to never leak an inner string into a body.

### 4.3 `ForgetIntent`

```rust
// pipeline/extract/intent.rs
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ForgetIntent {
    pub target_text: String,
    pub source_event: CaptureEventId,
    pub trigger_id: String,
}
```

`ForgetIntent` is a hint, not a verb invocation; the Filter/forget-routing logic in #75 + the `forget` verb own actually deleting records.

## 5. Rule shape

```rust
// pipeline/extract/regex/rule.rs
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RegexRule {
    TriggerPhrase {
        id: String,
        pattern: String,
        kind_hint: KindHint,
        confidence: Confidence,
        #[serde(default)]
        capture_group: Option<u8>,
    },
    ForgetPhrase {
        id: String,
        pattern: String,
        target_group: u8,
    },
    HookEvent {
        id: String,
        hook_name: String,
        #[serde(default)]
        tool_name: Option<String>,
        kind_hint: KindHint,
        confidence: Confidence,
    },
    ToolFrame {
        id: String,
        family: ToolFrameFamily,
        kind_hint: KindHint,
        confidence: Confidence,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolFrameFamily {
    Terminal { exit_code_nonzero: bool },
    Ide { event_kind: String }, // "diagnostic" | "test" | "lsp" | ...
}
```

The compiled form (`CompiledRule`) wraps each variant with a pre-built `regex::Regex` (where applicable) and is bucketed inside `RuleSet`:

```rust
pub struct RuleSet {
    text_rules: Vec<CompiledRule>,    // TriggerPhrase + ForgetPhrase
    hook_rules: Vec<CompiledRule>,    // HookEvent
    tool_frame_rules: Vec<CompiledRule>,
}

impl RuleSet {
    pub fn builtin() -> Self { /* defaults.rs */ }
    pub fn from_config(rules: Vec<RegexRule>) -> Result<Self, ExtractError> { /* validate + compile */ }
    pub fn merged(builtin: Self, user: Self) -> Self { /* user appends */ }
}
```

## 6. Dispatch

`RegexExtractor::extract` reads `input.event.payload` for the variant discriminator and `input.body` for any user text. **`CapturePayload` is not modified.** Raw text remains behind `payload_ref` + `payload_hash`; the caller materializes it (verifying the hash) and passes it as `ExtractInput.body`. This keeps the privacy boundary documented in `crates/cairn-core/src/domain/capture.rs:362-377` intact and avoids duplicating raw prompt text into a second serialized location.

| `CapturePayload` variant | Rule families consulted | Source of body for `text_rules` |
|---|---|---|
| `Hook` | `hook_rules` (always); `text_rules` if `input.body.is_some()` | `input.body` (caller resolves from `payload_ref` for hooks that carry a user utterance, e.g. `UserPromptSubmit`; `None` otherwise) |
| `Terminal` | `tool_frame_rules` filtered to `Terminal` | n/a |
| `Ide` | `tool_frame_rules` filtered to `Ide` | n/a |
| `Cli`, `Mcp` | `text_rules` if `input.body.is_some()` | `input.body` (caller resolves the user-supplied ingest payload from `payload_ref`) |
| `Proactive` | `text_rules` if `input.body.is_some()` | `input.body` (caller may pass either the agent's message body or `rationale` extract; both are textual) |
| `Voice`, `Screen`, `Clipboard`, `RecordingBatch` | none in this PR — extender lands with the relevant sensor issue | n/a |

For each matching rule, the extractor pushes a `Draft(MemoryDraft)` (or `Forget(ForgetIntent)`) onto the result vector. The first hit on a rule wins per event-rule pair — no multiple drafts per single rule per event.

**`max_drafts` enforcement (hard cap).** Checked inside the per-rule loop, after pushing each output. When `outputs.len() >= budget.max_drafts`:

1. Stop scanning further rules immediately (do not finish the current family).
2. Emit exactly one `tracing::warn!(worker = "regex", event_id, max_drafts, "regex extractor reached max_drafts cap")` — never the body.
3. Set `truncated = true` on the returned envelope (see below) and return `Ok(...)`.

**`max_wall_ms` enforcement (per-rule check).** `Instant::now()` is captured at extract entry. Elapsed time is checked **inside the per-rule loop**, immediately after each rule evaluation completes — *not* only at family boundaries. When elapsed exceeds `budget.max_wall_ms`:

- if zero outputs have been produced, return `Err(BudgetExceeded { worker, elapsed_ms })` so the chain (#74) falls through to the next extractor;
- otherwise stop scanning, `tracing::warn!` with elapsed time, set `truncated = true`, and return `Ok(...)`.

The per-rule check ensures a large user-rule list (which all lands in a single `text_rules` bucket) cannot blow the budget unchecked. Cost of the check: a single `Instant::now()` per rule, ~10 ns on x86_64 — negligible against any non-trivial regex match.

**Truncation envelope.** To make budget-driven truncation visible to downstream stages (so the chain dispatcher in #74 can decide whether to invoke the LLM extractor for the remainder), `extract` returns a small struct, not a bare `Vec`:

```rust
pub struct ExtractResult {
    pub outputs: Vec<ExtractOutput>,
    pub truncated: TruncationReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TruncationReason {
    None,
    MaxDrafts,
    MaxWallMs { elapsed_ms: u32 },
}
```

The trait signature becomes `async fn extract(...) -> Result<ExtractResult, ExtractError>`. Downstream stages can branch on `result.truncated` without re-running budget arithmetic. `ExtractError::BudgetExceeded` is reserved for the **zero-output** wall-clock case (chain falls through); over-cap or wall-clock-with-partial cases are `Ok` with `truncated` set.

Determinism: rules are dispatched in a stable order — built-in rules first (declaration order in `defaults.rs`), then user rules in `from_config` declaration order. Two identical events therefore produce identical truncated sets when the cap is hit.

Rationale on the `Ok` vs `Err` split: brief §5.2.a says "exceeding `budget` returns `ExtractBudgetExceeded`, falls through to next extractor." We honor that for the all-or-nothing case (zero outputs → fall through). For partial success we surface truncation as data, not error, so the chain has full information without exception-style control flow.

## 7. Default rule set (`defaults.rs`)

| Rule id | Variant | Pattern (case-insensitive) | Kind hint | Confidence |
|---|---|---|---|---|
| `remember.preference` | TriggerPhrase | `^\s*remember\s+(?:that\s+)?(.+?)\s*$` | `user` | 0.95 |
| `remember.rule` | TriggerPhrase | `^\s*remember:?\s*never\s+(.+?)\s*$` | `rule` | 0.95 |
| `correction` | TriggerPhrase | `^\s*correction:?\s*(.+?)\s*$` | `feedback` | 0.95 |
| `success.recipe` | TriggerPhrase | `this is how we did it\s*[—-]\s*it worked` | `strategy_success` | 0.85 |
| `skillify` | TriggerPhrase | `\bskillify\s+(?:this|it)\b` | `playbook` | 0.95 |
| `forget` | ForgetPhrase | `^\s*forget\s+(?:that\s+|what\s+)?(.+?)\s*$` | n/a (target = group 1) | n/a |
| `hook.post_tool_use` | HookEvent | hook=`PostToolUse` | `trace` | 0.8 |
| `hook.stop` | HookEvent | hook=`Stop` | `trace` | 0.7 |
| `hook.pre_compact` | HookEvent | hook=`PreCompact` | `trace` | 0.7 |
| `tool.terminal_failure` | ToolFrame | Terminal, exit_code_nonzero=true | `strategy_failure` | 0.7 |

All `TriggerPhrase` patterns are anchored or right-bounded, use bounded quantifiers, and are constructed with `regex::RegexBuilder::case_insensitive(true)` to keep matching linear and below the latency budget. Patterns are **conservative** — false negatives are fine (LLM extractor in #74 catches them); false positives at high confidence are not (they cause unwanted writes).

User-supplied rules are appended after defaults via `merged(builtin, user)`. Built-in rule ids never overlap with user ids; `from_config` rejects duplicate ids with `InvalidRule`.

## 8. Errors

`thiserror`-backed `ExtractError`. `BudgetExceeded` from extract (only when zero drafts produced — see §6). `InvalidRule` from rule-set construction (compile failure, duplicate id, unknown kind hint, confidence out of range).

No `unwrap()` / `expect()` in `cairn-core` (CLAUDE.md §6.2). All regex `RegexBuilder::build()` errors map to `InvalidRule`.

## 9. Observability

- `#[tracing::instrument(skip(self, event), fields(worker = "regex", event_id = %event.event_id, payload_family = ?event.payload.family()))]` on `extract`.
- Per-rule fires: `tracing::trace!(rule_id, ..)`. **Never** include `body` text or capture text above `trace` (CLAUDE.md §6.6, brief §14).
- Counters land in `.cairn/metrics.jsonl` via the broader pipeline metrics emitter — out of scope here; this PR's emission is `tracing` only.

## 10. Testing

### 10.1 Unit tests (`pipeline/extract/regex/{mod,rule,defaults}.rs`)

- `KindHint::new` rejects unknown kinds.
- `Confidence::new` rejects values outside `[0,1]`.
- Each built-in rule: ≥1 positive case, ≥1 negative case.
- `forget` rule emits `Forget(ForgetIntent)`, not `Draft`.
- Compile failure path: `RuleSet::from_config(<rule with bad pattern>)` → `InvalidRule`.
- Duplicate ids rejected.
- `RegexExtractor::name() == "regex"`, default budget = `{max_wall_ms: 2, max_drafts: 16}`.
- `max_drafts` enforcement: synthesize a `RuleSet` whose user-rule list contains `max_drafts + 5` always-matching rules; assert `extract` returns exactly `max_drafts` outputs, `truncated == TruncationReason::MaxDrafts`, and emits exactly one `tracing::warn!` (captured via `tracing-test`). Truncation order matches rule declaration order.
- `max_wall_ms` zero-output path: stub a sleeping rule (or set `budget.max_wall_ms = 0`); assert `extract` returns `Err(BudgetExceeded { worker: "regex", elapsed_ms })`.
- `max_wall_ms` partial-output path: feed a body that matches one early rule, then trip the wall-clock budget; assert `truncated == TruncationReason::MaxWallMs { .. }` and `outputs.len() == 1`.
- `ExtractInput.body == None` path: pass a `Cli` envelope with no body; assert text rules don't match and the result is empty (not an error).

### 10.2 Integration tests (`crates/cairn-core/tests/pipeline_extract_regex.rs`)

- Synthesize one `CaptureEvent` per `CapturePayload` variant via `cairn-test-fixtures`. Construct each `ExtractInput` with the appropriate `body` (caller-resolved text for `Hook` / `Cli` / `Mcp` / `Proactive`; `None` for the others).
- Run `RegexExtractor::builtin().extract(&input).await` and assert expected `ExtractOutput`s.
- `Snapshot (insta)` test on serialized `ExtractOutput` for stable rule ids — guards against accidental rule-id renames breaking downstream issue #75.
- Empty-fallthrough: `Cli` event, body `"hello world"`. Assert `outputs.is_empty()` and `truncated == None`. The chain wiring that would forward this to `LLMExtractor` lives in #74; this PR only verifies the empty result.

### 10.3 Property tests (`proptest`)

- `prop_random_text_no_panic`: random ASCII strings up to 4 KB → `extract` never panics, never exceeds `budget.max_drafts`.
- `prop_serde_round_trip`: arbitrary `MemoryDraft` / `ExtractOutput` round-trip through JSON.

### 10.4 Latency assertion (`crates/cairn-core/tests/pipeline_extract_regex_latency.rs`)

- Build a 10 000-event mixed fixture (mix of `Cli`, `Hook`, `Terminal`).
- Warm: run once to populate any internal state.
- Measure: `Instant`-based per-event, collect, assert p99 < 2 ms on the test runner.
- Tolerance: CI runners are noisy → mark `#[ignore]` by default; `cargo nextest run -- --include-ignored` runs locally and on a perf-tagged CI job. Track full `criterion` benchmark as a follow-up issue.

### 10.5 Acceptance criteria mapping

| Acceptance criterion (#73) | Test |
|---|---|
| Explicit remember/forget requests produce correct draft or forget intent | §10.2 fixture per trigger rule |
| Regex extraction stays under p99 latency budget | §10.4 latency assertion |
| Low-confidence/unmatched events fall through to LLM extractor chain | §10.2 empty-fallthrough; chain wiring proven in #74 |

## 11. Brief deviations

1. **Trait input.** §5.2.a writes `extract(event: &CaptureEvent) -> Vec<MemoryDraft>`. We widen the input to `&ExtractInput<'_>` so the caller can supply the resolved-body slice without expanding the `CaptureEvent` envelope. The envelope continues to carry `payload_ref` + `payload_hash`; the body is read once by the chain dispatcher and threaded into `ExtractInput` for every worker in the chain. Non-breaking — the trait is brand-new.
2. **Trait return type.** §5.2.a writes `Vec<MemoryDraft>`. We return `Result<ExtractResult, ExtractError>` so the trait can carry (a) `ForgetIntent` outputs alongside drafts via `ExtractOutput`, and (b) typed truncation reasons (`max_drafts` / `max_wall_ms`) the chain dispatcher uses to decide whether to invoke the LLM extractor for the remainder. `BudgetExceeded` is reserved for the zero-output wall-clock case.
3. **`MemoryDraft` fields.** Brief §5.2.a lists `{kind, body, entities, confidence}` plus #74's `{evidence, discard candidates}`. Regex output cannot produce `entities` / `evidence` reliably, so this PR ships `kind_hint`, `body`, `confidence`, `source_event`, `source_span`, `trigger_id`. #74 will add `entities` / `evidence` as `Option`-wrapped fields when the LLM extractor populates them.

## 12. Workspace impact

- Add `regex = { version = "1", default-features = false, features = ["std", "perf"] }` to `[workspace.dependencies]`. Disabling the `unicode` features keeps the binary slim; add Unicode-aware features only when a built-in rule needs them. CLAUDE.md §6.7 — workspace dep, justified in PR.
- `cairn-core` opts in to `regex = { workspace = true }`.
- `cargo deny` allowlist already covers MIT/Apache-2.0; `regex` is dual-licensed as both. No `deny.toml` change.
- Workspace-lint compliance: no new `#[allow]`s; no `unsafe`; `#![forbid(unsafe_code)]` already applies.
- Core boundary check (`scripts/check-core-boundary.sh`): unaffected — `regex` is an external crate, not a workspace crate.

## 13. Verification checklist (CLAUDE.md §8)

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

No `cairn-docgen` impact (no CLI flag, no MCP metadata change).

## 14. Open follow-ups (file as separate issues)

1. Add `criterion` benchmark for hot-path measurement; replace the `#[ignore]`-d test in §10.4.
2. Add built-in rules for `Voice`, `Screen`, `Clipboard`, `Ide` payloads as those sensors land.
3. Wire `RuleSet::from_config` into `.cairn/config.yaml` schema.
