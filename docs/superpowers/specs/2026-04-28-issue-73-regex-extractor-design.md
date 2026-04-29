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
- `ForgetIntent` domain type — structured selector (normalized text + match strategy + optional kind filter + source span), not free-form text. Documents the resolver contract #75 must obey to avoid over- or under-deletion.
- `ExtractInput<'a>` with `BodyResolution<'a>` (a 3-way enum: `NotApplicable` | `Resolved(&str)` | `Failed(BodyResolutionError)`) so payload-load failures cannot masquerade as "no body" and silently drop explicit intents.
- `ExtractorWorker` trait + `ExtractBudget`, `ExtractOutput`, `ExtractResult`, `TruncationReason`, `ExtractError` (including `BodyResolution` and `BudgetExceeded` variants).
- `RegexRule` enum with four variants: `TriggerPhrase`, `ForgetPhrase`, `HookEvent`, `ToolFrame`.
- `RegexExtractor` struct implementing `ExtractorWorker`.
- Built-in default rule set covering §11.6 + §18.a triggers and the most common hook / tool-frame events.
- Schema-validated user-rule extension hook (`RuleSet::from_config`) without binding to `.cairn/config.yaml` yet.
- Two-phase dispatch with **untruncatable built-in rules** (Phase A) and budget-bounded user rules (Phase B). Built-ins cover all §11.6 / §18.a explicit triggers, so truncation cannot silently lose `remember` / `forget` / `skillify` intents.
- Trigger-prefilter + phrase-window dispatch (aho-corasick): finds explicit-trigger keyword occurrences anywhere in the body — including across sentence boundaries and inside very large bodies — extracts a window per eligible occurrence, and runs text rules against the windows. First-match-wins applies *per window*. Quote-aware and abbreviation-aware. Built-in trigger detection runs unconditionally regardless of body size.
- Hardened `forget` resolver contract: regex-originated substring matches **never** auto-authorize delete; resolver routes them to an interactive `forget_ambiguous` outcome. Auto-proceed is gated on `match_strategy == Exact` (quoted-string capture) plus a unique candidate, or on a stable `record_id` passed by an out-of-band caller.
- **Runtime-enforced** body source via `BodyResolution::Resolved { text, source: BodySource }`. The `BodySource` enum has no variant for agent rationale, so internal reasoning literally cannot be passed in as a user utterance — the privacy invariant is encoded in the type, not in caller discipline.
- 64 KiB body cap on Phase B (user rules) only — Phase A always runs the prefilter so explicit triggers are detected on bodies of any size. Bodies above the cap still appear in `llm_eligible_spans` for additive LLM enrichment. Closes the latency-DoS path without sacrificing the always-on guarantee.
- Per-rule wall-clock budget enforcement and hard `max_drafts` cap on Phase B; typed `TruncationReason` returned to the caller (§6).
- Documented chain handoff contract for #74: regex emits typed `llm_eligible_spans` derived from clause spans minus high-confidence (≥0.9) regex coverage, plus any `ClauseCapExceeded` tail. Suppression is **span-scoped, not event-scoped**, and **confidence-gated** even under truncation: low-confidence regex spans remain LLM-eligible so weak matches cannot block LLM recovery.
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
    ├── mod.rs          # ExtractorWorker, ExtractInput, BodyResolution, BodyResolutionError, ExtractBudget, ExtractError, ExtractOutput, ExtractResult, TruncationReason
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
pub struct ExtractInput<'a> {
    pub event: &'a CaptureEvent,
    pub body: BodyResolution<'a>,
}

/// Body-resolution result, threaded through the extractor chain.
#[derive(Clone, Debug)]
pub enum BodyResolution<'a> {
    NotApplicable,
    /// A `ResolvedBody`, constructed only via the named-by-source
    /// constructors below. Public field access is disallowed; readers go
    /// through `text()` / `source()` accessors.
    Resolved(ResolvedBody<'a>),
    Failed(BodyResolutionError),
}

/// Resolved body bytes plus their trust-boundary source. The fields are
/// **private** to `cairn_core::pipeline::extract`. Construction goes
/// through one of the named functions below, each tied to a specific
/// `BodySource` and to the typed payload variant the bytes came from.
/// External callers cannot construct a `ResolvedBody { text, source }`
/// directly, so a buggy or stale caller cannot label `rationale` bytes
/// as `ProactiveMessage` by accident.
#[derive(Clone, Debug)]
pub struct ResolvedBody<'a> {
    text: &'a str,
    source: BodySource,
}

impl<'a> ResolvedBody<'a> {
    /// Construct from a `CapturePayload::Cli` or `::Mcp` envelope after
    /// the caller has materialized and hash-verified the user-supplied
    /// payload bytes. The `payload_kind` argument is taken by reference
    /// to bind the lifetime, and exists primarily to make the call site
    /// self-documenting; the function name is the contract.
    pub fn from_user_ingest(
        text: &'a str,
        payload_kind: UserIngestPayloadKind,
    ) -> Self {
        let _ = payload_kind;
        Self { text, source: BodySource::UserIngest }
    }

    /// Construct from a `CapturePayload::Hook` envelope after the caller
    /// has materialized and hash-verified the user utterance bytes
    /// (e.g. `UserPromptSubmit`).
    pub fn from_hook_utterance(text: &'a str, hook_name: &'a str) -> Self {
        let _ = hook_name;
        Self { text, source: BodySource::HookUtterance }
    }

    /// Construct from a `CapturePayload::Proactive` envelope's
    /// **message body**, NOT from `rationale`. The caller passes the
    /// hash-verified message-body bytes plus a reference to the
    /// `Proactive` payload so the constructor can perform a defensive
    /// runtime check: if `text == payload.rationale`, the constructor
    /// returns an error rather than producing a `ResolvedBody`. This
    /// catches the obvious bug where a caller passes the wrong field.
    /// More-subtle mislabelling is still possible at the byte level, so
    /// the caller's read path is also responsible for sourcing bytes
    /// from the message-body field; the runtime check is a backstop,
    /// not a substitute for that discipline.
    pub fn from_proactive_message(
        text: &'a str,
        payload: &ProactiveBodyContext<'a>,
    ) -> Result<Self, BodyResolutionError> {
        if text == payload.rationale {
            return Err(BodyResolutionError::ProactiveRationaleMislabel);
        }
        Ok(Self { text, source: BodySource::ProactiveMessage })
    }

