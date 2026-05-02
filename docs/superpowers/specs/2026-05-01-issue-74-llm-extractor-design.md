# Issue #74 — LLMExtractor, Structured Draft Schema, and Fallback Behavior

**Status:** Draft (awaiting user review)
**Issue:** [#74](https://github.com/windoliver/cairn/issues/74)
**Parent:** [#12](https://github.com/windoliver/cairn/issues/12) — Ingestion pipeline (extract/filter/classify/scope)
**Brief sources:** §5.2.a `LLMExtractor`, §4 `LLMProvider` contract, §5.2 Filter discard reasons
**Depends on:** #73 (`ExtractorWorker` trait + `RegexExtractor` + `llm_eligible_spans`), #144 (`LLMProvider` adapter + JSON-mode contract tests)
**Date:** 2026-05-01

---

## 1. Goal

Implement the **`LLMExtractor`** — the P0 mainline extractor that turns a `CaptureEvent` plus its resolved body into structured `MemoryDraft`s by issuing a single schema-validated `LLMProvider.complete()` call. Pair it with a minimal **`ExtractChain`** sequencer so the regex → llm hand-off contract laid down in #73 is exercised end-to-end and the "fallback-chain tests" required by issue #74's verification list have a concrete object to test.

## 2. Scope

**In scope (this PR):**

- `LLMExtractor` struct implementing `ExtractorWorker` (in `cairn-core::pipeline::extract::llm`).
- Static prompt template + structured-output JSON schema (kind / body / confidence / entities / evidence / discards).
- Schema-validated parse path producing `Vec<ExtractOutput>` and a new `Vec<DiscardCandidate>`.
- Error policy: typed soft-fail vs hard-fail (§6 below) with one retry on `InvalidJsonOutput`.
- Wall-clock and prompt-size budgets enforced inside the extractor before delegating to the provider.
- `ExtractChain` sequencer — a `Vec<Box<dyn ExtractorWorker>>` runner that honours `llm_eligible_spans`, captures per-worker hard-fails, and merges results.
- Extension to `ExtractBudget` (`max_prompt_tokens`, `max_response_tokens`).
- Two new error variants on `ExtractError`: `Provider { worker, code, source }` and `SpanOutOfBounds { worker, span }`.
- One new public type: `DiscardCandidate { reason, source_span, evidence: String }`.
- Unit + property + chain integration tests; wiremock-backed end-to-end test using `cairn-llm-openai-compat`.

**Explicitly out of scope:**

- Configurable / user-supplied prompt templates. P0 ships one built-in template only.
- Confidence-threshold fan-out to `AgentExtractor` (#... future P2 issue).
- Wiring `ExtractChain` into the actual ingest verb / WAL path — that lives in the ingest-pipeline epic (parent #12) and is a separate issue.
- Streaming completions, tool calls, multi-modal extraction.
- Forget-intent generation. `LLMExtractor` produces `MemoryDraft`s only; `ForgetIntent`s remain regex-only (precision matters more than recall for deletion).
- New configuration surface in `.cairn/config.yaml`. Budgets carry sane defaults; config integration follows the `pipeline.extract.chain` schema in a later issue.
- Replacing the existing `ExtractBudget` shape — fields are added, not renamed; `regex_default()` keeps producing the same struct value modulo two new `None` fields.

## 3. Why this is shaped the way it is

The five load-bearing decisions, recorded so review can challenge them rather than re-derive them:

1. **Single-call, full-body prompt with `<eligible>` markers.** Brief §5.2.a budgets the LLM extractor at "~1 model call × ≤ 2 KB prompt, p95 < 400 ms". Per-span calls would multiply that by N; concatenating only span substrings drops the surrounding context the model needs to resolve entities. Marker-style prompts let the model see the full body (giving it co-reference + entity-resolution context) while a validation step rejects any draft whose `source_span` falls outside the eligible set. The validation step is what makes the marker-based approach safe — without it the model would be free to emit drafts from anywhere in the body.
2. **Tagged-union output (`drafts` + `discards`) over flat draft array.** Brief §5.2 already classes "discard" as a first-class pipeline outcome with logged reason codes. Asking the model to emit discard candidates directly, instead of only drafts, makes the model's "I considered this and chose not to memorise" reasoning observable and feeds the Filter stage with model-suggested reasons (which Filter still validates — never blindly trusts). The downstream Filter stage in a later issue can choose to surface discards in `metrics.jsonl`; this PR just lands the data path.
3. **Typed soft-fail vs hard-fail.** Issue #74 acceptance says "Extractor failures are observable and do not block the user-facing response path." Always-`Ok(empty)` (option A from brainstorming) satisfies "does not block" but loses observability — `auth_denied` would look identical to `not_configured`. Splitting by error class keeps configuration-class failures (`NotConfigured`, `BudgetExceeded`) silent-and-skipped, while infrastructure-class failures (`Unreachable`, `AuthDenied`, repeated `InvalidJsonOutput`) surface as `ExtractError::Provider` for the chain runner to record. The chain runner converts all of those into per-worker `failures` so the user-facing response is never blocked, but operators see what failed and why.
4. **`ExtractChain` is in scope; full policy-driven dispatcher is not.** Issue #74's verification line "Run fallback-chain tests" requires a chain-shaped object to test against. Without it, `llm_eligible_spans` from #73 is dead code. The minimum viable chain is a sequential runner with span-suppression honouring confidence ≥ 0.9 (the rule #73 already enforces). YAML-driven dispatch (`pipeline.extract.chain.workers`), confidence-triggered fan-out to `AgentExtractor`, and per-kind worker selection are policy concerns that belong with the config-schema or AgentExtractor issue. Keeping them out here keeps the diff under control without leaving an integration gap.
5. **Live in `cairn-core`, no new crate.** `RegexExtractor` lives in `cairn-core::pipeline::extract::regex`; `LLMExtractor` symmetrically lives in `cairn-core::pipeline::extract::llm`. The extractor only ever talks to the `LLMProvider` *trait*, never to a concrete adapter, so `cairn-core`'s zero-workspace-deps invariant (CLAUDE.md §3) holds. `cairn-llm-openai-compat` is exercised only in the integration test that lives under `crates/cairn-core/tests/` as a `dev-dependency`.

## 4. Architecture

### 4.1 File layout

```
crates/cairn-core/src/pipeline/extract/
├── llm/
│   ├── mod.rs        — LLMExtractor + ExtractorWorker impl
│   ├── prompt.rs     — render_prompt(input) -> String
│   ├── schema.rs     — pub const RESPONSE_SCHEMA: &str (lazy_static jsonschema validator)
│   ├── parse.rs      — parse_response(json, eligible) -> Result<ParsedResponse, ParseError>
│   └── tests/        — unit tests grouped by file
├── chain.rs          — ExtractChain { workers, ... }
└── mod.rs            — extended ExtractBudget; new DiscardCandidate; new ExtractError variants
```

### 4.2 `LLMExtractor`

```rust
pub struct LLMExtractor {
    provider: Arc<dyn LLMProvider>,
    budget: ExtractBudget,
}

impl LLMExtractor {
    pub fn new(provider: Arc<dyn LLMProvider>) -> Self { ... }
    pub fn with_budget(mut self, budget: ExtractBudget) -> Self { ... }
}

#[async_trait]
impl ExtractorWorker for LLMExtractor {
    fn name(&self) -> &'static str { "llm" }
    fn budget(&self) -> ExtractBudget { self.budget }
    async fn extract(&self, input: &ExtractInput<'_>) -> Result<ExtractResult, ExtractError>;
}
```

`Arc<dyn LLMProvider>` (not `Box<dyn>`): the provider is share-able across many extractor instances and across an `ExtractChain`'s lifetime, and the chain runner needs to hold workers behind `Box<dyn ExtractorWorker>` while the underlying provider is shared.

### 4.3 `ExtractChain`

```rust
pub struct ExtractChain {
    workers: Vec<Box<dyn ExtractorWorker>>,
}

#[derive(Debug)]
pub struct ChainResult {
    pub outputs: Vec<ExtractOutput>,
    pub discards: Vec<DiscardCandidate>,
    pub failures: Vec<WorkerFailure>,
    pub truncated: TruncationReason,
}

#[derive(Debug)]
pub struct WorkerFailure {
    pub worker: &'static str,
    pub error: ExtractError,
}

impl ExtractChain {
    pub fn new(workers: Vec<Box<dyn ExtractorWorker>>) -> Self;
    pub async fn run(&self, input: &ExtractInput<'_>) -> ChainResult;
}
```

Behaviour:

1. Iterate `workers` in declaration order.
2. Maintain a running `eligible_spans: Vec<TextSpan>` initialised from the input's body length (one span covering `0..body_len`).
3. Before each worker, rewrite the input's `eligible_spans` to the running set.
4. After each worker:
   - Append `result.outputs` to `ChainResult.outputs` and discards to `ChainResult.discards`.
   - Subtract spans of every output with `confidence >= CONFIDENCE_GATE_FOR_SUPPRESSION` (0.9) from the running `eligible_spans` (same rule as #73 §6.5).
   - Append the worker's `truncated` to the chain truncation reason (last non-`None` wins; this is good enough for P0 — chain-level truncation aggregation is left as a follow-up).
5. On `Err(ExtractError::*)` from a worker: do **not** abort. Append a `WorkerFailure` and move to the next worker. The user-facing response path is never blocked by an extractor error.

`ExtractChain` does **not** itself implement `ExtractorWorker` — it is a top-level orchestrator, not an extractor. Composing chains-of-chains is YAGNI for P0.

### 4.4 Extended `ExtractBudget`

```rust
pub struct ExtractBudget {
    pub max_wall_ms: u32,
    pub max_drafts: u16,
    pub max_prompt_tokens: Option<u32>,    // NEW; None = no cap (regex default)
    pub max_response_tokens: Option<u32>,  // NEW
}

impl ExtractBudget {
    pub const fn regex_default() -> Self {
        Self { max_wall_ms: MAX_PHASE_A_WALL_MS, max_drafts: 16,
               max_prompt_tokens: None, max_response_tokens: None }
    }
    pub const fn llm_default() -> Self {
        Self { max_wall_ms: 500, max_drafts: 16,
               max_prompt_tokens: Some(2000), max_response_tokens: Some(1500) }
    }
}
```

Token counts are **character-approximate** for P0 (`prompt.len() / 4` heuristic, conservative upper bound; precise tokenisation is provider-specific and lives in the adapter). Documented as such on the field. Hard upper bound.

### 4.5 New error variants

```rust
#[non_exhaustive]
pub enum ExtractError {
    // ... existing variants ...

    /// Provider returned an error the extractor cannot recover from.
    /// The chain runner captures these into `ChainResult.failures`.
    #[error("provider failure in extractor `{worker}`: {code}")]
    Provider {
        worker: &'static str,
        /// Stable error class for metrics: matches `LlmError` discriminant
        /// stringified, e.g. "unreachable", "auth_denied", "invalid_json_output".
        code: &'static str,
        #[source]
        source: LlmError,
    },

    /// Model emitted a `source_span` outside the input's `eligible_spans`.
    /// The offending item is dropped; this error is *only* raised when the
    /// model emits zero usable items. Otherwise the bad items are dropped
    /// silently with a `tracing::warn!` and a metric.
    #[error("extractor `{worker}` emitted only spans outside the eligible set")]
    SpanOutOfBounds { worker: &'static str },
}
```

### 4.6 `DiscardCandidate`

```rust
pub struct DiscardCandidate {
    pub reason: DiscardReason,
    pub source_span: TextSpan,
    pub evidence: String,    // model-emitted, ≤ 512 chars (schema-enforced)
}

#[non_exhaustive]
pub enum DiscardReason {
    Volatile,
    ToolLookup,
    CompetingSource,
    LowSalience,
    Other,
}
```

Mirrors §5.2 Filter reason codes (subset; `pii_blocked`, `policy_blocked`, `duplicate` are decided by Filter, never proposed by the model).

## 5. JSON schema and prompt

### 5.1 Output schema

The full schema lives in `schema.rs` as a string constant + lazy-built `jsonschema::Validator`. Shape (abridged — see schema.rs for the literal):

```json
{
  "type": "object",
  "required": ["items"],
  "additionalProperties": false,
  "properties": {
    "items": {
      "type": "array",
      "maxItems": 16,
      "items": {
        "oneOf": [
          {
            "type": "object",
            "required": ["type", "kind", "body", "confidence", "source_span"],
            "additionalProperties": false,
            "properties": {
              "type": { "const": "draft" },
              "kind": { "enum": [/* the 19 MemoryKind names from IDL */] },
              "body": { "type": "string", "minLength": 1, "maxLength": 4096 },
              "confidence": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
              "entities": { "type": "array", "maxItems": 32,
                            "items": { "type": "string", "maxLength": 128 } },
              "evidence": { "type": "string", "maxLength": 512 },
              "source_span": {
                "type": "object",
                "required": ["start", "end"],
                "additionalProperties": false,
                "properties": {
                  "start": { "type": "integer", "minimum": 0 },
                  "end":   { "type": "integer", "minimum": 0 }
                }
              }
            }
          },
          {
            "type": "object",
            "required": ["type", "reason", "source_span"],
            "additionalProperties": false,
            "properties": {
              "type": { "const": "discard" },
              "reason": { "enum": ["volatile", "tool_lookup", "competing_source",
                                   "low_salience", "other"] },
              "evidence": { "type": "string", "maxLength": 512 },
              "source_span": { "$ref": "#/properties/items/items/oneOf/0/properties/source_span" }
            }
          }
        ]
      }
    }
  }
}
```

The `kind` enum's variants are the IDL's `MemoryKind` set. P0 hand-lists them with a CI assertion (`#[test] fn schema_kind_enum_matches_idl()`) that compares to the generated `MemoryKind::ALL`. When codegen for schema is wired in a later issue, the assertion goes away.

### 5.2 Prompt template

Static, English-only, ≤ 600 tokens of fixed instruction overhead. Stored as `pub const PROMPT_TEMPLATE: &str` in `prompt.rs`.

```text
You are an extraction component for a personal-memory system.

For each <eligible>...</eligible> region in the conversation below, decide
whether to:
  (a) emit a memory draft (lasting fact, preference, or rule), or
  (b) emit a discard candidate (volatile, tool lookup, low-salience, etc.).

Your reply MUST be a single JSON object that matches the provided schema.
Do not include text outside the JSON. Do not invent regions; every
`source_span` you emit MUST fall inside one of the <eligible> tags.

Memory kinds: {{KIND_LIST}}.

Discard reasons: volatile, tool_lookup, competing_source, low_salience, other.

Conversation:
{{BODY_WITH_MARKERS}}
```

`{{KIND_LIST}}` is rendered from the IDL-generated `MemoryKind` enum. `{{BODY_WITH_MARKERS}}` is the body string with `<eligible>` / `</eligible>` inserted at each eligible-span boundary; UTF-8 boundaries are respected by clamping span start/end to `char_indices()`.

### 5.3 Span validation

After parse:

```text
for each item in parsed.items:
    if item.type == "draft" or item.type == "discard":
        if not any(eligible.contains(item.source_span) for eligible in input.eligible_spans):
            drop item, increment metric `llm.span_out_of_bounds`
            tracing::warn!(span = ?item.source_span, eligible = ?input.eligible_spans, ...)
```

If, after dropping, the parse yields zero usable items **and** the model emitted at least one item, return `ExtractError::SpanOutOfBounds { worker: "llm" }`. If the model emitted zero items, return `Ok(empty result)` — that's a legitimate "nothing memorable here" outcome, not a fault.

### 5.4 Empty-body and not-applicable inputs

| `input.body`                       | `LLMExtractor` returns                      |
|------------------------------------|---------------------------------------------|
| `BodyResolution::NotApplicable`    | `Ok(empty)` — no LLM call                   |
| `BodyResolution::Failed(_)`        | `Err(ExtractError::BodyResolution { .. })`  |
| `BodyResolution::Resolved` empty   | `Ok(empty)` — no LLM call                   |
| `eligible_spans` empty AND prior workers ran | `Ok(empty)` — high-confidence regex covered everything |
| `eligible_spans` empty AND first worker | full body treated as one eligible span (chain initialiser sets this) |

## 6. Error handling and fallback policy

| Underlying `LlmError`            | Internal handling             | Returned to chain                                         |
|----------------------------------|-------------------------------|-----------------------------------------------------------|
| `NotConfigured`                  | `tracing::info` + metric      | `Ok(empty, truncated = None)`                             |
| `BudgetExceeded`                 | `tracing::info` + metric      | `Ok(empty, truncated = MaxWallMs { elapsed_ms })`         |
| `InvalidJsonOutput` (1st)        | retry once with reinforcing prompt | (continue)                                           |
| `InvalidJsonOutput` (2nd)        | give up                       | `Err(Provider { code: "invalid_json_output", .. })`       |
| `Unreachable`                    | no retry                      | `Err(Provider { code: "unreachable", .. })`               |
| `AuthDenied`                     | no retry                      | `Err(Provider { code: "auth_denied", .. })`               |
| `CapabilityMissing`              | no retry                      | `Err(Provider { code: "capability_missing", .. })`        |
| Wall-clock timeout in extractor  | cancel future                 | `Ok(empty, truncated = MaxWallMs { elapsed_ms })`         |
| Prompt too large pre-flight      | reject before call            | `Ok(empty, truncated = MaxWallMs { elapsed_ms: 0 })` *and* `tracing::warn` (no `Err`, since this is config-class) |

The reinforcing-retry suffix is a single sentence appended to the prompt:
`"Your previous response was not valid JSON matching the required schema. Reply with valid JSON only — no prose, no code fences."`

`ExtractChain::run` converts every `Err(_)` into a `WorkerFailure` and continues; the user-facing response path never observes an `Err` from the chain.

## 7. Capability advertisement

`LLMExtractor::new` returns the extractor regardless of provider configuration — the *configurability* is decided at runtime by the provider itself returning `LlmError::NotConfigured`. This matches CLAUDE.md invariant 6 ("fail closed on capability") only superficially; the *real* fail-closed gate happens at the verb-status layer in a later issue (the verb has to declare whether `llm` is available based on `provider.capabilities()`). For now, the extractor's contract is "calls provider; degrades to empty on `NotConfigured`," which is exactly what the brief §3.1 example config (`worker: llm ... runs iff an LLMProvider is configured; skipped with llm.not_configured otherwise`) describes.

## 8. Testing strategy

Tests are written before implementation, per CLAUDE.md §7 ("Write tests first").

### 8.1 Unit tests

| File                         | Test                                                         | Tooling     |
|------------------------------|--------------------------------------------------------------|-------------|
| `llm/schema.rs`              | valid response passes schema                                 | `jsonschema`|
| `llm/schema.rs`              | each schema violation (missing field, wrong type, oneOf miss, maxItems exceeded) is rejected | rstest |
| `llm/schema.rs`              | schema's `kind` enum matches `MemoryKind::ALL`               | direct      |
| `llm/parse.rs`               | round-trips a draft + discard mixed payload                  | direct      |
| `llm/parse.rs`               | drops items whose `source_span` falls outside `eligible_spans` | rstest    |
| `llm/parse.rs`               | returns `SpanOutOfBounds` only when *all* items are out of bounds | rstest |
| `llm/parse.rs`               | proptest: any payload that survives schema validation parses without panic | proptest |
| `llm/prompt.rs`              | eligible markers placed at correct char boundaries, including multi-byte UTF-8 | rstest |
| `llm/prompt.rs`              | snapshot of rendered prompt for a fixture body                | insta      |
| `llm/mod.rs`                 | `NotConfigured` → `Ok(empty)`                                | StubLlm    |
| `llm/mod.rs`                 | `BudgetExceeded` → `Ok(empty, truncated = MaxWallMs)`        | StubLlm    |
| `llm/mod.rs`                 | `InvalidJsonOutput` retried once, then `Provider`            | StubLlm sequence |
| `llm/mod.rs`                 | `Unreachable` returns `Provider` immediately                 | StubLlm    |
| `llm/mod.rs`                 | wall-clock timeout via `tokio::time::timeout` — `Ok(empty, MaxWallMs)` | tokio::time::pause |
| `llm/mod.rs`                 | empty body → `Ok(empty)` without calling provider            | StubLlm with assertion |
| `llm/mod.rs`                 | `BodyResolution::Failed` → `BodyResolution` error            | direct     |
| `llm/mod.rs`                 | discard candidates surface in `ExtractResult.discards`       | StubLlm    |
| `chain.rs`                   | regex → llm chain: regex high-conf draft narrows llm's eligible spans | rstest |
| `chain.rs`                   | provider hard-fail captured in `failures`, regex output preserved | StubLlm |
| `chain.rs`                   | both workers `NotConfigured` / no-op → empty result, no failures | StubLlm |
| `chain.rs`                   | empty `workers` is allowed and returns empty                 | direct     |

### 8.2 Integration tests

`crates/cairn-core/tests/llm_extractor_e2e.rs` — uses `cairn-llm-openai-compat` as a `dev-dependency` plus `wiremock` to stub an OpenAI-compatible HTTP server. Verifies:

- Round trip from `ExtractInput` → HTTP POST → parsed `ExtractOutput`.
- Schema sent in `response_format.json_schema` matches `RESPONSE_SCHEMA`.
- 401 from wiremock surfaces as `Provider { code: "auth_denied" }`.
- Connection refused surfaces as `Provider { code: "unreachable" }`.
- Two consecutive 200s with mangled JSON surface as `Provider { code: "invalid_json_output" }` (verifies retry path).

### 8.3 Documentation tests

`LLMExtractor::new`, `LLMExtractor::with_budget`, and `ExtractChain::new` carry `rust,no_run` doctests showing typical wiring.

## 9. Migration / compatibility

- `ExtractBudget` gains two `Option` fields. All in-tree constructors are updated (`regex_default`, the test fixtures in `pipeline::extract::mod_tests`, and the `RegexExtractor` constructor). Public, defaulted-`None` fields keep external constructors that use `..Default::default()` working — but `ExtractBudget` does not currently derive `Default`, so this is in-tree-only and listed in the PR as a touched-public-API entry.
- `ExtractError` gains two `#[non_exhaustive]` variants. Marked `#[non_exhaustive]` already, so additive.
- No changes to `MemoryDraft`, `ForgetIntent`, `CaptureEvent`, or any envelope type.
- No DB schema or WAL change.

## 10. Open follow-ups (filed as issues, not done here)

- **Config integration.** `pipeline.extract.chain.workers: [...]` — wire `LLMExtractor` into the YAML schema and let `cairn ingest` read it.
- **Verb-level capability advertisement.** Have `cairn status` declare `llm` mode based on `provider.capabilities()`.
- **`AgentExtractor` (§5.2.a P2).** Confidence-triggered fan-out, signed-envelope `cairn` CLI shell-out.
- **Schema-from-IDL codegen.** Replace the hand-listed `kind` enum with a generated constant; remove the `schema_kind_enum_matches_idl` assertion.
- **Provider tokenizer hook.** Replace the `len()/4` heuristic with `LLMProvider::estimate_tokens`.
- **Streaming + tool calls.** Wait until a real use case demands them.

## 11. Verification checklist before PR

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh    # MUST pass — no new workspace deps in cairn-core
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
```

A doc-only update to `docs/design/traceability.md` adds the §5.2.a `LLMExtractor` row pointing at this issue.
