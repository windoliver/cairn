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
- `ExtractorWorker` trait + `ExtractBudget`, `ExtractOutput`, `ExtractError`.
- `RegexRule` enum with four variants: `TriggerPhrase`, `ForgetPhrase`, `HookEvent`, `ToolFrame`.
- `RegexExtractor` struct implementing `ExtractorWorker`.
- Built-in default rule set covering §11.6 + §18.a triggers and the most common hook / tool-frame events.
- Schema-validated user-rule extension hook (`RuleSet::from_config`) without binding to `.cairn/config.yaml` yet.
- Hard `max_drafts` enforcement in the dispatch loop with deterministic truncation semantics (§6).
- Extension to `CapturePayload::Hook` adding an optional `body` field so primary-chat-path (Mode A) `remember` / `forget` / `skillify` triggers are extractable in this PR (§6.1).
- Unit, integration, property, and a CI-friendly latency assertion test.

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
    ├── mod.rs          # ExtractorWorker, ExtractBudget, ExtractError, ExtractOutput
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

pub trait ExtractorWorker: Send + Sync {
    fn name(&self) -> &'static str;
    fn budget(&self) -> ExtractBudget;
    async fn extract(
        &self,
        event: &CaptureEvent,
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

`RegexExtractor::extract` matches on `event.payload`:

| `CapturePayload` variant | Rule families consulted |
|---|---|
| `Hook` | `hook_rules` (always); `text_rules` against `body` if present (see §6.1 for the schema extension) |
| `Terminal` | `tool_frame_rules` filtered to `ToolFrameFamily::Terminal` |
| `Ide` | `tool_frame_rules` filtered to `ToolFrameFamily::Ide` |
| `Cli`, `Mcp` | `text_rules` against the `kind_hint` field (Mode B body is the user-supplied ingest payload) |
| `Proactive` | `text_rules` against the `rationale` field |
| `Voice`, `Screen`, `Clipboard`, `RecordingBatch` | none in this PR — extender lands with the relevant sensor issue |

For each matching rule, the extractor pushes a `Draft(MemoryDraft)` (or `Forget(ForgetIntent)`) onto the result vector. The first hit on a rule wins per event-rule pair — no multiple drafts per single rule per event.

**`max_drafts` enforcement (hard cap).** The dispatch loop checks `outputs.len() >= budget.max_drafts` after pushing each output. When the cap is reached the loop:

1. Stops scanning further rules.
2. Emits exactly one `tracing::warn!(worker = "regex", event_id, max_drafts, "regex extractor reached max_drafts cap")` — never the body.
3. Returns `Ok(outputs)` with the truncated set; the cap itself is success, not error. Rationale: the cap exists to bound work, not to gate correctness. Returning a partial set keeps the contract simple for the chain (#74) and matches how `max_wall_ms` is handled below. A user-rule explosion that triggers the cap is loud (warn log + metrics counter) but cannot stall or error the extract path.

Determinism: rules are dispatched in a stable order — built-in rules first (declaration order in `defaults.rs`), then user rules in `from_config` declaration order. Two identical events therefore produce identical truncated sets when the cap is hit.

**`max_wall_ms` enforcement.** A single `Instant::now()` snapshot is taken before the dispatch loop, and the elapsed time is checked once per rule-family boundary (3 checks per event maximum). If elapsed exceeds `budget.max_wall_ms`:

- if zero outputs have been produced, return `Err(BudgetExceeded)` so the chain (#74) can fall through to the next extractor;
- otherwise stop scanning, `tracing::warn!` with the elapsed time, and return `Ok(outputs)`.

Rationale: brief §5.2.a says "exceeding budget returns `ExtractBudgetExceeded`, falls through to next extractor." For regex we treat partial success as success (the only fallthrough beyond regex is no extraction at all, since regex *is* the fallback layer). This matches the brief's "RegexExtractor fallback chain still captures hook events + 'tell it directly' triggers" guarantee (§intro).

### 6.1 `CapturePayload::Hook` body extension

To make Mode A trigger phrases ("remember…", "forget…", "skillify…" typed in chat) extractable, this PR extends `CapturePayload::Hook` with an optional body field:

```rust
Hook {
    hook_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_name: Option<String>,
    /// Captured user-message body when the hook is `UserPromptSubmit` /
    /// `SessionStart` / similar harness hooks that carry a literal user
    /// utterance. Sensitive — never logged above `trace`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    body: Option<String>,
},
```

Backwards-compatible: `#[serde(default)]` keeps existing wire payloads parsing. The `Debug` impl already redacts `Hook` fields (`crates/cairn-core/src/domain/capture.rs:475`); the new field follows the same redaction. Sensors that don't carry a body (e.g., `PostToolUse`) leave the field `None`; sensors that do (e.g., `UserPromptSubmit`) populate it.

Test impact: existing `tests/capture_event.rs` round-trip tests must stay green; one new round-trip case covers the populated-body variant. No migration / WAL / store change — `CaptureEvent` is captured-time, not stored-form.

This extension lands in **this PR**, not a follow-up: without it the high-priority acceptance criterion ("explicit remember/forget requests produce correct draft or forget intent") cannot be met for the primary chat path.

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
- `max_drafts` enforcement: synthesize a `RuleSet` whose user-rule list contains `max_drafts + 5` always-matching rules; assert `extract` returns exactly `max_drafts` outputs and emits exactly one `tracing::warn!` (captured via `tracing-test`). Truncation order matches rule declaration order.

### 10.2 Integration tests (`crates/cairn-core/tests/pipeline_extract_regex.rs`)

- Synthesize one `CaptureEvent` per `CapturePayload` variant via `cairn-test-fixtures`.
- Run `RegexExtractor::builtin().extract(&event).await` and assert expected `ExtractOutput`s.
- `Snapshot (insta)` test on serialized `ExtractOutput` for stable rule ids — guards against accidental rule-id renames breaking downstream issue #75.
- Empty-fallthrough: synthesize a `Cli` event with body `"hello world"` (no trigger). Assert `Vec::new()` returned. The chain wiring that would forward this to `LLMExtractor` lives in #74; this PR only verifies the empty result.

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

1. **Trait return type.** §5.2.a writes `extract(...) -> Vec<MemoryDraft>`. The acceptance criterion of #73 says drafts **or** forget intents must be produced. We widen to `Result<Vec<ExtractOutput>, ExtractError>` for both reasons (forget routing + budget surfacing). Non-breaking — the trait is brand-new in this PR.
2. **`MemoryDraft` fields.** Brief §5.2.a lists `{kind, body, entities, confidence}` plus #74's `{evidence, discard candidates}`. Regex output cannot produce `entities` / `evidence` reliably, so this PR ships `kind_hint`, `body`, `confidence`, `source_event`, `source_span`, `trigger_id`. #74 will add `entities` / `evidence` as `Option`-wrapped fields when the LLM extractor populates them.
3. **Budget partial-success.** Brief §5.2.a: "exceeding `budget` returns `ExtractBudgetExceeded`, falls through to next extractor." We treat partial regex output as success (§6) since regex *is* the fallback layer — there is no extractor below it. Documented inline in `extract`.

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