    pub fn text(&self) -> &str { self.text }
    pub fn source(&self) -> BodySource { self.source }
}

/// Marker for the `Cli` / `Mcp` payload variant the bytes came from.
/// Carried for tracing and audit; `from_user_ingest` does not branch on
/// it — the function name itself is the contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserIngestPayloadKind {
    Cli,
    Mcp,
}

/// Reference into `CapturePayload::Proactive` for the runtime
/// rationale-mislabel check.
pub struct ProactiveBodyContext<'a> {
    pub rationale: &'a str,
}

/// The trust boundary a resolved body came from.
///
/// **There is deliberately no `Rationale` variant.** Combined with the
/// private constructors on `ResolvedBody`, the only way to produce a
/// `ResolvedBody` tagged `ProactiveMessage` is via
/// `from_proactive_message`, which (a) is named after the message-body
/// field, (b) takes the message-body text and (c) defensively rejects
/// text equal to `rationale`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum BodySource {
    UserIngest,
    HookUtterance,
    ProactiveMessage,
}

impl BodyResolution<'_> {
    pub fn allows_text_rules(&self) -> bool {
        matches!(self, BodyResolution::Resolved(_))
    }
}

/// Reason a body could not be resolved. Stable variants the chain branches on.
#[derive(Clone, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BodyResolutionError {
    #[error("payload_ref not found: {0}")]
    NotFound(String),
    #[error("payload_hash mismatch (expected {expected}, got {got})")]
    HashMismatch { expected: String, got: String },
    #[error("payload bytes are not valid UTF-8")]
    NotUtf8,
    #[error("transient I/O error reading payload_ref: {0}")]
    Io(String),
    #[error("ResolvedBody::from_proactive_message called with text equal to rationale — refusing to extract internal reasoning as user memory")]
    ProactiveRationaleMislabel,
}

/// `#[async_trait]` is required, not native async-fn-in-traits + RPITIT.
/// The chain dispatcher in #74 holds extractors as `Box<dyn ExtractorWorker>`
/// (homogeneous storage of regex / llm / agent workers under one type), and a
/// trait with native `async fn` is not object-safe in Rust 1.95. CLAUDE.md
/// §6.3 explicitly carves out this case: "Keep async_trait only when trait
/// objects (`dyn Trait`) are required." This is one of those cases.
#[async_trait::async_trait]
pub trait ExtractorWorker: Send + Sync {
    fn name(&self) -> &'static str;
    fn budget(&self) -> ExtractBudget;
    async fn extract(
        &self,
        input: &ExtractInput<'_>,
    ) -> Result<ExtractResult, ExtractError>;
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
    #[error("body resolution failed for event {event_id}")]
    BodyResolution {
        event_id: String,
        #[source]
        source: BodyResolutionError,
    },
}
```

Trait-object storage is the load-bearing constraint: the chain in #74 holds
`Vec<Box<dyn ExtractorWorker>>` to dispatch regex/llm/agent uniformly, so the
trait must be object-safe. `async_trait` adds a heap-allocated future per
call but the chain only invokes one extractor per event per phase — a single
boxing allocation, well below the latency budget.

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

`ForgetIntent` is a structured selector, not free-form text — `forget` is irreversible, so `target_text` alone would force #75 to guess which records to delete.

```rust
// pipeline/extract/intent.rs

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ForgetIntent {
    /// Normalized free-text selector lifted from the trigger (lowercased,
    /// whitespace-collapsed, leading/trailing stop-words trimmed).
    /// Carried as a hint, never as the sole basis for deletion.
    pub target_text_normalized: String,

    /// How the downstream resolver should compare `target_text_normalized`
    /// against candidate record bodies. The default rule emits `Substring`;
    /// the resolver may upgrade to `Exact` if the user's phrasing was
    /// quoted, or to `Fuzzy` once #75 ships fuzzy matching.
    pub match_strategy: ForgetMatchStrategy,

    /// Optional kind hint to narrow the candidate set (e.g. user-said
    /// "forget the rule about X" → `Some(KindHint::from(MemoryKind::Rule))`).
    /// Default rule leaves this `None`; user rules can encode it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind_filter: Option<KindHint>,

    /// Byte span within the resolved body where the trigger fired —
    /// audit + debug only. Filter/Classify must not derive deletion
    /// scope from this span.
    pub source_span: TextSpan,

    /// How confident the rule was that this clause is a forget intent.
    /// Same `Confidence` newtype as `MemoryDraft.confidence`. The
    /// LLM-suppression algorithm in §6.5 treats `ForgetIntent` outputs
    /// uniformly with `MemoryDraft` outputs: if `confidence >=
    /// CONFIDENCE_GATE_FOR_SUPPRESSION` (0.9), the span is removed
    /// from `llm_eligible_spans`; otherwise it stays eligible. The
    /// built-in `forget` rule emits 0.95.
    pub confidence: Confidence,

    pub source_event: CaptureEventId,
    pub trigger_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ForgetMatchStrategy {
    /// Body matches `target_text_normalized` after the same normalization.
    Exact,
    /// `target_text_normalized` is a substring of the (normalized) body.
    /// Default for the built-in `forget` rule.
    Substring,
    /// Reserved for #75; rejected at construction time in this PR.
    Fuzzy,
}
```

**Resolver contract for #75 (documented here so the dependency is visible):**

A `ForgetIntent` is a *candidate* for deletion, never an instruction. `forget` is irreversible (§5.6), so the resolver must require strong evidence of user intent. The resolver in #75 must:

1. Find the candidate set using `kind_filter` + `match_strategy(target_text_normalized)`.
2. **Auto-proceed is only allowed when both of these hold**:
   - `match_strategy == Exact` (target text was either a quoted-string capture or otherwise anchored, not a free substring), and
   - the candidate set has exactly one record.

   Even then the verb stays subject to the consent + WAL gates of §5.6.
3. **Substring matches never auto-proceed.** Every `match_strategy == Substring` intent — even when the candidate set has exactly one record — is surfaced as a `forget_ambiguous` interactive outcome (CLI / MCP / SDK) listing the candidate(s) and requiring the user to either pass a stable `record_id` on a follow-up call or confirm. Substring is a hint; it is not authorization.
4. If the candidate set is empty, write a `lint`-surfaced `forget_unresolved` event; never delete.
5. For any candidate set size > 1, regardless of strategy, emit `forget_ambiguous` and require disambiguation by stable `record_id`. Never silently pick one.
6. **Stable-id path is also supported**: callers (notably the chain dispatcher and any agent surface) may attach an explicit `record_id: Option<RecordId>` to the resolver call. When present, that path bypasses the text-match resolver entirely. The regex extractor never populates it — only out-of-band callers do (CLI flag, MCP arg).

The built-in `forget` rule emits `match_strategy = Substring`, which means a regex-originated forget intent on its own can never authorize a delete: it always either matches no records (lint) or routes through `forget_ambiguous`. To get auto-proceed, the user has to (a) phrase the request with quoted text the resolver can promote to `Exact`, (b) pass a stable record id directly to the verb, or (c) confirm in the interactive outcome. All three paths are explicit. This closes the round-4 risk that a single accidental substring match could authorize the wrong delete.

User rules may emit `match_strategy = Exact` only if they capture a quoted-string group from the body (e.g. `forget "my old address"`); enforced by a runtime check in `from_config` that any rule with `Exact` declares a `quoted_capture: true` field. `match_strategy = Fuzzy` is rejected by `from_config` until #75 ships fuzzy matching.

This PR ships only the structured `ForgetIntent` type and the populated fields from regex-matched triggers; it does not implement the resolver. The contract is captured here so that #75 cannot accidentally weaken it later without an explicit spec amendment.

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
        /// Confidence emitted into `ForgetIntent.confidence`. Carries
        /// the same semantics as `MemoryDraft.confidence`: the chain
        /// dispatcher uses it to decide LLM suppression (§6.5). The
        /// built-in `forget` rule emits 0.95; user rules pick their own
        /// (validated `[0.0, 1.0]`).
        confidence: Confidence,
        /// Match strategy emitted by this rule into the resulting
        /// `ForgetIntent`. The default rule emits `Substring`; the
        /// resolver in #75 routes substring intents to interactive
        /// confirmation regardless of candidate-set size.
        ///
        /// Validated by `from_config`:
        /// - `Substring` (default): always allowed.
        /// - `Exact`: only allowed when the rule captures a
        ///   quoted-string group; the rule must additionally set
        ///   `quoted_capture: true` and the pattern must wrap
        ///   `target_group` in `"…"` or `'…'`.
        /// - `Fuzzy`: rejected until #75 ships fuzzy matching.
        #[serde(default = "ForgetMatchStrategy::default_substring")]
        match_strategy: ForgetMatchStrategy,
        #[serde(default)]
        quoted_capture: bool,
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
    /// Built-in rules that always run to completion. Cannot be truncated by
    /// `max_drafts` or `max_wall_ms`. The set is bounded and cheap (~10
    /// rules in §7); load-bearing for the privacy guarantee that explicit
    /// `remember`/`forget`/`skillify` triggers are never silently dropped.
    builtin_text_rules: Vec<CompiledRule>,
    builtin_hook_rules: Vec<CompiledRule>,
    builtin_tool_frame_rules: Vec<CompiledRule>,
    /// User-supplied rules. Subject to `max_drafts` and `max_wall_ms`.
    user_text_rules: Vec<CompiledRule>,
    user_hook_rules: Vec<CompiledRule>,
    user_tool_frame_rules: Vec<CompiledRule>,
}

impl RuleSet {
    pub fn builtin() -> Self { /* defaults.rs */ }
    pub fn from_config(rules: Vec<RegexRule>) -> Result<Self, ExtractError> { /* validate + compile */ }
    pub fn with_user_rules(self, user: Vec<RegexRule>) -> Result<Self, ExtractError> { /* compile + append, reject duplicate ids */ }
}
```

The split between built-in and user-rule buckets is the load-bearing fix for "truncation could drop explicit user intents": built-ins are the universe of triggers documented in brief §11.6 / §18.a, and they always run.

## 6. Dispatch

`RegexExtractor::extract` reads `input.event.payload` for the variant discriminator and `input.body` for any user text. **`CapturePayload` is not modified.** Raw text remains behind `payload_ref` + `payload_hash`; the caller materializes it (verifying the hash) and passes it as `ExtractInput.body`. This keeps the privacy boundary documented in `crates/cairn-core/src/domain/capture.rs:362-377` intact and avoids duplicating raw prompt text into a second serialized location.

| `CapturePayload` variant | Rule families consulted | Source of body for `text_rules` |
|---|---|---|
| `Hook` | `hook_rules` (always); `text_rules` if `input.body.is_some()` | `input.body` (caller resolves from `payload_ref` for hooks that carry a user utterance, e.g. `UserPromptSubmit`; `None` otherwise) |
| `Terminal` | `tool_frame_rules` filtered to `Terminal` | n/a |
| `Ide` | `tool_frame_rules` filtered to `Ide` | n/a |
| `Cli`, `Mcp` | `text_rules` if `input.body.is_some()` | `input.body` (caller resolves the user-supplied ingest payload from `payload_ref`) |
| `Proactive` | `text_rules` if `input.body.is_some()` | **`input.body` is the user-visible message body only.** The `rationale` field on `CapturePayload::Proactive` is internal agent reasoning and is **never** an extraction source — using it would persist internal text as user memories and create an unstable trust boundary. Callers that resolve a `Proactive` body must read the message-body bytes from `payload_ref` (verifying `payload_hash`) and *not* substitute `rationale`. The caller contract documents this explicitly; the extractor cannot enforce it from inside (the bytes look the same), so it is a hard rule on the resolution layer. |
| `Voice`, `Screen`, `Clipboard`, `RecordingBatch` | none in this PR — extender lands with the relevant sensor issue | n/a |

For each matching rule, the extractor pushes a `Draft(MemoryDraft)` (or `Forget(ForgetIntent)`) onto the result vector.

### 6.1 Body resolution

Before dispatching text rules, the extractor inspects `input.body`:

| `BodyResolution` | Behaviour |
|---|---|
| `Resolved(s)` | `s` is the input to text rules. |
| `NotApplicable` | Text rules are silently skipped. Hook + tool-frame rules still run. Not an error. |
| `Failed(err)` | Returns `Err(ExtractError::BodyResolution { event_id, source: err })`. The chain dispatcher (#74) decides retry vs. surface; the extractor must not produce any outputs from a partial dispatch on a failed body. |

This separation means a hash mismatch or I/O error on `payload_ref` cannot be misread as "no body" and therefore cannot silently drop an explicit `remember` / `forget` request.

### 6.2 Two-phase dispatch (built-in then user)

Dispatch runs in two phases per event:

1. **Prefilter (always runs, body-size-independent).** Run the aho-corasick trigger scan and build the phrase-window list (§6.2). Cost: O(body_len) at ~GB/s; well under 1 ms even on 1 MiB bodies. **There is no body-size escape from the prefilter** — built-in trigger detection is unconditionally reachable. The earlier-spec behaviour ("skip Phase A entirely on bodies > 64 KiB") is rejected because it silently dropped explicit `forget` / `remember` intents in long pasted transcripts when the LLM was unavailable.

2. **Phase A — built-ins on phrase windows.** Run every built-in text rule against every phrase window in declaration order. **`max_drafts` is not enforced** in Phase A. A Phase-A wall-clock observability rail (`MAX_PHASE_A_WALL_MS`, default 2 ms; `MAX_PHASE_A_WALL_MS_LARGE`, default 10 ms when `body_len > 64 KiB`) is checked once after Phase A completes; if exceeded, the extractor emits `tracing::warn!` and the deployment self-check (follow-up issue) flags the configuration. The hit count from the prefilter bounds work: phrase windows are ≤ 64 occurrences in any practical input (the prefilter caps them at 64 with a `tracing::warn!` on overflow; remaining keyword occurrences after the 64th become a single span in `llm_eligible_spans`).

3. **Phase B — user rules on phrase windows.** Run user rules against the same phrase windows. **Phase B is the only general-truncation surface.** `max_drafts` and `max_wall_ms` are checked after each user rule. If user rules declare custom keyword vocabularies (a future extension), a separate prefilter is built for them; in this PR Phase B reuses the same windows.

4. **Hook and tool-frame rules.** Run independently of body and prefilter; bounded by rule count.

The `MAX_BODY_LEN_FOR_REGEX = 64 KiB` constant **still exists** but it now only governs Phase B, not Phase A. On bodies above the cap, Phase B is skipped (user rules don't run) and `truncated = BodyTooLarge { body_len }` is set so the chain still routes those bodies through LLM enrichment via `llm_eligible_spans`. Phase A always runs; `tracing::warn!(worker = "regex", body_len, "body exceeds MAX_BODY_LEN_FOR_REGEX, skipping Phase B user rules")` fires once.

**Trigger-prefilter + phrase-window dispatch.** Text rules do not run against the whole body and do not depend on splitting prose into clauses. Instead, the extractor:

1. **Prefilters** the body for occurrences of the small set of trigger keywords using `aho-corasick` (linear-time, ~GB/s, prefix-set is fixed and tiny):
   - `remember`
   - `forget`
   - `correction`
   - `skillify`
   - `this is how`

2. For each keyword hit, checks **sentence-start eligibility**. A hit is at a sentence start if any of these hold:
   - hit position is `0` (start of body);
   - preceding byte (after stripping whitespace) is `\n`, `;`, `?`, or `!`;
   - preceding byte (after stripping whitespace) is `.` AND the byte before that period is **not** an abbreviation-marker (uppercase letter inside ≤ 4 chars of an earlier `.`, the standard `U.S.`-style guard) — implemented with a tiny one-pass scanner over the previous 6 bytes;
   - preceded by a comma or one of `and`/`but`/`then` followed by whitespace (the round-7 trigger-prefixed conjunction case);
   - inside-quote check: hits inside `"…"`, `'…'`, or backtick spans are not eligible.

3. **Phrase windows.** For each eligible hit, the extractor extracts a window from `hit_pos` forward to the next "stop": whichever of `;`, `\n`, `.`-followed-by-uppercase-or-EOF, end-of-body, or another eligible trigger keyword comes first. The window is the substring rules match against, anchored at position 0.

4. **Rules dispatch.** Built-in text rules (Phase A) and user text rules (Phase B) run against each phrase window in order, with first-match-wins **per window** (the round-5/6 invariants).

This makes both round-9 failure modes structurally impossible:

- `"FYI, old address is stale. forget my old address"` — the prefilter finds `forget` at the start of the second sentence; the abbreviation-aware sentence-start check accepts because the preceding `.` is followed by uppercase-equivalent (any keyword start). Window is `"forget my old address"`. The built-in `forget` rule fires.
- `"remember that I live in the U.S. and prefer cash"` — the prefilter finds one `remember` at position 0. No second hit at `U.S.` because none of the keywords appears there. The window runs from position 0 to end-of-body (no intermediate stop), so the captured body is the whole sentence.
- A 1 MiB pasted transcript ending in `"forget my old address"` — the prefilter scans the whole body in roughly 1 ms (RE2-style scan); the only eligible hit is the trailing trigger; the window is `"forget my old address"`. Phase A still fires, no fallback dependency on the LLM. (See §6.3 for how Phase B and the body-size cap interact with this; Phase A's prefilter has no body-size cap.)

**Quote-awareness:** the inside-quote check above means `"the user said \"remember that\" by mistake"` does not produce an extraction — the trigger inside quotes is not eligible. Implemented in the same one-pass scanner.

Hook and tool-frame rules ignore the prefilter entirely and continue to run at most once per event.

**Output spans.** Each rule fire records `source_span` as the byte range of its phrase window in the original body, used for the dedup contract in §6.5.

This handles compound utterances correctly:

| Body | Clauses (after split) | Outputs |
|---|---|---|
| `"forget my old address and remember the new one is 1 Main St"` | `["forget my old address", "remember the new one is 1 Main St"]` | `Forget(target="my old address")` + `Draft(kind=user, body="the new one is 1 Main St")` |
| `"correction: it's actually Z; remember that"` | `["correction: it's actually Z", "remember that"]` | `Draft(kind=feedback, body="it's actually Z")` + `Draft(kind=user, body="that")` (low-quality remember; that's fine — confidence stays at 0.95 from the rule, but #75's filter has its own discard reasons) |
| `"remember that I prefer dark mode"` | `["remember that I prefer dark mode"]` | `Draft(kind=user, body="I prefer dark mode")` (single clause, single output) |

**Per-clause first-match-wins still prevents the original overlap bug.** The "remember never share API keys" case sits in one clause, so the `remember.rule` priority over `remember.preference` from §6 still holds.

**Bounded clause count.** The clause splitter caps at 8 clauses per body. The first 8 clauses are dispatched as normal; **the remaining body bytes (from the start of clause 9 to end-of-body) are recorded as a single uncovered span** in `ExtractResult.llm_eligible_spans` (see §6.4) and `truncated` is set to `TruncationReason::ClauseCapExceeded { processed: 8, body_len }`. A `tracing::warn!(splitter = "regex", body_len, "clause cap reached")` is emitted. The earlier behaviour ("collapse to single-clause first-match-wins on overflow") is rejected because it would silently drop later explicit commands in long compound utterances — exactly the failure mode this design exists to prevent. The cap exists to bound work, not to discard intent: the LLM extractor is free to take the uncovered tail and complete the extraction.

**No multi-fire per rule per event.** A given rule fires at most once per *clause*. The same rule can fire on multiple clauses of one body — that is the desired behavior for compound utterances (e.g. two consecutive `correction:` clauses). Hook and tool-frame rules continue to fire at most once per event.

### 6.3 Budget enforcement (Phase B only)

Built-ins always run to completion (§6.2). The budget governs Phase B.

**`max_drafts` (hard cap on user-rule output).** After each Phase-B rule push, check `outputs.len() >= budget.max_drafts`. When the cap is reached:

1. Stop scanning further user rules.
2. Emit exactly one `tracing::warn!(worker = "regex", event_id, max_drafts, "regex extractor reached max_drafts cap during user-rule dispatch")` — never the body.
3. Set `truncated = TruncationReason::MaxDrafts`.

**`max_wall_ms` (per-rule wall-clock check on user-rule loop).** `Instant::now()` is captured at the start of Phase B. Elapsed time is checked **after each user-rule evaluation**. When elapsed exceeds `budget.max_wall_ms` mid-Phase-B:

- if Phase A produced zero outputs **and** zero Phase-B outputs, return `Err(BudgetExceeded { worker, elapsed_ms })` (full fall-through);
- otherwise stop scanning user rules, `tracing::warn!` with elapsed time, set `truncated = TruncationReason::MaxWallMs { elapsed_ms }`.

The per-rule check ensures a large or pathological user-rule list cannot blow the budget unchecked. Cost: one `Instant::now()` per user rule, ~10 ns on x86_64.

### 6.4 Return shape

```rust
pub struct ExtractResult {
    pub outputs: Vec<ExtractOutput>,
    pub truncated: TruncationReason,
    /// Byte ranges of the original body that the LLM extractor in the
    /// chain SHOULD run on. Includes (a) clauses that no text rule
    /// matched, (b) clauses matched only by a regex with
    /// `confidence < CONFIDENCE_GATE_FOR_SUPPRESSION`, and (c) any tail
    /// uncovered because of `ClauseCapExceeded`. Sorted and disjoint;
    /// safe to feed directly into a slice-extraction loop. This is the
    /// canonical input the chain dispatcher consumes — derive nothing
    /// else from raw output spans.
    pub llm_eligible_spans: Vec<TextSpan>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TruncationReason {
    None,
    MaxDrafts,
    MaxWallMs { elapsed_ms: u32 },
    /// Body had > 8 clauses; first 8 dispatched, the remainder is part
    /// of `llm_eligible_spans` for the LLM extractor to pick up.
    ClauseCapExceeded { processed: u8, body_len: u32 },
    /// Body exceeded `MAX_BODY_LEN_FOR_REGEX`; text rules skipped, full
    /// body added to `llm_eligible_spans`.
    BodyTooLarge { body_len: u32 },
}
```

`llm_eligible_spans` is computed against the original body bytes. The
algorithm builds the list explicitly so it stays correct under all
truncation modes:

1. Start with every clause span produced by the splitter.
2. Subtract any clause span that was matched by a regex output with
   `confidence >= CONFIDENCE_GATE_FOR_SUPPRESSION` (default 0.9). Those
   spans are "fully claimed" by regex and the LLM should not revisit
   them.
3. Append the tail span when `ClauseCapExceeded` fires (from clause 9's
   start to end-of-body).
4. Merge adjacent or overlapping ranges; merge separator bytes into
   adjacent uncovered ranges.
5. For non-text payload variants (`NotApplicable` body, hook /
   tool-frame events) the list is empty.

Crucially, **a low-confidence regex match does not subtract its span**.
If a synthetic user rule with `confidence: 0.5` matched clause 1, that
clause's span stays in `llm_eligible_spans` so the LLM can produce a
competing higher-quality draft. Filter (#75) breaks ties by confidence.
This makes the round-7 invariant — "weak regex matches stay
re-checkable, even under truncation" — provable by construction.

Trait signature: `async fn extract(...) -> Result<ExtractResult, ExtractError>`.

### 6.5 Chain handoff (informs #74)

The chain dispatcher in #74 enforces the following match on this PR's output. It is documented here to keep the contract close to where it is produced:

| Outcome | Chain behaviour | Why safe |
|---|---|---|
| `Err(ExtractError::BudgetExceeded { .. })` | Fall through to LLM extractor on the same body. | No regex output exists, so no duplicate-write risk. |
| `Err(ExtractError::BodyResolution { .. })` | Surface or retry per the failure variant; **do not** treat as bodyless. | Failed resolution is not "no body". |
| `Ok(result)` with `truncated == MaxDrafts` / `MaxWallMs` / `ClauseCapExceeded` | Run LLM extractor on `result.llm_eligible_spans` (sliced from the body). Drafts produced by the LLM coexist with regex drafts; downstream Filter (#75) deduplicates by confidence. | Regex output is preserved; LLM gets exactly the byte ranges regex did not cover. No silent loss. |
| `Ok(result)` with `truncated == None` | Same: run LLM extractor on `result.llm_eligible_spans`. If the vector is empty and the body was non-empty, the LLM still runs on the full body for additive enrichment (entity extraction, etc.) but the chain marks any LLM draft whose span overlaps a regex draft as `competing` — Filter (#75) breaks the tie by confidence + kind agreement, not by suppression. | Regex on its own is not authoritative enough to silence the LLM; weak regex matches (low confidence, partial body) must be re-checkable. |
| `Err(ExtractError::BudgetExceeded { .. })` | Fall through to LLM extractor on the full body. | No regex output to coexist with. |
| `Err(ExtractError::BodyResolution { .. })` | Surface or retry per the failure variant; do not treat as bodyless. | Failed resolution is not "no body". |

**Suppression is span-scoped, not event-scoped.** A regex `Forget` intent on clause 1 does not suppress LLM forget extraction on clauses 2–N. The LLM dispatcher passes each uncovered span to its own extraction call; forget detection on each span is independent.

**Suppression is also confidence-gated, uniformly across `MemoryDraft` and `ForgetIntent`.** Both output variants carry `confidence: Confidence`. Regex outputs with `confidence < 0.9` do **not** mark their span as covered for LLM purposes; outputs at or above 0.9 do. This means a low-confidence pickup like `remember that → body="that"` (the §6.2 "low quality" example) leaves its span eligible for LLM re-extraction, and a low-confidence (user-defined) forget rule does the same. Built-in trigger rules above 0.9 (every entry in §7, including `forget` at 0.95) gate their span; they are unambiguous user intents and re-extraction would only duplicate. The 0.9 threshold is a constant in `extract::CONFIDENCE_GATE_FOR_SUPPRESSION`; tunable per future research, but pinned for #74's contract.

**Span discipline:** every `MemoryDraft` and every `ForgetIntent` produced by a text rule must populate `source_span` (clause start/end in the original body's byte offsets). The regex emitter is the only path that creates these and has the offsets in scope. Hook and tool-frame outputs carry `source_span = None`; the LLM dispatcher never runs on those events, so the dedup key is well-defined wherever it is needed.

Together, span-scoped + confidence-gated suppression keep three invariants:

1. High-confidence regex captures (the explicit `remember` / `forget` / `skillify` triggers in §7) are not duplicated by the LLM.
2. Low-confidence or partial regex captures stay re-checkable — weak matches do not become terminal.
3. Uncovered clauses (including the tail after `ClauseCapExceeded`) always run through the LLM. **No body byte goes unexamined unless the user disabled the LLM extractor.**

#74 will own the actual dispatcher implementation and metrics (`chain.regex_truncated`, `chain.body_resolution_failed`); this PR's job is to produce the inputs that contract takes.

### 6.6 Determinism

Built-ins dispatch in `defaults.rs` declaration order. User rules dispatch in `from_config` declaration order. Two identical inputs produce identical outputs and identical `truncated` flags.

## 7. Default rule set (`defaults.rs`)

Text rules are listed in dispatch order (most specific first); first-match-wins per §6.

| Order | Rule id | Variant | Pattern (case-insensitive) | Kind hint | Confidence |
|---|---|---|---|---|---|
| 1 | `remember.rule` | TriggerPhrase | `^\s*remember(?::|,)?\s+never\s+(.+?)\s*$` | `rule` | 0.95 |
| 2 | `remember.preference` | TriggerPhrase | `^\s*remember(?:\s+that)?\s+(.+?)\s*$` | `user` | 0.95 |
| 3 | `correction` | TriggerPhrase | `^\s*correction:?\s+(.+?)\s*$` | `feedback` | 0.95 |
| 4 | `success.recipe` | TriggerPhrase | `^\s*this is how we did it\s*[—-]\s*it worked\s*$` | `strategy_success` | 0.85 |
| 5 | `skillify` | TriggerPhrase | `^\s*skillify\s+(?:this|it)\s*$` | `playbook` | 0.95 |
| 6 | `forget` | ForgetPhrase | `^\s*forget\s+(?:that\s+|what\s+)?(.+?)\s*$` | n/a (target = group 1) | 0.95 |
| — | `hook.post_tool_use` | HookEvent | hook=`PostToolUse` | `trace` | 0.8 |
| — | `hook.stop` | HookEvent | hook=`Stop` | `trace` | 0.7 |
| — | `hook.pre_compact` | HookEvent | hook=`PreCompact` | `trace` | 0.7 |
| — | `tool.terminal_failure` | ToolFrame | Terminal, exit_code_nonzero=true | `strategy_failure` | 0.7 |

Ordering is load-bearing: `remember.rule` must precede `remember.preference` so an utterance like `remember never share API keys` produces exactly one `rule` draft, not a `rule` + `user` pair. `from_config` rejects user rules that share an `id` with a built-in; user rules are appended after built-ins, so a user rule can never preempt a built-in trigger by re-binding an identical pattern.

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
- **Overlap regression:** body `"remember never share API keys"` produces exactly one output, with `kind_hint = "rule"` (matched by `remember.rule`, not `remember.preference`). Body `"remember that I prefer dark mode"` produces exactly one output with `kind_hint = "user"`. First-match-wins: a body that synthetically matches two TriggerPhrase rules emits only the first.
- **Built-ins are untruncatable:** synthesize a `RuleSet` with `builtin()` plus a user-rule list of `max_drafts + 5` always-matching rules; pass a body that the built-in `remember.preference` matches. Assert (a) the built-in fires (Phase A produced its draft), (b) `truncated == TruncationReason::MaxDrafts` from Phase B, (c) `outputs[0].kind_hint == "user"` (built-in came first), (d) `outputs.len() == max_drafts`. Demonstrates that truncation cannot silently drop a `remember` trigger.
- **Body resolution failure:** `ExtractInput.body = BodyResolution::Failed(BodyResolutionError::HashMismatch { .. })`; assert `extract` returns `Err(ExtractError::BodyResolution { event_id, source: HashMismatch { .. } })` and produces zero outputs (no partial side effects).
- **Compound utterance:** body `"forget my old address and remember the new one is 1 Main St"`; assert `outputs.len() == 2`, `outputs[0]` is `Forget(target_text_normalized="my old address")`, `outputs[1]` is `Draft(kind="user", body="the new one is 1 Main St")`. Both have non-overlapping `source_span`s. `llm_eligible_spans` is empty (both clauses matched at confidence ≥ 0.9).
- **Conjunction-split safety (round-7 regression):** body `"remember that Alice and Bob are on call"`; assert `outputs.len() == 1`, the single draft body is `"Alice and Bob are on call"` (whole sentence, not just `"Alice"`). The `and` here is not followed by a trigger prefix, so the splitter must keep the body as one clause. Mirror tests for `but` and `then`. Add a snapshot for `"remember that I prefer dark mode but actually light mode at night"` showing it stays one clause.
- **Quote-aware splitting:** body `'remember that "X, Y, and Z" is the order'`; assert `outputs.len() == 1` and the captured body preserves the quoted list intact (no split inside quotes).
- **Clause cap preserves overflow as uncovered:** body with 10 comma-separated `remember X, remember Y, ...` clauses; assert (a) `outputs.len() == 8`, (b) `truncated == ClauseCapExceeded { processed: 8, body_len }`, (c) `llm_eligible_spans` contains exactly one range starting at the byte offset of clause 9 and ending at end-of-body, (d) one `clause cap reached` warn fires. Earlier-spec behaviour ("collapse to single-clause") is forbidden — guard with a regression test that asserts the body is **not** matched as a single first-match-wins string.
- **Uncovered spans on partial coverage:** body `"hello world. remember that I prefer dark mode"`; assert `outputs.len() == 1`, the regex draft's `source_span` covers clause 2 only, and `llm_eligible_spans` contains one range covering clause 1 (`"hello world"` plus the `. ` separator merge).
- **Confidence gate:** add a synthetic user rule with `confidence: 0.5` matching `"hello (.+)"`; pass body `"hello world"`. Assert (a) the rule fires and is in `outputs`, (b) **`llm_eligible_spans` contains the matched clause's span** — the low-confidence regex output does not subtract its span from the LLM-eligible set. Mirror with `confidence: 0.95`: assert `llm_eligible_spans` is empty for that case.
- **Confidence gate under truncation:** combine the previous test with a Phase-B truncation (`max_drafts: 1`, two always-matching low-confidence user rules); assert `truncated == MaxDrafts`, `outputs.len() == 1`, and `llm_eligible_spans` includes both the matched span (low confidence) and the unprocessed clause's span. The earlier-spec failure mode ("low-confidence match becomes terminal under truncation") is forbidden — guard with a regression test.
- **Span-scoped forget suppression:** body `"forget my address. anyway, please also forget that other thing"`; built-in `forget` matches the first clause. Assert `outputs[0]` is the `Forget` intent on clause 1, `llm_eligible_spans` contains the second clause's range (so the LLM dispatcher in #74 can run `forget` re-extraction on clause 2 independently). The contract that "regex Forget does NOT suppress LLM forget on other spans" is asserted via a unit test on the contract documentation.
- **Within-clause first-match-wins:** body `"remember never share API keys"`; assert single output with `kind = "rule"`. (The clause splitter must not break this case — `"remember never share API keys"` stays as one clause because there is no separator.)
- **Forget contract — substring stays substring:** `forget` rule fires on `"forget my old address"`; assert `match_strategy == Substring` (not `Exact`) and `confidence == 0.95`. Round-trip serde preserves both.
- **Forget LLM-suppression uniformity:** synthesize a user `ForgetPhrase` rule with `confidence: 0.5`; pass a body that matches it. Assert (a) the intent is in `outputs`, (b) the span stays in `llm_eligible_spans` (low-confidence forget remains LLM-eligible). Mirror with `confidence: 0.95`: span is removed from `llm_eligible_spans`. The contract that "forget intents are treated uniformly with drafts under the 0.9 gate" is asserted by direct comparison.
- **Forget contract — Fuzzy rejected:** `RuleSet::from_config(<rule with match_strategy: Fuzzy>)` returns `Err(InvalidRule)`. (This guards the resolver contract: even if a future user-config tries to declare `Fuzzy`, the extractor refuses to compile it until #75 is wired up.)
- **Forget contract — user `Exact` requires `quoted_capture: true`:** user rule with `match_strategy: Exact` and `quoted_capture: false` (or absent) → `InvalidRule`.
- **Proactive runtime enforcement:** `BodySource` has no `Rationale` variant (exhaustive-match test). `ResolvedBody`'s fields are private — there is no compilable path to construct a `Resolved` variant outside `cairn_core::pipeline::extract`. Test that `ResolvedBody::from_proactive_message(rationale_str, &ProactiveBodyContext { rationale: rationale_str })` returns `Err(BodyResolutionError::ProactiveRationaleMislabel)` (defensive runtime check). Test that `ResolvedBody::from_proactive_message(message_body_str, ...)` returns `Ok(_)` and the resulting `BodySource` is `ProactiveMessage`.
- **Oversize body still extracts triggers (round-9 regression):** synthesize a 1 MiB body whose last line is `"forget my old address"`; assert (a) `outputs.len() == 1` containing the `Forget` intent (Phase A prefilter found the trigger), (b) `truncated == BodyTooLarge { body_len: 1048576 + … }`, (c) `llm_eligible_spans` covers the body (LLM still gets enrichment), (d) Phase B was skipped (no user-rule output), (e) one `body exceeds MAX_BODY_LEN_FOR_REGEX, skipping Phase B user rules` warn fires. The earlier-spec failure mode ("oversize body skips text rules entirely") is forbidden.
- **Multi-sentence trigger (round-9 regression):** body `"FYI, old address is stale. forget my old address"`; assert `outputs.len() == 1`, the single output is `Forget(target_text_normalized = "my old address")`, `source_span` covers only the second sentence. The first sentence (no trigger keyword) does not appear in `outputs` and is in `llm_eligible_spans`. Mirror tests: `"One more thing. remember that I prefer cash"`, `"Got it; remember that meetings move to 3 PM"`.
- **Abbreviation safety:** body `"remember that I live in the U.S. and prefer cash"`; assert `outputs.len() == 1` and the captured body is `"I live in the U.S. and prefer cash"` (whole sentence). The prefilter finds one `remember` at position 0; no second hit at `U.S.`; the window runs to end-of-body. Mirror with decimal points (`"remember that prices have rounded to $9.99 today"`) and `"e.g."`.
- **Quote-aware:** body `'the user said "remember that" by mistake'`; assert `outputs` is empty (the trigger is inside quotes and is therefore not eligible).
- **Phrase-window cap:** body containing 70 occurrences of `remember`; assert `outputs.len() <= 64` and a `phrase-window cap reached` warn fires; `llm_eligible_spans` includes the tail beyond hit 64.
- **Span discipline:** every text-rule output has `source_span = Some(_)`; every Hook / ToolFrame output has `source_span = None`. Property test asserts the invariant across `extract` outputs.
- `max_drafts` enforcement: build a `RuleSet` whose **user**-rule list contains `max_drafts + 5` always-matching rules and an empty built-in set; assert `extract` returns exactly `max_drafts` outputs, `truncated == TruncationReason::MaxDrafts`, and emits exactly one `tracing::warn!` (captured via `tracing-test`). Truncation order matches user-rule declaration order.
- `max_wall_ms` zero-output path: empty built-ins, set `budget.max_wall_ms = 0`, one user rule that does work; assert `extract` returns `Err(BudgetExceeded { worker: "regex", elapsed_ms })`.
- `max_wall_ms` partial-output path: empty built-ins, two user rules — first matches, second sleeps past the budget; assert `truncated == TruncationReason::MaxWallMs { .. }` and `outputs.len() == 1`.
- `BodyResolution::NotApplicable`: `Voice` envelope; assert text rules don't run (no error, no outputs from text rules); hook/tool-frame rules still run if the variant carries them.
- `ForgetIntent` shape: `forget` rule fires on `"forget that I mentioned my address"`; assert `target_text_normalized == "i mentioned my address"`, `match_strategy == Substring`, `kind_filter == None`, `source_span` covers the matched portion of the body.
- `ForgetMatchStrategy::Fuzzy` is rejected by `RuleSet::from_config` for user rules (out of scope for this PR; deferred to #75).
- `with_user_rules` rejects a user rule whose `id` collides with a built-in id; returns `InvalidRule`.

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
4. **`ForgetIntent` is structured, not free text.** The brief does not pin a shape for forget intents; this PR formalizes it (normalized target + match strategy + optional kind filter + span) and documents the resolver contract #75 must obey (no silent over-deletion, multi-match → ambiguous outcome).
5. **Two-phase dispatch.** Brief §5.2.a treats the extractor as a single function. We split into Phase A (built-ins, untruncatable) and Phase B (user rules, budget-bounded) so explicit user triggers cannot be lost to truncation. The trait return is unchanged; the split is internal to `RegexExtractor`.

## 12. Workspace impact

- Add `regex = { version = "1", default-features = false, features = ["std", "perf"] }` to `[workspace.dependencies]`. Disabling the `unicode` features keeps the binary slim; add Unicode-aware features only when a built-in rule needs them. CLAUDE.md §6.7 — workspace dep, justified in PR.
- Add `aho-corasick = { version = "1", default-features = false, features = ["std", "perf-literal"] }` to `[workspace.dependencies]`. Powers the trigger prefilter (§6.2). Already a transitive dep of `regex`, so this is effectively a re-export with our chosen features; no new crate added to the build graph.
- Add `async-trait = "0.1"` to `[workspace.dependencies]`. Required because `ExtractorWorker` is held as `Box<dyn ExtractorWorker>` at the chain boundary in #74, and a trait with native `async fn` is not object-safe in Rust 1.95. CLAUDE.md §6.3 explicitly carves this out.
- `cairn-core` opts in to `regex = { workspace = true }`, `aho-corasick = { workspace = true }`, and `async-trait = { workspace = true }`.
- `cargo deny` allowlist already covers MIT/Apache-2.0; all three crates are dual-licensed as such. No `deny.toml` change.
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
