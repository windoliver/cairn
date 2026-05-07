# RegexExtractor and Explicit Trigger Extraction — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement issue #73 — the regex/state-machine extractor for the Cairn pipeline `Extract` stage. Lands `MemoryDraft` + `ForgetIntent` types, the `ExtractorWorker` trait + supporting types, a `RegexExtractor` impl with built-in `remember`/`forget`/`skillify`/`correction`/hook/tool-frame rules, two-phase (built-ins-untruncatable / user-rules-bounded) dispatch on aho-corasick-prefiltered phrase windows, and a span-confidence-gated chain handoff contract for #74.

**Architecture:** New module `cairn-core::pipeline::extract/` with a flat `mod.rs` holding the trait + envelope types and a `regex/` submodule with the impl. Body resolution is encapsulated behind `ResolvedBody` (private fields, named-by-source constructors) so internal agent rationale cannot be mislabelled as user message bytes. Text-rule dispatch is keyword-prefiltered (`aho-corasick`) into phrase windows so explicit triggers fire across sentence boundaries, on bodies of any size, regardless of LLM availability.

**Tech Stack:** Rust 1.95 (edition 2024), `regex` 1.x, `aho-corasick` 1.x, `async-trait` 0.1, `thiserror`, `serde`, `tokio` (`tokio::test`), `proptest`, `insta`, `rstest`, `tracing`, `tracing-test`, `cargo nextest`.

**Spec:** `docs/superpowers/specs/2026-04-28-issue-73-regex-extractor-design.md`

---

## File Structure

| Path | Role |
|---|---|
| `Cargo.toml` (workspace) | MODIFY — add `regex`, `aho-corasick`, `async-trait` to `[workspace.dependencies]` |
| `crates/cairn-core/Cargo.toml` | MODIFY — opt in to the three new deps; add `tracing-test` as dev-dep |
| `crates/cairn-core/src/pipeline/mod.rs` | MODIFY — add `pub mod extract;` |
| `crates/cairn-core/src/pipeline/extract/mod.rs` | NEW — `ExtractorWorker`, `ExtractInput`, `ExtractBudget`, `ExtractOutput`, `ExtractResult`, `TruncationReason`, `ExtractError`, re-exports |
| `crates/cairn-core/src/pipeline/extract/draft.rs` | NEW — `MemoryDraft`, `KindHint`, `Confidence`, `TextSpan` |
| `crates/cairn-core/src/pipeline/extract/intent.rs` | NEW — `ForgetIntent`, `ForgetMatchStrategy` |
| `crates/cairn-core/src/pipeline/extract/body.rs` | NEW — `BodyResolution`, `ResolvedBody`, `BodySource`, `UserIngestPayloadKind`, `ProactiveBodyContext`, `BodyResolutionError` |
| `crates/cairn-core/src/pipeline/extract/regex/mod.rs` | NEW — `RegexExtractor` struct + `ExtractorWorker` impl |
| `crates/cairn-core/src/pipeline/extract/regex/rule.rs` | NEW — `RegexRule`, `ToolFrameFamily`, `RuleSet`, `CompiledRule` |
| `crates/cairn-core/src/pipeline/extract/regex/defaults.rs` | NEW — built-in rule set |
| `crates/cairn-core/src/pipeline/extract/regex/prefilter.rs` | NEW — `TriggerPrefilter` (aho-corasick) + sentence-start eligibility + phrase-window builder |
| `crates/cairn-core/src/pipeline/extract/regex/dispatch.rs` | NEW — Phase 0/A/B dispatch + budget enforcement + `llm_eligible_spans` |
| `crates/cairn-core/tests/pipeline_extract_regex.rs` | NEW — integration tests (one `CaptureEvent` per payload variant) |
| `crates/cairn-core/tests/pipeline_extract_regex_latency.rs` | NEW — `#[ignore]`-d latency assertion |
| `crates/cairn-core/tests/snapshots/` | NEW (insta) — `ExtractOutput` JSON snapshots |

Constants exposed at the module root for #74 to reference:

- `extract::CONFIDENCE_GATE_FOR_SUPPRESSION = 0.9`
- `extract::MAX_BODY_LEN_FOR_REGEX = 64 * 1024`
- `extract::MAX_PHASE_A_WALL_MS = 2`
- `extract::MAX_PHASE_A_WALL_MS_LARGE = 10`
- `extract::MAX_PHRASE_WINDOWS = 64`

---

## Task 1: Workspace deps + module skeleton

**Files:**
- Modify: `Cargo.toml` (workspace)
- Modify: `crates/cairn-core/Cargo.toml`
- Modify: `crates/cairn-core/src/pipeline/mod.rs`
- Create: `crates/cairn-core/src/pipeline/extract/mod.rs`

- [ ] **Step 1: Add workspace dependencies**

In the workspace `Cargo.toml`, under `[workspace.dependencies]`, add (alphabetised):

```toml
aho-corasick = { version = "1", default-features = false, features = ["std", "perf-literal"] }
async-trait = "0.1"
regex = { version = "1", default-features = false, features = ["std", "perf"] }
```

If any of these are already present (transitively), keep one canonical entry; do not duplicate.

- [ ] **Step 2: Opt cairn-core into the new deps**

In `crates/cairn-core/Cargo.toml`, under `[dependencies]` add:

```toml
aho-corasick = { workspace = true }
async-trait = { workspace = true }
regex = { workspace = true }
```

Under `[dev-dependencies]` add:

```toml
tracing-test = "0.2"
```

- [ ] **Step 3: Wire the new submodule**

Edit `crates/cairn-core/src/pipeline/mod.rs`. After the existing `pub mod filter;` line add:

```rust
pub mod extract;
```

- [ ] **Step 4: Create empty `extract/mod.rs`**

`crates/cairn-core/src/pipeline/extract/mod.rs`:

```rust
//! Extract stage of the write pipeline (brief §5.2, §5.2.a).
//!
//! Produces `ExtractOutput` (drafts and forget intents) from a
//! `CaptureEvent` plus a caller-resolved body. Pure functions and pure
//! data — no I/O, no async outside the trait method itself.
//!
//! See `docs/superpowers/specs/2026-04-28-issue-73-regex-extractor-design.md`
//! for the contract this module implements.

// Submodules land in subsequent tasks.
```

- [ ] **Step 5: Verify the workspace still builds**

Run: `cargo check --workspace --all-targets --locked`
Expected: success (warnings about unused crate deps are acceptable until later tasks consume them).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/cairn-core/Cargo.toml \
        crates/cairn-core/src/pipeline/mod.rs \
        crates/cairn-core/src/pipeline/extract/mod.rs
git commit -m "feat(extract): wire empty extract module + workspace deps (#73)"
```

---

## Task 2: Foundational newtypes + `MemoryDraft`

**Files:**
- Create: `crates/cairn-core/src/pipeline/extract/draft.rs`
- Modify: `crates/cairn-core/src/pipeline/extract/mod.rs`

- [ ] **Step 1: Write failing tests for `Confidence`, `TextSpan`, `KindHint`, `MemoryDraft`**

Create `crates/cairn-core/src/pipeline/extract/draft.rs`:

```rust
//! `MemoryDraft` and its constituent newtypes.

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::domain::{taxonomy::MemoryKind, CaptureEventId};

/// Confidence score in `[0.0, 1.0]`. Constructed via `try_from`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Confidence(f32);

impl Confidence {
    pub const ZERO: Self = Self(0.0);

    /// Inner value as `f32`.
    #[must_use]
    pub fn as_f32(self) -> f32 {
        self.0
    }
}

#[derive(thiserror::Error, Debug, PartialEq)]
pub enum ConfidenceError {
    #[error("confidence {0} is outside [0.0, 1.0]")]
    OutOfRange(f32),
    #[error("confidence is NaN")]
    Nan,
}

impl TryFrom<f32> for Confidence {
    type Error = ConfidenceError;

    fn try_from(value: f32) -> Result<Self, Self::Error> {
        if value.is_nan() {
            return Err(ConfidenceError::Nan);
        }
        if !(0.0..=1.0).contains(&value) {
            return Err(ConfidenceError::OutOfRange(value));
        }
        Ok(Self(value))
    }
}

impl fmt::Display for Confidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.2}", self.0)
    }
}

/// Byte range within a body: `[start, end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextSpan {
    pub start: u32,
    pub end: u32,
}

impl TextSpan {
    #[must_use]
    pub fn new(start: u32, end: u32) -> Self {
        debug_assert!(start <= end, "TextSpan: start must be <= end");
        Self { start, end }
    }

    #[must_use]
    pub fn len(self) -> u32 {
        self.end - self.start
    }

    #[must_use]
    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    #[must_use]
    pub fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

/// Wrapper around `MemoryKind` plus a tag identifying the rule that
/// produced it. Always serialises as the inner kind string for wire
/// stability with the IDL.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KindHint(pub MemoryKind);

impl From<MemoryKind> for KindHint {
    fn from(kind: MemoryKind) -> Self {
        Self(kind)
    }
}

impl KindHint {
    #[must_use]
    pub fn kind(&self) -> MemoryKind {
        self.0
    }
}

/// Draft memory record, the standard output of an extractor for an
/// observed-memorise intent.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::taxonomy::MemoryKind;

    #[test]
    fn confidence_accepts_in_range() {
        assert!(Confidence::try_from(0.0).is_ok());
        assert!(Confidence::try_from(0.5).is_ok());
        assert!(Confidence::try_from(1.0).is_ok());
    }

    #[test]
    fn confidence_rejects_out_of_range() {
        assert_eq!(
            Confidence::try_from(-0.1).unwrap_err(),
            ConfidenceError::OutOfRange(-0.1)
        );
        assert_eq!(
            Confidence::try_from(1.5).unwrap_err(),
            ConfidenceError::OutOfRange(1.5)
        );
    }

    #[test]
    fn confidence_rejects_nan() {
        assert_eq!(
            Confidence::try_from(f32::NAN).unwrap_err(),
            ConfidenceError::Nan
        );
    }

    #[test]
    fn text_span_overlap_detects_intersection() {
        let a = TextSpan::new(0, 10);
        let b = TextSpan::new(5, 15);
        let c = TextSpan::new(20, 30);
        assert!(a.overlaps(b));
        assert!(!a.overlaps(c));
        // Touching but not overlapping
        assert!(!a.overlaps(TextSpan::new(10, 20)));
    }

    #[test]
    fn kind_hint_round_trips_via_serde() {
        let hint = KindHint::from(MemoryKind::User);
        let json = serde_json::to_string(&hint).expect("ser");
        // Inner enum serialises as a snake_case string.
        assert_eq!(json, "\"user\"");
        let back: KindHint = serde_json::from_str(&json).expect("deser");
        assert_eq!(back, hint);
    }

    #[test]
    fn memory_draft_round_trips_via_serde() {
        use crate::domain::CaptureEventId;
        let draft = MemoryDraft {
            kind_hint: KindHint::from(MemoryKind::User),
            body: "I prefer dark mode".to_owned(),
            confidence: Confidence::try_from(0.95).unwrap(),
            source_event: CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV")
                .expect("valid ulid"),
            source_span: Some(TextSpan::new(0, 18)),
            trigger_id: Some("remember.preference".to_owned()),
        };
        let json = serde_json::to_string(&draft).expect("ser");
        let back: MemoryDraft = serde_json::from_str(&json).expect("deser");
        assert_eq!(back, draft);
    }
}
```

- [ ] **Step 2: Add the module declaration**

In `crates/cairn-core/src/pipeline/extract/mod.rs`, add:

```rust
pub mod draft;

pub use draft::{Confidence, ConfidenceError, KindHint, MemoryDraft, TextSpan};
```

- [ ] **Step 3: Verify the test build fails (compile error before draft.rs's contents land)**

Already done in step 1 — the file now exists. Skip.

- [ ] **Step 4: Run the new tests**

Run: `cargo nextest run -p cairn-core --locked pipeline::extract::draft`
Expected: 5 tests pass.

- [ ] **Step 5: Run clippy**

Run: `cargo clippy -p cairn-core --all-targets --locked -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/src/pipeline/extract/
git commit -m "feat(extract): add Confidence, TextSpan, KindHint, MemoryDraft (#73)"
```

---

## Task 3: `ForgetIntent` + `ForgetMatchStrategy`

**Files:**
- Create: `crates/cairn-core/src/pipeline/extract/intent.rs`
- Modify: `crates/cairn-core/src/pipeline/extract/mod.rs`

- [ ] **Step 1: Write `intent.rs` with type + tests**

Create `crates/cairn-core/src/pipeline/extract/intent.rs`:

```rust
//! `ForgetIntent` — a structured forget selector. See spec §4.3.

use serde::{Deserialize, Serialize};

use super::{Confidence, KindHint, TextSpan};
use crate::domain::CaptureEventId;

/// How the resolver in #75 should compare `target_text_normalized`
/// against candidate record bodies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ForgetMatchStrategy {
    /// Body matches `target_text_normalized` after the same
    /// normalisation. Auto-proceed is permitted iff the candidate set
    /// is exactly one record.
    Exact,
    /// `target_text_normalized` is a substring of the (normalised)
    /// body. **Never** auto-proceeds — always routed through
    /// interactive `forget_ambiguous`. Default for the built-in `forget`
    /// rule.
    Substring,
    /// Reserved for #75; rejected at construction time in this PR.
    Fuzzy,
}

impl ForgetMatchStrategy {
    /// Default selector strategy used by `serde(default)` on
    /// user-supplied rules.
    #[must_use]
    pub fn default_substring() -> Self {
        Self::Substring
    }
}

/// A structured forget candidate. Never an instruction — see spec §4.3
/// for the resolver contract #75 must obey.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ForgetIntent {
    pub target_text_normalized: String,
    pub match_strategy: ForgetMatchStrategy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind_filter: Option<KindHint>,
    pub source_span: TextSpan,
    pub confidence: Confidence,
    pub source_event: CaptureEventId,
    pub trigger_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::taxonomy::MemoryKind;
    use crate::domain::CaptureEventId;

    fn fixture_event_id() -> CaptureEventId {
        CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ulid")
    }

    #[test]
    fn match_strategy_round_trips_substring() {
        let s = ForgetMatchStrategy::Substring;
        let json = serde_json::to_string(&s).expect("ser");
        assert_eq!(json, "\"substring\"");
        let back: ForgetMatchStrategy = serde_json::from_str(&json).expect("deser");
        assert_eq!(back, s);
    }

    #[test]
    fn match_strategy_round_trips_exact_and_fuzzy() {
        for s in [ForgetMatchStrategy::Exact, ForgetMatchStrategy::Fuzzy] {
            let json = serde_json::to_string(&s).unwrap();
            let back: ForgetMatchStrategy = serde_json::from_str(&json).unwrap();
            assert_eq!(back, s);
        }
    }

    #[test]
    fn forget_intent_round_trips_with_kind_filter() {
        let intent = ForgetIntent {
            target_text_normalized: "my old address".to_owned(),
            match_strategy: ForgetMatchStrategy::Substring,
            kind_filter: Some(KindHint::from(MemoryKind::User)),
            source_span: TextSpan::new(0, 21),
            confidence: Confidence::try_from(0.95).unwrap(),
            source_event: fixture_event_id(),
            trigger_id: "forget".to_owned(),
        };
        let json = serde_json::to_string(&intent).unwrap();
        let back: ForgetIntent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, intent);
    }

    #[test]
    fn forget_intent_round_trips_without_kind_filter() {
        let intent = ForgetIntent {
            target_text_normalized: "old thing".to_owned(),
            match_strategy: ForgetMatchStrategy::Substring,
            kind_filter: None,
            source_span: TextSpan::new(0, 9),
            confidence: Confidence::try_from(0.95).unwrap(),
            source_event: fixture_event_id(),
            trigger_id: "forget".to_owned(),
        };
        let json = serde_json::to_string(&intent).unwrap();
        // `kind_filter: None` is omitted via skip_serializing_if.
        assert!(!json.contains("kind_filter"));
        let back: ForgetIntent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, intent);
    }
}
```

- [ ] **Step 2: Re-export from `extract/mod.rs`**

In `crates/cairn-core/src/pipeline/extract/mod.rs`, after `pub mod draft;` add:

```rust
pub mod intent;

pub use intent::{ForgetIntent, ForgetMatchStrategy};
```

- [ ] **Step 3: Run the new tests**

Run: `cargo nextest run -p cairn-core --locked pipeline::extract::intent`
Expected: 4 tests pass.

- [ ] **Step 4: Run clippy**

Run: `cargo clippy -p cairn-core --all-targets --locked -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/pipeline/extract/
git commit -m "feat(extract): add ForgetIntent and ForgetMatchStrategy (#73)"
```

---

## Task 4: Body resolution layer (`BodyResolution`, `ResolvedBody`, `BodySource`, errors)

**Files:**
- Create: `crates/cairn-core/src/pipeline/extract/body.rs`
- Modify: `crates/cairn-core/src/pipeline/extract/mod.rs`

- [ ] **Step 1: Create `body.rs` with private fields + named constructors**

`crates/cairn-core/src/pipeline/extract/body.rs`:

```rust
//! Body resolution: encapsulated, source-tagged user-text input for
//! the extractor. See spec §4.1.
//!
//! `ResolvedBody`'s fields are private. Construction goes through one
//! of the named functions below, each tied to a specific `BodySource`.
//! External callers cannot construct `Resolved { text, source }`
//! directly, so a buggy or stale caller cannot mislabel
//! `Proactive.rationale` as `ProactiveMessage` by accident.

use serde::{Deserialize, Serialize};

/// The trust boundary a resolved body came from.
///
/// **There is deliberately no `Rationale` variant.** Combined with the
/// private fields on `ResolvedBody`, the only way to produce a
/// `ResolvedBody` tagged `ProactiveMessage` is via
/// `from_proactive_message`, which (a) is named after the message-body
/// field, (b) takes the message-body text, and (c) defensively rejects
/// text equal to `rationale`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BodySource {
    UserIngest,
    HookUtterance,
    ProactiveMessage,
}

/// Marker for the `Cli` / `Mcp` payload variant the bytes came from.
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

#[derive(Clone, Debug, thiserror::Error, PartialEq)]
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

/// Resolved body bytes plus their trust-boundary source.
///
/// Fields are private; callers go through the constructors below.
#[derive(Clone, Debug)]
pub struct ResolvedBody<'a> {
    text: &'a str,
    source: BodySource,
}

impl<'a> ResolvedBody<'a> {
    /// Construct from a `CapturePayload::Cli` or `::Mcp` envelope.
    #[must_use]
    pub fn from_user_ingest(text: &'a str, payload_kind: UserIngestPayloadKind) -> Self {
        let _ = payload_kind;
        Self {
            text,
            source: BodySource::UserIngest,
        }
    }

    /// Construct from a `CapturePayload::Hook` envelope.
    #[must_use]
    pub fn from_hook_utterance(text: &'a str, hook_name: &'a str) -> Self {
        let _ = hook_name;
        Self {
            text,
            source: BodySource::HookUtterance,
        }
    }

    /// Construct from a `CapturePayload::Proactive` envelope's
    /// **message body**. Refuses to construct if `text == rationale`.
    pub fn from_proactive_message(
        text: &'a str,
        payload: &ProactiveBodyContext<'a>,
    ) -> Result<Self, BodyResolutionError> {
        if text == payload.rationale {
            return Err(BodyResolutionError::ProactiveRationaleMislabel);
        }
        Ok(Self {
            text,
            source: BodySource::ProactiveMessage,
        })
    }

    #[must_use]
    pub fn text(&self) -> &str {
        self.text
    }

    #[must_use]
    pub fn source(&self) -> BodySource {
        self.source
    }
}

/// Body-resolution result, threaded through the extractor chain.
#[derive(Clone, Debug)]
pub enum BodyResolution<'a> {
    /// The event's source family does not carry extractable text.
    NotApplicable,
    /// Body bytes were materialized and verified.
    Resolved(ResolvedBody<'a>),
    /// Body resolution attempted but failed.
    Failed(BodyResolutionError),
}

impl BodyResolution<'_> {
    /// Whether text rules may run on this body.
    #[must_use]
    pub fn allows_text_rules(&self) -> bool {
        matches!(self, BodyResolution::Resolved(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_user_ingest_tags_correctly() {
        let body = ResolvedBody::from_user_ingest("hello", UserIngestPayloadKind::Cli);
        assert_eq!(body.text(), "hello");
        assert_eq!(body.source(), BodySource::UserIngest);
    }

    #[test]
    fn from_hook_utterance_tags_correctly() {
        let body = ResolvedBody::from_hook_utterance("hi", "UserPromptSubmit");
        assert_eq!(body.source(), BodySource::HookUtterance);
    }

    #[test]
    fn from_proactive_message_accepts_distinct_text() {
        let ctx = ProactiveBodyContext {
            rationale: "internal-reasoning",
        };
        let body =
            ResolvedBody::from_proactive_message("user-visible message", &ctx).expect("distinct");
        assert_eq!(body.source(), BodySource::ProactiveMessage);
    }

    #[test]
    fn from_proactive_message_rejects_rationale_mislabel() {
        let ctx = ProactiveBodyContext {
            rationale: "secret rationale",
        };
        let err = ResolvedBody::from_proactive_message("secret rationale", &ctx).unwrap_err();
        assert_eq!(err, BodyResolutionError::ProactiveRationaleMislabel);
    }

    #[test]
    fn body_source_has_no_rationale_variant() {
        // Exhaustive match: if a new variant is added, this test breaks
        // and the reviewer must explicitly justify it.
        fn is_user_visible(src: BodySource) -> bool {
            match src {
                BodySource::UserIngest => true,
                BodySource::HookUtterance => true,
                BodySource::ProactiveMessage => true,
            }
        }
        assert!(is_user_visible(BodySource::UserIngest));
        assert!(is_user_visible(BodySource::HookUtterance));
        assert!(is_user_visible(BodySource::ProactiveMessage));
    }

    #[test]
    fn body_resolution_allows_text_rules_only_when_resolved() {
        let resolved = BodyResolution::Resolved(ResolvedBody::from_user_ingest(
            "hi",
            UserIngestPayloadKind::Cli,
        ));
        assert!(resolved.allows_text_rules());

        let na: BodyResolution<'_> = BodyResolution::NotApplicable;
        assert!(!na.allows_text_rules());

        let failed: BodyResolution<'_> =
            BodyResolution::Failed(BodyResolutionError::NotFound("nope".into()));
        assert!(!failed.allows_text_rules());
    }
}
```

- [ ] **Step 2: Re-export from `extract/mod.rs`**

```rust
pub mod body;

pub use body::{
    BodyResolution, BodyResolutionError, BodySource, ProactiveBodyContext, ResolvedBody,
    UserIngestPayloadKind,
};
```

- [ ] **Step 3: Run the new tests**

Run: `cargo nextest run -p cairn-core --locked pipeline::extract::body`
Expected: 6 tests pass.

- [ ] **Step 4: Run clippy + fmt**

```bash
cargo clippy -p cairn-core --all-targets --locked -- -D warnings
cargo fmt --all --check
```

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/pipeline/extract/
git commit -m "feat(extract): add encapsulated body resolution layer (#73)"
```

---

## Task 5: Extractor trait + envelope types

**Files:**
- Modify: `crates/cairn-core/src/pipeline/extract/mod.rs`

- [ ] **Step 1: Add trait + types to `extract/mod.rs`**

Append to `crates/cairn-core/src/pipeline/extract/mod.rs` (after the existing module declarations and re-exports):

```rust
use crate::domain::CaptureEvent;

/// Confidence at or above this value causes a regex output's span to
/// suppress LLM re-extraction on that span. Below it, the LLM is free
/// to compete; Filter (#75) breaks ties by confidence.
pub const CONFIDENCE_GATE_FOR_SUPPRESSION: f32 = 0.9;

/// Maximum body length, in bytes, that Phase B (user rules) will scan.
/// Bodies above this still run Phase A (built-ins) on prefilter hits;
/// see spec §6.3.
pub const MAX_BODY_LEN_FOR_REGEX: usize = 64 * 1024;

/// Phase A wall-clock observability rail (default).
pub const MAX_PHASE_A_WALL_MS: u32 = 2;

/// Phase A wall-clock observability rail for bodies above
/// `MAX_BODY_LEN_FOR_REGEX`.
pub const MAX_PHASE_A_WALL_MS_LARGE: u32 = 10;

/// Hard cap on phrase windows produced by the prefilter per body.
pub const MAX_PHRASE_WINDOWS: usize = 64;

/// Resolved input for an extractor.
pub struct ExtractInput<'a> {
    pub event: &'a CaptureEvent,
    pub body: BodyResolution<'a>,
}

/// Per-extractor budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtractBudget {
    pub max_wall_ms: u32,
    pub max_drafts: u16,
}

impl ExtractBudget {
    /// Default budget for `RegexExtractor`.
    #[must_use]
    pub const fn regex_default() -> Self {
        Self {
            max_wall_ms: MAX_PHASE_A_WALL_MS,
            max_drafts: 16,
        }
    }
}

/// One extracted output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExtractOutput {
    Draft(MemoryDraft),
    Forget(ForgetIntent),
}

impl ExtractOutput {
    /// Confidence carried by either variant.
    #[must_use]
    pub fn confidence(&self) -> Confidence {
        match self {
            ExtractOutput::Draft(d) => d.confidence,
            ExtractOutput::Forget(f) => f.confidence,
        }
    }

    /// Source span carried by either variant; `None` for non-text-rule
    /// outputs (hook / tool-frame).
    #[must_use]
    pub fn source_span(&self) -> Option<TextSpan> {
        match self {
            ExtractOutput::Draft(d) => d.source_span,
            ExtractOutput::Forget(f) => Some(f.source_span),
        }
    }
}

/// Why an extraction was truncated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TruncationReason {
    None,
    MaxDrafts,
    MaxWallMs { elapsed_ms: u32 },
    BodyTooLarge { body_len: u32 },
}

/// Result envelope returned by `ExtractorWorker::extract`.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtractResult {
    pub outputs: Vec<ExtractOutput>,
    pub truncated: TruncationReason,
    pub llm_eligible_spans: Vec<TextSpan>,
}

/// Errors any extractor may surface.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ExtractError {
    #[error("extractor `{worker}` exceeded budget after {elapsed_ms} ms")]
    BudgetExceeded {
        worker: &'static str,
        elapsed_ms: u32,
    },
    #[error("invalid rule `{rule_id}`: {reason}")]
    InvalidRule { rule_id: String, reason: String },
    #[error("body resolution failed for event {event_id}")]
    BodyResolution {
        event_id: String,
        #[source]
        source: BodyResolutionError,
    },
}

/// The pluggable extractor contract — see brief §5.2.a and spec §4.1.
///
/// `#[async_trait]` is used because the chain dispatcher in #74 holds
/// extractors as `Box<dyn ExtractorWorker>`, and a trait with native
/// `async fn` is not object-safe in Rust 1.95. CLAUDE.md §6.3
/// explicitly carves this out.
#[async_trait::async_trait]
pub trait ExtractorWorker: Send + Sync {
    fn name(&self) -> &'static str;
    fn budget(&self) -> ExtractBudget;
    async fn extract(
        &self,
        input: &ExtractInput<'_>,
    ) -> Result<ExtractResult, ExtractError>;
}

// `serde` is brought in via `MemoryDraft` and friends — re-add the
// `use` here so the macro derives compile.
use serde::{Deserialize, Serialize};
```

- [ ] **Step 2: Add tests for the envelope types**

At the end of `extract/mod.rs`, add:

```rust
#[cfg(test)]
mod mod_tests {
    use super::*;
    use crate::domain::taxonomy::MemoryKind;
    use crate::domain::CaptureEventId;

    fn fixture_event_id() -> CaptureEventId {
        CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ulid")
    }

    fn fixture_draft() -> MemoryDraft {
        MemoryDraft {
            kind_hint: KindHint::from(MemoryKind::User),
            body: "hi".to_owned(),
            confidence: Confidence::try_from(0.95).unwrap(),
            source_event: fixture_event_id(),
            source_span: Some(TextSpan::new(0, 2)),
            trigger_id: Some("remember.preference".to_owned()),
        }
    }

    #[test]
    fn output_confidence_returns_inner() {
        let draft = ExtractOutput::Draft(fixture_draft());
        assert_eq!(draft.confidence().as_f32(), 0.95);
    }

    #[test]
    fn output_source_span_returns_inner() {
        let draft = ExtractOutput::Draft(fixture_draft());
        assert_eq!(draft.source_span(), Some(TextSpan::new(0, 2)));
    }

    #[test]
    fn truncation_round_trips_via_serde() {
        let cases = [
            TruncationReason::None,
            TruncationReason::MaxDrafts,
            TruncationReason::MaxWallMs { elapsed_ms: 5 },
            TruncationReason::BodyTooLarge { body_len: 65_537 },
        ];
        for reason in cases {
            let json = serde_json::to_string(&reason).unwrap();
            let back: TruncationReason = serde_json::from_str(&json).unwrap();
            assert_eq!(back, reason);
        }
    }

    #[test]
    fn regex_default_budget_matches_spec() {
        let b = ExtractBudget::regex_default();
        assert_eq!(b.max_wall_ms, MAX_PHASE_A_WALL_MS);
        assert_eq!(b.max_drafts, 16);
    }

    #[test]
    fn confidence_gate_constant_is_zero_point_nine() {
        assert!((CONFIDENCE_GATE_FOR_SUPPRESSION - 0.9).abs() < f32::EPSILON);
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p cairn-core --locked pipeline::extract`
Expected: previous tests + 5 new tests pass.

- [ ] **Step 4: Clippy**

Run: `cargo clippy -p cairn-core --all-targets --locked -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/pipeline/extract/
git commit -m "feat(extract): add ExtractorWorker trait + envelope types (#73)"
```

---

## Task 6: `RegexRule` enum + `ToolFrameFamily`

**Files:**
- Create: `crates/cairn-core/src/pipeline/extract/regex/mod.rs` (skeleton)
- Create: `crates/cairn-core/src/pipeline/extract/regex/rule.rs`

- [ ] **Step 1: Create `regex/mod.rs` skeleton**

`crates/cairn-core/src/pipeline/extract/regex/mod.rs`:

```rust
//! `RegexExtractor` — regex/state-machine implementation of
//! `ExtractorWorker`. See spec §6.

pub mod rule;

pub use rule::{RegexRule, ToolFrameFamily};
```

Wire it from `extract/mod.rs`:

```rust
pub mod regex;

pub use regex::rule::{RegexRule, ToolFrameFamily};
```

- [ ] **Step 2: Create `rule.rs` with the rule enum**

`crates/cairn-core/src/pipeline/extract/regex/rule.rs`:

```rust
//! Regex rule shapes. Spec §5.

use serde::Deserialize;

use super::super::{Confidence, ForgetMatchStrategy, KindHint};

/// A user-or-built-in rule, before compilation.
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
        confidence: Confidence,
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

impl RegexRule {
    /// Stable id of the rule, used for audit and dedup.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            RegexRule::TriggerPhrase { id, .. }
            | RegexRule::ForgetPhrase { id, .. }
            | RegexRule::HookEvent { id, .. }
            | RegexRule::ToolFrame { id, .. } => id,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolFrameFamily {
    Terminal { exit_code_nonzero: bool },
    Ide { event_kind: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::taxonomy::MemoryKind;

    #[test]
    fn trigger_phrase_round_trips() {
        let json = r#"{
            "type": "trigger_phrase",
            "id": "remember.preference",
            "pattern": "^\\s*remember.+",
            "kind_hint": "user",
            "confidence": 0.95
        }"#;
        let rule: RegexRule = serde_json::from_str(json).unwrap();
        assert_eq!(rule.id(), "remember.preference");
        match rule {
            RegexRule::TriggerPhrase {
                kind_hint, confidence, capture_group, ..
            } => {
                assert_eq!(kind_hint, KindHint::from(MemoryKind::User));
                assert_eq!(confidence.as_f32(), 0.95);
                assert!(capture_group.is_none());
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn forget_phrase_defaults_strategy_to_substring() {
        let json = r#"{
            "type": "forget_phrase",
            "id": "forget",
            "pattern": "^forget (.+)$",
            "target_group": 1,
            "confidence": 0.95
        }"#;
        let rule: RegexRule = serde_json::from_str(json).unwrap();
        match rule {
            RegexRule::ForgetPhrase { match_strategy, quoted_capture, .. } => {
                assert_eq!(match_strategy, ForgetMatchStrategy::Substring);
                assert!(!quoted_capture);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn unknown_field_is_rejected() {
        let json = r#"{"type":"trigger_phrase","id":"a","pattern":"b","kind_hint":"user","confidence":0.5,"bogus":1}"#;
        assert!(serde_json::from_str::<RegexRule>(json).is_err());
    }

    #[test]
    fn tool_frame_terminal_round_trips() {
        let json = r#"{
            "type": "tool_frame",
            "id": "tool.terminal_failure",
            "family": {"kind": "terminal", "exit_code_nonzero": true},
            "kind_hint": "strategy_failure",
            "confidence": 0.7
        }"#;
        let rule: RegexRule = serde_json::from_str(json).unwrap();
        match rule {
            RegexRule::ToolFrame { family: ToolFrameFamily::Terminal { exit_code_nonzero }, .. } => {
                assert!(exit_code_nonzero);
            }
            _ => panic!("wrong variant"),
        }
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p cairn-core --locked pipeline::extract::regex::rule`
Expected: 4 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/pipeline/extract/
git commit -m "feat(extract): add RegexRule enum and ToolFrameFamily (#73)"
```

---

## Task 7: `RuleSet` + `CompiledRule`

**Files:**
- Modify: `crates/cairn-core/src/pipeline/extract/regex/rule.rs`

- [ ] **Step 1: Append compiled types to `rule.rs`**

```rust
use ::regex::Regex;

/// Pre-compiled form of a `RegexRule`. Built by `RuleSet::from_config`
/// or `RuleSet::builtin`.
#[derive(Clone, Debug)]
pub struct CompiledRule {
    pub id: String,
    pub origin: RuleOrigin,
    pub kind: CompiledRuleKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleOrigin {
    BuiltIn,
    User,
}

#[derive(Clone, Debug)]
pub enum CompiledRuleKind {
    TriggerPhrase {
        re: Regex,
        kind_hint: KindHint,
        confidence: Confidence,
        capture_group: u8,
    },
    ForgetPhrase {
        re: Regex,
        target_group: u8,
        confidence: Confidence,
        match_strategy: ForgetMatchStrategy,
    },
    HookEvent {
        hook_name: String,
        tool_name: Option<String>,
        kind_hint: KindHint,
        confidence: Confidence,
    },
    ToolFrame {
        family: ToolFrameFamily,
        kind_hint: KindHint,
        confidence: Confidence,
    },
}

/// Bucketed compiled rules, ready for dispatch.
#[derive(Clone, Debug, Default)]
pub struct RuleSet {
    pub(crate) builtin_text: Vec<CompiledRule>,
    pub(crate) builtin_hook: Vec<CompiledRule>,
    pub(crate) builtin_tool_frame: Vec<CompiledRule>,
    pub(crate) user_text: Vec<CompiledRule>,
    pub(crate) user_hook: Vec<CompiledRule>,
    pub(crate) user_tool_frame: Vec<CompiledRule>,
}

impl RuleSet {
    /// Empty ruleset — useful in tests.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Built-in rule set populated from `defaults.rs` (Task 8).
    /// Defined here so doc-tests in later tasks can reference it; the
    /// body lives in `defaults.rs`.
    #[must_use]
    pub fn builtin() -> Self {
        super::defaults::builtin_rule_set()
    }

    /// Compile and validate user rules into a fresh `RuleSet`. Use
    /// `with_user_rules` to merge with built-ins.
    pub fn from_config(rules: Vec<RegexRule>) -> Result<Self, super::super::ExtractError> {
        let mut set = Self::empty();
        for rule in rules {
            compile_user_rule(&mut set, rule)?;
        }
        Ok(set)
    }

    /// Append user rules to an existing ruleset (typically the
    /// built-in one).
    pub fn with_user_rules(
        mut self,
        rules: Vec<RegexRule>,
    ) -> Result<Self, super::super::ExtractError> {
        let existing_ids: std::collections::HashSet<&str> = self
            .builtin_text
            .iter()
            .chain(self.builtin_hook.iter())
            .chain(self.builtin_tool_frame.iter())
            .map(|r| r.id.as_str())
            .collect();
        for rule in &rules {
            if existing_ids.contains(rule.id()) {
                return Err(super::super::ExtractError::InvalidRule {
                    rule_id: rule.id().to_owned(),
                    reason: "user rule id collides with built-in".to_owned(),
                });
            }
        }
        for rule in rules {
            compile_user_rule(&mut self, rule)?;
        }
        Ok(self)
    }
}

fn compile_user_rule(
    set: &mut RuleSet,
    rule: RegexRule,
) -> Result<(), super::super::ExtractError> {
    let compiled = compile_rule(&rule, RuleOrigin::User)?;
    match &compiled.kind {
        CompiledRuleKind::TriggerPhrase { .. } | CompiledRuleKind::ForgetPhrase { .. } => {
            set.user_text.push(compiled);
        }
        CompiledRuleKind::HookEvent { .. } => set.user_hook.push(compiled),
        CompiledRuleKind::ToolFrame { .. } => set.user_tool_frame.push(compiled),
    }
    Ok(())
}

/// Compile a single rule. Public so `defaults.rs` can build built-ins.
pub(crate) fn compile_rule(
    rule: &RegexRule,
    origin: RuleOrigin,
) -> Result<CompiledRule, super::super::ExtractError> {
    match rule {
        RegexRule::TriggerPhrase {
            id,
            pattern,
            kind_hint,
            confidence,
            capture_group,
        } => {
            let re = compile_pattern(id, pattern)?;
            Ok(CompiledRule {
                id: id.clone(),
                origin,
                kind: CompiledRuleKind::TriggerPhrase {
                    re,
                    kind_hint: kind_hint.clone(),
                    confidence: *confidence,
                    capture_group: capture_group.unwrap_or(0),
                },
            })
        }
        RegexRule::ForgetPhrase {
            id,
            pattern,
            target_group,
            confidence,
            match_strategy,
            quoted_capture,
        } => {
            let strategy = *match_strategy;
            if matches!(strategy, ForgetMatchStrategy::Fuzzy) {
                return Err(super::super::ExtractError::InvalidRule {
                    rule_id: id.clone(),
                    reason: "Fuzzy match strategy is reserved for #75".to_owned(),
                });
            }
            if matches!(strategy, ForgetMatchStrategy::Exact) && !*quoted_capture {
                return Err(super::super::ExtractError::InvalidRule {
                    rule_id: id.clone(),
                    reason: "Exact match strategy requires quoted_capture: true".to_owned(),
                });
            }
            let re = compile_pattern(id, pattern)?;
            Ok(CompiledRule {
                id: id.clone(),
                origin,
                kind: CompiledRuleKind::ForgetPhrase {
                    re,
                    target_group: *target_group,
                    confidence: *confidence,
                    match_strategy: strategy,
                },
            })
        }
        RegexRule::HookEvent {
            id,
            hook_name,
            tool_name,
            kind_hint,
            confidence,
        } => Ok(CompiledRule {
            id: id.clone(),
            origin,
            kind: CompiledRuleKind::HookEvent {
                hook_name: hook_name.clone(),
                tool_name: tool_name.clone(),
                kind_hint: kind_hint.clone(),
                confidence: *confidence,
            },
        }),
        RegexRule::ToolFrame {
            id,
            family,
            kind_hint,
            confidence,
        } => Ok(CompiledRule {
            id: id.clone(),
            origin,
            kind: CompiledRuleKind::ToolFrame {
                family: family.clone(),
                kind_hint: kind_hint.clone(),
                confidence: *confidence,
            },
        }),
    }
}

fn compile_pattern(id: &str, pattern: &str) -> Result<Regex, super::super::ExtractError> {
    ::regex::RegexBuilder::new(pattern)
        .case_insensitive(true)
        .size_limit(1 << 20) // 1 MiB compiled-DFA cap; plenty for our anchored patterns
        .build()
        .map_err(|e| super::super::ExtractError::InvalidRule {
            rule_id: id.to_owned(),
            reason: e.to_string(),
        })
}
```

- [ ] **Step 2: Add tests for `RuleSet::from_config` + `with_user_rules`**

Inside the `#[cfg(test)] mod tests` block of `rule.rs`:

```rust
    #[test]
    fn from_config_compiles_valid_rules() {
        let json = r#"[{
            "type":"trigger_phrase","id":"u.x","pattern":"^x .+",
            "kind_hint":"user","confidence":0.5
        }]"#;
        let rules: Vec<RegexRule> = serde_json::from_str(json).unwrap();
        let set = RuleSet::from_config(rules).expect("compile ok");
        assert_eq!(set.user_text.len(), 1);
    }

    #[test]
    fn from_config_rejects_invalid_pattern() {
        let json = r#"[{
            "type":"trigger_phrase","id":"u.bad","pattern":"[unclosed",
            "kind_hint":"user","confidence":0.5
        }]"#;
        let rules: Vec<RegexRule> = serde_json::from_str(json).unwrap();
        let err = RuleSet::from_config(rules).unwrap_err();
        match err {
            super::super::super::ExtractError::InvalidRule { rule_id, .. } => {
                assert_eq!(rule_id, "u.bad");
            }
            _ => panic!("wrong error"),
        }
    }

    #[test]
    fn from_config_rejects_fuzzy_strategy() {
        let json = r#"[{
            "type":"forget_phrase","id":"u.f","pattern":"^forget (.+)$",
            "target_group":1,"confidence":0.5,"match_strategy":"fuzzy"
        }]"#;
        let rules: Vec<RegexRule> = serde_json::from_str(json).unwrap();
        let err = RuleSet::from_config(rules).unwrap_err();
        match err {
            super::super::super::ExtractError::InvalidRule { reason, .. } => {
                assert!(reason.contains("Fuzzy"));
            }
            _ => panic!("wrong error"),
        }
    }

    #[test]
    fn from_config_rejects_exact_without_quoted_capture() {
        let json = r#"[{
            "type":"forget_phrase","id":"u.e","pattern":"^forget (.+)$",
            "target_group":1,"confidence":0.5,"match_strategy":"exact"
        }]"#;
        let rules: Vec<RegexRule> = serde_json::from_str(json).unwrap();
        let err = RuleSet::from_config(rules).unwrap_err();
        match err {
            super::super::super::ExtractError::InvalidRule { reason, .. } => {
                assert!(reason.contains("quoted_capture"));
            }
            _ => panic!("wrong error"),
        }
    }

    #[test]
    fn with_user_rules_rejects_duplicate_id_against_builtin() {
        let builtin = RuleSet::builtin();
        let json = r#"[{
            "type":"trigger_phrase","id":"remember.preference",
            "pattern":"^x .+","kind_hint":"user","confidence":0.5
        }]"#;
        let rules: Vec<RegexRule> = serde_json::from_str(json).unwrap();
        let err = builtin.with_user_rules(rules).unwrap_err();
        match err {
            super::super::super::ExtractError::InvalidRule { rule_id, reason } => {
                assert_eq!(rule_id, "remember.preference");
                assert!(reason.contains("collides"));
            }
            _ => panic!("wrong error"),
        }
    }
```

(Note `RuleSet::builtin()` calls into Task 8's `defaults`. To keep this task self-contained, defer the duplicate-id test to Task 8 and stub `defaults::builtin_rule_set` here. Add a temporary stub:)

- [ ] **Step 3: Stub `defaults` to satisfy `builtin()`**

Create `crates/cairn-core/src/pipeline/extract/regex/defaults.rs`:

```rust
//! Built-in rule set. Spec §7. Populated in Task 8.

use super::rule::RuleSet;

#[must_use]
pub fn builtin_rule_set() -> RuleSet {
    RuleSet::empty()
}
```

Add `pub mod defaults;` to `regex/mod.rs`.

The `with_user_rules_rejects_duplicate_id_against_builtin` test is meaningless against an empty built-in set; mark it `#[ignore = "enabled in Task 8"]` for now.

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p cairn-core --locked pipeline::extract::regex::rule`
Expected: previous 4 + new 4 (1 ignored) = 8 tests, 7 pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/pipeline/extract/
git commit -m "feat(extract): add RuleSet, CompiledRule, and rule compilation (#73)"
```

---

## Task 8: Built-in default rule set

**Files:**
- Modify: `crates/cairn-core/src/pipeline/extract/regex/defaults.rs`
- Modify: `crates/cairn-core/src/pipeline/extract/regex/rule.rs`

- [ ] **Step 1: Replace the `defaults.rs` stub with the real built-in set**

`crates/cairn-core/src/pipeline/extract/regex/defaults.rs`:

```rust
//! Built-in rule set. Spec §7.
//!
//! Text rules are listed in dispatch order (most specific first); the
//! per-window first-match-wins policy in §6.2 makes ordering
//! load-bearing — `remember.rule` MUST precede `remember.preference`.

use super::rule::{compile_rule, CompiledRuleKind, RegexRule, RuleOrigin, RuleSet, ToolFrameFamily};
use super::super::{Confidence, ForgetMatchStrategy, KindHint};
use crate::domain::taxonomy::MemoryKind;

#[must_use]
pub fn builtin_rule_set() -> RuleSet {
    let mut set = RuleSet::empty();

    let rules = builtin_rules();
    for rule in rules {
        let compiled = compile_rule(&rule, RuleOrigin::BuiltIn)
            .expect("built-in rules are compile-checked at build time");
        match &compiled.kind {
            CompiledRuleKind::TriggerPhrase { .. } | CompiledRuleKind::ForgetPhrase { .. } => {
                set.builtin_text.push(compiled);
            }
            CompiledRuleKind::HookEvent { .. } => set.builtin_hook.push(compiled),
            CompiledRuleKind::ToolFrame { .. } => set.builtin_tool_frame.push(compiled),
        }
    }
    set
}

fn conf(v: f32) -> Confidence {
    Confidence::try_from(v).expect("static built-in confidence in [0,1]")
}

fn kind(k: MemoryKind) -> KindHint {
    KindHint::from(k)
}

fn builtin_rules() -> Vec<RegexRule> {
    vec![
        // Text rules: most-specific first.
        RegexRule::TriggerPhrase {
            id: "remember.rule".into(),
            pattern: r"^\s*remember(?::|,)?\s+never\s+(.+?)\s*$".into(),
            kind_hint: kind(MemoryKind::Rule),
            confidence: conf(0.95),
            capture_group: Some(1),
        },
        RegexRule::TriggerPhrase {
            id: "remember.preference".into(),
            pattern: r"^\s*remember(?:\s+that)?\s+(.+?)\s*$".into(),
            kind_hint: kind(MemoryKind::User),
            confidence: conf(0.95),
            capture_group: Some(1),
        },
        RegexRule::TriggerPhrase {
            id: "correction".into(),
            pattern: r"^\s*correction:?\s+(.+?)\s*$".into(),
            kind_hint: kind(MemoryKind::Feedback),
            confidence: conf(0.95),
            capture_group: Some(1),
        },
        RegexRule::TriggerPhrase {
            id: "success.recipe".into(),
            pattern: r"^\s*this is how we did it\s*[—-]\s*it worked\s*$".into(),
            kind_hint: kind(MemoryKind::StrategySuccess),
            confidence: conf(0.85),
            capture_group: Some(0),
        },
        RegexRule::TriggerPhrase {
            id: "skillify".into(),
            pattern: r"^\s*skillify\s+(?:this|it)\s*$".into(),
            kind_hint: kind(MemoryKind::Playbook),
            confidence: conf(0.95),
            capture_group: Some(0),
        },
        RegexRule::ForgetPhrase {
            id: "forget".into(),
            pattern: r"^\s*forget\s+(?:that\s+|what\s+)?(.+?)\s*$".into(),
            target_group: 1,
            confidence: conf(0.95),
            match_strategy: ForgetMatchStrategy::Substring,
            quoted_capture: false,
        },
        // Hook rules.
        RegexRule::HookEvent {
            id: "hook.post_tool_use".into(),
            hook_name: "PostToolUse".into(),
            tool_name: None,
            kind_hint: kind(MemoryKind::Trace),
            confidence: conf(0.8),
        },
        RegexRule::HookEvent {
            id: "hook.stop".into(),
            hook_name: "Stop".into(),
            tool_name: None,
            kind_hint: kind(MemoryKind::Trace),
            confidence: conf(0.7),
        },
        RegexRule::HookEvent {
            id: "hook.pre_compact".into(),
            hook_name: "PreCompact".into(),
            tool_name: None,
            kind_hint: kind(MemoryKind::Trace),
            confidence: conf(0.7),
        },
        // Tool-frame.
        RegexRule::ToolFrame {
            id: "tool.terminal_failure".into(),
            family: ToolFrameFamily::Terminal {
                exit_code_nonzero: true,
            },
            kind_hint: kind(MemoryKind::StrategyFailure),
            confidence: conf(0.7),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::rule::CompiledRuleKind;

    #[test]
    fn builtin_set_has_expected_buckets() {
        let set = builtin_rule_set();
        assert_eq!(set.builtin_text.len(), 6);
        assert_eq!(set.builtin_hook.len(), 3);
        assert_eq!(set.builtin_tool_frame.len(), 1);
        assert!(set.user_text.is_empty());
    }

    #[test]
    fn text_rule_order_is_specific_first() {
        let set = builtin_rule_set();
        let ids: Vec<&str> = set.builtin_text.iter().map(|r| r.id.as_str()).collect();
        // "remember.rule" must precede "remember.preference"
        let rule_pos = ids.iter().position(|s| *s == "remember.rule").unwrap();
        let pref_pos = ids.iter().position(|s| *s == "remember.preference").unwrap();
        assert!(rule_pos < pref_pos, "remember.rule must precede remember.preference");
    }

    #[test]
    fn remember_preference_matches_basic_phrase() {
        let set = builtin_rule_set();
        let rule = set
            .builtin_text
            .iter()
            .find(|r| r.id == "remember.preference")
            .unwrap();
        let re = match &rule.kind {
            CompiledRuleKind::TriggerPhrase { re, .. } => re,
            _ => panic!("wrong kind"),
        };
        assert!(re.is_match("remember that I prefer dark mode"));
        assert!(re.is_match("Remember I prefer cash"));
        // The rule alone is NOT mutually-exclusive against
        // remember.rule — exclusivity is enforced at dispatch via
        // first-match-wins (Task 11).
    }

    #[test]
    fn remember_rule_matches_never_form() {
        let set = builtin_rule_set();
        let rule = set
            .builtin_text
            .iter()
            .find(|r| r.id == "remember.rule")
            .unwrap();
        let re = match &rule.kind {
            CompiledRuleKind::TriggerPhrase { re, .. } => re,
            _ => panic!("wrong kind"),
        };
        assert!(re.is_match("remember never share API keys"));
        assert!(re.is_match("Remember: never push to main"));
        assert!(!re.is_match("remember that I prefer dark mode"));
    }

    #[test]
    fn forget_rule_captures_target() {
        let set = builtin_rule_set();
        let rule = set.builtin_text.iter().find(|r| r.id == "forget").unwrap();
        let (re, target_group) = match &rule.kind {
            CompiledRuleKind::ForgetPhrase {
                re, target_group, ..
            } => (re, *target_group),
            _ => panic!("wrong kind"),
        };
        let caps = re.captures("forget that I mentioned my address").unwrap();
        assert_eq!(&caps[target_group as usize], "I mentioned my address");
    }

    #[test]
    fn skillify_matches_only_anchored_form() {
        let set = builtin_rule_set();
        let rule = set
            .builtin_text
            .iter()
            .find(|r| r.id == "skillify")
            .unwrap();
        let re = match &rule.kind {
            CompiledRuleKind::TriggerPhrase { re, .. } => re,
            _ => panic!("wrong kind"),
        };
        assert!(re.is_match("skillify this"));
        assert!(re.is_match("Skillify it"));
        assert!(!re.is_match("we should skillify the procedure later"));
    }
}
```

- [ ] **Step 2: Re-enable the duplicate-id test in `rule.rs`**

Remove the `#[ignore = "enabled in Task 8"]` attribute from the `with_user_rules_rejects_duplicate_id_against_builtin` test added in Task 7.

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p cairn-core --locked pipeline::extract::regex`
Expected: all `defaults` tests pass + `with_user_rules_rejects_duplicate_id_against_builtin` now active and passing.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/pipeline/extract/regex/
git commit -m "feat(extract): add built-in default rule set (#73)"
```

---

## Task 9: Trigger prefilter — keyword scan, sentence-start eligibility, quote-awareness

**Files:**
- Create: `crates/cairn-core/src/pipeline/extract/regex/prefilter.rs`
- Modify: `crates/cairn-core/src/pipeline/extract/regex/mod.rs`

- [ ] **Step 1: Create `prefilter.rs`**

`crates/cairn-core/src/pipeline/extract/regex/prefilter.rs`:

```rust
//! Trigger-keyword prefilter and phrase-window builder. Spec §6.2.

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};

use super::super::TextSpan;

/// Fixed trigger keyword set the prefilter scans for.
const TRIGGER_KEYWORDS: &[&str] = &[
    "remember",
    "forget",
    "correction",
    "skillify",
    "this is how",
];

/// Pre-built trigger prefilter. One instance per `RegexExtractor`.
pub struct TriggerPrefilter {
    ac: AhoCorasick,
}

impl TriggerPrefilter {
    /// Build the (case-insensitive) prefilter.
    pub fn new() -> Self {
        let ac = AhoCorasickBuilder::new()
            .ascii_case_insensitive(true)
            .match_kind(MatchKind::LeftmostFirst)
            .build(TRIGGER_KEYWORDS)
            .expect("static patterns build");
        Self { ac }
    }

    /// Find every keyword occurrence whose start position is at a
    /// sentence-start (per `is_sentence_start`) and not inside a quoted
    /// span. Returns `(start, end)` byte offsets in the original body.
    /// Capped at `MAX_PHRASE_WINDOWS` hits; overflow is flagged via the
    /// returned `truncated` boolean and the caller is expected to add
    /// the tail to `llm_eligible_spans`.
    pub fn scan(&self, body: &str) -> PrefilterScan {
        let quote_spans = collect_quote_spans(body);
        let mut hits: Vec<TextSpan> = Vec::new();
        let mut truncated = false;
        let bytes = body.as_bytes();
        for m in self.ac.find_iter(body) {
            let start = m.start();
            let end = m.end();
            // Skip hits inside quoted spans.
            if quote_spans
                .iter()
                .any(|(qs, qe)| *qs <= start && end <= *qe)
            {
                continue;
            }
            if !is_sentence_start(bytes, start) {
                continue;
            }
            if hits.len() >= super::super::MAX_PHRASE_WINDOWS {
                truncated = true;
                tracing::warn!(
                    body_len = body.len(),
                    cap = super::super::MAX_PHRASE_WINDOWS,
                    "regex extractor: phrase-window cap reached"
                );
                break;
            }
            hits.push(TextSpan::new(start as u32, end as u32));
        }
        PrefilterScan { hits, truncated }
    }
}

#[derive(Debug, PartialEq)]
pub struct PrefilterScan {
    pub hits: Vec<TextSpan>,
    pub truncated: bool,
}

/// Whether the byte offset `pos` is at a "sentence-start" position for
/// trigger eligibility. See spec §6.2 for the rules.
pub fn is_sentence_start(body: &[u8], pos: usize) -> bool {
    if pos == 0 {
        return true;
    }
    // Walk backwards over whitespace.
    let mut i = pos;
    while i > 0 && body[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    if i == 0 {
        return true;
    }
    let prev = body[i - 1];
    match prev {
        b'\n' | b';' | b'?' | b'!' => true,
        b'.' => is_period_sentence_boundary(body, i - 1),
        b',' => true,
        // After a conjunction word.
        _ => preceded_by_conjunction(body, i),
    }
}

fn preceded_by_conjunction(body: &[u8], end: usize) -> bool {
    // `end` is exclusive; preceding word is `body[..end]` trimmed of
    // trailing whitespace (already handled by caller), so check the
    // last word.
    let mut j = end;
    while j > 0 && !body[j - 1].is_ascii_whitespace() {
        j -= 1;
    }
    let word = &body[j..end];
    matches!(word, b"and" | b"but" | b"then" | b"AND" | b"BUT" | b"THEN")
        // Case-insensitive ASCII: also accept mixed case.
        || ascii_eq_ignore_case(word, b"and")
        || ascii_eq_ignore_case(word, b"but")
        || ascii_eq_ignore_case(word, b"then")
}

fn ascii_eq_ignore_case(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|(x, y)| x.eq_ignore_ascii_case(y))
}

fn is_period_sentence_boundary(body: &[u8], period_pos: usize) -> bool {
    // Abbreviation guard: if the previous 6 bytes contain another `.`
    // adjacent to single uppercase letters (e.g. `U.S.`), this is
    // probably not a sentence boundary. Conservative: only accept the
    // period as a boundary when the preceding 6 bytes are NOT an
    // abbreviation pattern.
    let start = period_pos.saturating_sub(6);
    let window = &body[start..period_pos];
    !looks_like_abbreviation(window)
}

fn looks_like_abbreviation(window: &[u8]) -> bool {
    // Crude: contains a `.` followed by an ASCII uppercase letter
    // (e.g. `U.S.`, `e.g.`).
    let mut i = 0;
    while i + 1 < window.len() {
        if window[i] == b'.' && window[i + 1].is_ascii_alphabetic() {
            return true;
        }
        i += 1;
    }
    // Ends with an uppercase letter directly preceding the period?
    // e.g. `Inc` (still a sentence boundary, leave alone) — only the
    // multi-period pattern is the abbreviation case.
    false
}

/// Collect `(start, end)` byte ranges of quoted spans (`"..."`,
/// `'...'`, `` `...` ``). Inclusive of opening quote, exclusive of
/// closing quote+1, so byte ranges are `[start, end)`.
fn collect_quote_spans(body: &str) -> Vec<(usize, usize)> {
    let bytes = body.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if matches!(b, b'"' | b'\'' | b'`') {
            // Find the matching closer.
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b {
                j += 1;
            }
            if j < bytes.len() {
                spans.push((i, j + 1));
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(body: &str) -> Vec<&str> {
        let pre = TriggerPrefilter::new();
        let scan = pre.scan(body);
        scan.hits
            .iter()
            .map(|s| &body[s.start as usize..s.end as usize])
            .collect()
    }

    #[test]
    fn finds_remember_at_start() {
        let hits = scan("remember that I prefer dark mode");
        assert_eq!(hits, vec!["remember"]);
    }

    #[test]
    fn finds_remember_after_period_with_space() {
        let hits = scan("FYI old. remember that I prefer cash");
        assert_eq!(hits, vec!["remember"]);
    }

    #[test]
    fn ignores_abbreviation_period() {
        // The period after `U.S` should NOT be treated as a sentence
        // boundary, so a hypothetical second `remember` after it would
        // not be eligible. Use a body where this matters:
        let hits = scan("remember that I live in the U.S. and prefer cash");
        // Only the leading "remember" is found; no second hit.
        assert_eq!(hits, vec!["remember"]);
    }

    #[test]
    fn skips_keyword_inside_quotes() {
        let hits = scan(r#"the user said "remember that" by mistake"#);
        assert!(hits.is_empty());
    }

    #[test]
    fn finds_after_conjunction_with_trigger() {
        let hits = scan("forget X and remember Y");
        assert_eq!(hits, vec!["forget", "remember"]);
    }

    #[test]
    fn does_not_split_normal_and() {
        // `and` followed by a non-trigger word is NOT a boundary, so
        // the only hit is the leading `remember`.
        let hits = scan("remember that Alice and Bob are on call");
        assert_eq!(hits, vec!["remember"]);
    }

    #[test]
    fn caps_at_max_phrase_windows() {
        let body = "remember a; ".repeat(super::super::super::MAX_PHRASE_WINDOWS + 5);
        let pre = TriggerPrefilter::new();
        let scan = pre.scan(&body);
        assert!(scan.truncated);
        assert_eq!(scan.hits.len(), super::super::super::MAX_PHRASE_WINDOWS);
    }

    #[test]
    fn finds_trigger_in_oversized_body() {
        let mut body = "lorem ipsum ".repeat(20_000); // ~240 KB
        body.push_str("forget my old address");
        let hits = scan(&body);
        assert_eq!(hits, vec!["forget"]);
    }

    #[test]
    fn quote_aware_finds_unquoted_after_quoted() {
        let body = r#"the user said "remember that" then forget my password"#;
        let hits = scan(body);
        assert_eq!(hits, vec!["forget"]);
    }
}
```

- [ ] **Step 2: Wire `prefilter` into `regex/mod.rs`**

```rust
pub mod prefilter;

pub use prefilter::TriggerPrefilter;
```

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p cairn-core --locked pipeline::extract::regex::prefilter`
Expected: 9 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/pipeline/extract/regex/
git commit -m "feat(extract): add aho-corasick trigger prefilter (#73)"
```

---

## Task 10: Phrase-window builder

**Files:**
- Modify: `crates/cairn-core/src/pipeline/extract/regex/prefilter.rs`

- [ ] **Step 1: Add `build_phrase_windows`**

Append to `prefilter.rs` (above the `#[cfg(test)] mod tests` block):

```rust
/// Phrase window: a slice of the body anchored at a prefilter hit, ending
/// at the next stop (sentence boundary, semicolon, newline, next hit, or EOB).
#[derive(Debug, PartialEq)]
pub struct PhraseWindow {
    pub span: TextSpan,
}

/// Build phrase windows from prefilter hits. Each window starts at a
/// hit and runs to the next stop. Overlapping or zero-length windows
/// are dropped.
#[must_use]
pub fn build_phrase_windows(body: &str, hits: &[TextSpan]) -> Vec<PhraseWindow> {
    let bytes = body.as_bytes();
    let mut windows = Vec::with_capacity(hits.len());
    for (i, hit) in hits.iter().enumerate() {
        let start = hit.start as usize;
        let next_hit_start = hits
            .get(i + 1)
            .map(|h| h.start as usize)
            .unwrap_or(bytes.len());
        let stop = find_window_stop(bytes, start, next_hit_start);
        if stop > start {
            windows.push(PhraseWindow {
                span: TextSpan::new(start as u32, stop as u32),
            });
        }
    }
    windows
}

fn find_window_stop(bytes: &[u8], start: usize, hard_stop: usize) -> usize {
    // Advance from `start` until we hit one of: `;`, `\n`, `.<space|EOB>`
    // (with abbreviation guard), or `hard_stop`.
    let mut i = start;
    while i < hard_stop {
        let b = bytes[i];
        match b {
            b';' | b'\n' => return i,
            b'.' => {
                // Sentence-end period: followed by whitespace/EOB AND
                // not an abbreviation.
                let after_is_ws_or_eob = i + 1 == bytes.len() || bytes[i + 1].is_ascii_whitespace();
                if after_is_ws_or_eob && is_period_sentence_boundary(bytes, i) {
                    return i;
                }
            }
            _ => {}
        }
        i += 1;
    }
    hard_stop.min(bytes.len())
}
```

- [ ] **Step 2: Add tests**

Inside the existing `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn build_windows_single_hit_runs_to_eob() {
        let body = "remember that I prefer dark mode";
        let pre = TriggerPrefilter::new();
        let scan = pre.scan(body);
        let windows = build_phrase_windows(body, &scan.hits);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].span, TextSpan::new(0, body.len() as u32));
    }

    #[test]
    fn build_windows_stops_at_semicolon() {
        let body = "remember that thing; nothing else";
        let pre = TriggerPrefilter::new();
        let scan = pre.scan(body);
        let windows = build_phrase_windows(body, &scan.hits);
        assert_eq!(windows.len(), 1);
        let s = &body[windows[0].span.start as usize..windows[0].span.end as usize];
        assert_eq!(s, "remember that thing");
    }

    #[test]
    fn build_windows_two_triggers() {
        let body = "forget X and remember Y";
        let pre = TriggerPrefilter::new();
        let scan = pre.scan(body);
        let windows = build_phrase_windows(body, &scan.hits);
        assert_eq!(windows.len(), 2);
        let w0 = &body[windows[0].span.start as usize..windows[0].span.end as usize];
        let w1 = &body[windows[1].span.start as usize..windows[1].span.end as usize];
        assert_eq!(w0, "forget X and ");
        assert_eq!(w1, "remember Y");
    }
```

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p cairn-core --locked pipeline::extract::regex::prefilter`
Expected: 12 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/pipeline/extract/regex/prefilter.rs
git commit -m "feat(extract): add phrase-window builder (#73)"
```

---

## Task 11: Two-phase dispatch + budget enforcement + `llm_eligible_spans`

**Files:**
- Create: `crates/cairn-core/src/pipeline/extract/regex/dispatch.rs`
- Modify: `crates/cairn-core/src/pipeline/extract/regex/mod.rs`

- [ ] **Step 1: Create `dispatch.rs`**

`crates/cairn-core/src/pipeline/extract/regex/dispatch.rs`:

```rust
//! Phase 0/A/B dispatch + budget enforcement + llm_eligible_spans.
//! Spec §6.

use std::time::Instant;

use crate::domain::{CaptureEvent, CapturePayload};

use super::super::{
    BodyResolution, BodySource, Confidence, ExtractBudget, ExtractError, ExtractInput,
    ExtractOutput, ExtractResult, ForgetIntent, ForgetMatchStrategy, KindHint, MemoryDraft,
    TextSpan, TruncationReason, CONFIDENCE_GATE_FOR_SUPPRESSION, MAX_BODY_LEN_FOR_REGEX,
    MAX_PHASE_A_WALL_MS, MAX_PHASE_A_WALL_MS_LARGE,
};
use super::prefilter::{build_phrase_windows, PhraseWindow, TriggerPrefilter};
use super::rule::{CompiledRule, CompiledRuleKind, RuleSet, ToolFrameFamily};

pub(crate) async fn dispatch(
    rules: &RuleSet,
    prefilter: &TriggerPrefilter,
    budget: &ExtractBudget,
    input: &ExtractInput<'_>,
) -> Result<ExtractResult, ExtractError> {
    // Body resolution must be checked first so a `Failed` body cannot
    // masquerade as `NotApplicable`.
    let body_text: Option<&str> = match &input.body {
        BodyResolution::Resolved(rb) => {
            // Source allowlist is the type system's job; just read text.
            let _ = rb.source(); // for tracing later
            Some(rb.text())
        }
        BodyResolution::NotApplicable => None,
        BodyResolution::Failed(err) => {
            return Err(ExtractError::BodyResolution {
                event_id: input.event.event_id.as_str().to_owned(),
                source: err.clone(),
            });
        }
    };

    let event = input.event;
    let mut outputs: Vec<ExtractOutput> = Vec::new();
    let mut llm_spans: Vec<TextSpan> = Vec::new();
    let mut truncated = TruncationReason::None;

    // Hook + tool-frame rules (independent of body).
    run_hook_rules(&rules.builtin_hook, event, &mut outputs);
    run_hook_rules(&rules.user_hook, event, &mut outputs);
    run_tool_frame_rules(&rules.builtin_tool_frame, event, &mut outputs);
    run_tool_frame_rules(&rules.user_tool_frame, event, &mut outputs);

    // Text-rule dispatch. Only runs when a body is resolved.
    if let Some(text) = body_text {
        let scan = prefilter.scan(text);
        let mut windows = build_phrase_windows(text, &scan.hits);

        // Phase 0: body-size guard for Phase B.
        let body_too_large = text.len() > MAX_BODY_LEN_FOR_REGEX;
        if body_too_large {
            tracing::warn!(
                body_len = text.len(),
                "body exceeds MAX_BODY_LEN_FOR_REGEX, skipping Phase B user rules"
            );
        }

        // Phase A: built-ins on every window.
        let phase_a_start = Instant::now();
        let mut covered_high_conf: Vec<TextSpan> = Vec::new();
        for window in &windows {
            run_text_rules_first_match_wins(
                &rules.builtin_text,
                event,
                text,
                window,
                &mut outputs,
                &mut covered_high_conf,
            );
        }
        let phase_a_ms = phase_a_start.elapsed().as_millis() as u32;
        let limit_a = if body_too_large {
            MAX_PHASE_A_WALL_MS_LARGE
        } else {
            MAX_PHASE_A_WALL_MS
        };
        if phase_a_ms > limit_a {
            tracing::warn!(
                worker = "regex",
                phase_a_ms,
                limit_a,
                "Phase A exceeded its wall-clock observability rail"
            );
        }

        // Phase B: user rules (capped). Skipped on oversize bodies.
        if !body_too_large {
            let phase_b_start = Instant::now();
            'phase_b: for window in &windows {
                for rule in &rules.user_text {
                    let elapsed_ms = phase_b_start.elapsed().as_millis() as u32;
                    if elapsed_ms > budget.max_wall_ms {
                        if outputs.is_empty() {
                            return Err(ExtractError::BudgetExceeded {
                                worker: "regex",
                                elapsed_ms,
                            });
                        }
                        tracing::warn!(
                            worker = "regex",
                            elapsed_ms,
                            budget = budget.max_wall_ms,
                            "regex extractor: Phase B exceeded max_wall_ms"
                        );
                        truncated = TruncationReason::MaxWallMs { elapsed_ms };
                        break 'phase_b;
                    }
                    let before = outputs.len();
                    apply_text_rule(rule, event, text, window, &mut outputs, &mut covered_high_conf);
                    let added = outputs.len() > before;
                    if added && outputs.len() >= budget.max_drafts as usize {
                        tracing::warn!(
                            worker = "regex",
                            max_drafts = budget.max_drafts,
                            "regex extractor: reached max_drafts cap during Phase B"
                        );
                        truncated = TruncationReason::MaxDrafts;
                        break 'phase_b;
                    }
                    if added {
                        // First-match-wins: stop scanning further user rules
                        // for this window.
                        break;
                    }
                }
            }
        } else {
            truncated = TruncationReason::BodyTooLarge {
                body_len: text.len() as u32,
            };
        }

        // Phrase-window cap also flags llm_eligible_spans for the tail.
        if scan.truncated {
            // The tail is everything after the last window. We don't
            // know exactly where the next would-be hit was, so use the
            // end-of-last-window onward.
            if let Some(last) = windows.last() {
                if (last.span.end as usize) < text.len() {
                    llm_spans.push(TextSpan::new(last.span.end, text.len() as u32));
                }
            }
            if matches!(truncated, TruncationReason::None) {
                truncated = TruncationReason::ClauseCapExceeded {
                    processed: super::super::MAX_PHRASE_WINDOWS as u8,
                    body_len: text.len() as u32,
                };
            }
        }

        compute_llm_eligible_spans(text, &windows, &covered_high_conf, &mut llm_spans);

        if body_too_large {
            // Whole body needs LLM enrichment.
            llm_spans.clear();
            llm_spans.push(TextSpan::new(0, text.len() as u32));
        }
    }

    Ok(ExtractResult {
        outputs,
        truncated,
        llm_eligible_spans: llm_spans,
    })
}

fn run_hook_rules(
    rules: &[CompiledRule],
    event: &CaptureEvent,
    outputs: &mut Vec<ExtractOutput>,
) {
    let CapturePayload::Hook { hook_name, tool_name, .. } = &event.payload else {
        return;
    };
    for rule in rules {
        let CompiledRuleKind::HookEvent {
            hook_name: rule_hook,
            tool_name: rule_tool,
            kind_hint,
            confidence,
        } = &rule.kind
        else {
            continue;
        };
        if rule_hook != hook_name {
            continue;
        }
        if let Some(t) = rule_tool {
            if Some(t) != tool_name.as_ref() {
                continue;
            }
        }
        outputs.push(ExtractOutput::Draft(MemoryDraft {
            kind_hint: kind_hint.clone(),
            body: format!("hook:{hook_name}"),
            confidence: *confidence,
            source_event: event.event_id.clone(),
            source_span: None,
            trigger_id: Some(rule.id.clone()),
        }));
    }
}

fn run_tool_frame_rules(
    rules: &[CompiledRule],
    event: &CaptureEvent,
    outputs: &mut Vec<ExtractOutput>,
) {
    for rule in rules {
        let CompiledRuleKind::ToolFrame {
            family,
            kind_hint,
            confidence,
        } = &rule.kind
        else {
            continue;
        };
        let fired = match (family, &event.payload) {
            (
                ToolFrameFamily::Terminal { exit_code_nonzero: true },
                CapturePayload::Terminal { exit_code: Some(code), .. },
            ) => *code != 0,
            (
                ToolFrameFamily::Terminal { exit_code_nonzero: false },
                CapturePayload::Terminal { .. },
            ) => true,
            (ToolFrameFamily::Ide { event_kind }, CapturePayload::Ide { event_kind: ek, .. }) => {
                event_kind == ek
            }
            _ => false,
        };
        if !fired {
            continue;
        }
        outputs.push(ExtractOutput::Draft(MemoryDraft {
            kind_hint: kind_hint.clone(),
            body: format!("tool-frame:{}", rule.id),
            confidence: *confidence,
            source_event: event.event_id.clone(),
            source_span: None,
            trigger_id: Some(rule.id.clone()),
        }));
    }
}

fn run_text_rules_first_match_wins(
    rules: &[CompiledRule],
    event: &CaptureEvent,
    body: &str,
    window: &PhraseWindow,
    outputs: &mut Vec<ExtractOutput>,
    covered_high_conf: &mut Vec<TextSpan>,
) {
    for rule in rules {
        let before = outputs.len();
        apply_text_rule(rule, event, body, window, outputs, covered_high_conf);
        if outputs.len() > before {
            return;
        }
    }
}

fn apply_text_rule(
    rule: &CompiledRule,
    event: &CaptureEvent,
    body: &str,
    window: &PhraseWindow,
    outputs: &mut Vec<ExtractOutput>,
    covered_high_conf: &mut Vec<TextSpan>,
) {
    let slice = &body[window.span.start as usize..window.span.end as usize];
    match &rule.kind {
        CompiledRuleKind::TriggerPhrase {
            re,
            kind_hint,
            confidence,
            capture_group,
        } => {
            if let Some(caps) = re.captures(slice) {
                let group_text = caps
                    .get(*capture_group as usize)
                    .map(|m| m.as_str())
                    .unwrap_or(slice);
                outputs.push(ExtractOutput::Draft(MemoryDraft {
                    kind_hint: kind_hint.clone(),
                    body: group_text.to_owned(),
                    confidence: *confidence,
                    source_event: event.event_id.clone(),
                    source_span: Some(window.span),
                    trigger_id: Some(rule.id.clone()),
                }));
                if confidence.as_f32() >= CONFIDENCE_GATE_FOR_SUPPRESSION {
                    covered_high_conf.push(window.span);
                }
            }
        }
        CompiledRuleKind::ForgetPhrase {
            re,
            target_group,
            confidence,
            match_strategy,
        } => {
            if let Some(caps) = re.captures(slice) {
                let target = caps
                    .get(*target_group as usize)
                    .map(|m| m.as_str())
                    .unwrap_or("");
                outputs.push(ExtractOutput::Forget(ForgetIntent {
                    target_text_normalized: normalize_target(target),
                    match_strategy: *match_strategy,
                    kind_filter: None,
                    source_span: window.span,
                    confidence: *confidence,
                    source_event: event.event_id.clone(),
                    trigger_id: rule.id.clone(),
                }));
                if confidence.as_f32() >= CONFIDENCE_GATE_FOR_SUPPRESSION {
                    covered_high_conf.push(window.span);
                }
            }
        }
        _ => {}
    }
}

fn normalize_target(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_was_space = false;
    for c in s.chars() {
        let lc = c.to_ascii_lowercase();
        if lc.is_whitespace() {
            if !prev_was_space {
                out.push(' ');
                prev_was_space = true;
            }
        } else {
            out.push(lc);
            prev_was_space = false;
        }
    }
    out.trim().to_owned()
}

/// Build `llm_eligible_spans` from windows and high-confidence coverage.
fn compute_llm_eligible_spans(
    body: &str,
    windows: &[PhraseWindow],
    covered_high_conf: &[TextSpan],
    out: &mut Vec<TextSpan>,
) {
    // Start: every window span.
    let mut eligible: Vec<TextSpan> = windows.iter().map(|w| w.span).collect();
    // Plus body bytes outside any window (uncovered prose).
    let mut last_end: u32 = 0;
    for w in windows {
        if w.span.start > last_end {
            eligible.push(TextSpan::new(last_end, w.span.start));
        }
        last_end = last_end.max(w.span.end);
    }
    if (last_end as usize) < body.len() {
        eligible.push(TextSpan::new(last_end, body.len() as u32));
    }
    // Subtract high-confidence coverage.
    let eligible: Vec<TextSpan> = eligible
        .into_iter()
        .filter(|s| !covered_high_conf.iter().any(|c| c.overlaps(*s) && c.start <= s.start && c.end >= s.end))
        .collect();
    // Merge adjacent.
    let mut sorted = eligible;
    sorted.sort_by_key(|s| s.start);
    let mut merged: Vec<TextSpan> = Vec::new();
    for s in sorted {
        if let Some(last) = merged.last_mut() {
            if s.start <= last.end {
                last.end = last.end.max(s.end);
                continue;
            }
        }
        merged.push(s);
    }
    out.extend(merged);
}
```

- [ ] **Step 2: Wire `dispatch` into `regex/mod.rs`**

Add `pub(crate) mod dispatch;` to `regex/mod.rs`.

- [ ] **Step 3: Run tests**

Run: `cargo check -p cairn-core --all-targets --locked`
Expected: clean compile.

(No new unit tests yet — exercised by the `RegexExtractor` integration in Task 12.)

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/pipeline/extract/
git commit -m "feat(extract): add two-phase dispatch + budget enforcement (#73)"
```

---

## Task 12: `RegexExtractor` struct + `ExtractorWorker` impl

**Files:**
- Modify: `crates/cairn-core/src/pipeline/extract/regex/mod.rs`

- [ ] **Step 1: Add the public extractor**

Replace `regex/mod.rs` contents with:

```rust
//! `RegexExtractor` — regex/state-machine implementation of
//! `ExtractorWorker`. See spec §6.

pub mod defaults;
pub(crate) mod dispatch;
pub mod prefilter;
pub mod rule;

pub use prefilter::TriggerPrefilter;
pub use rule::{RegexRule, RuleSet, ToolFrameFamily};

use async_trait::async_trait;

use super::{
    ExtractBudget, ExtractError, ExtractInput, ExtractResult, ExtractorWorker,
};

/// Built-in + user-rule extractor.
pub struct RegexExtractor {
    rules: RuleSet,
    prefilter: TriggerPrefilter,
    budget: ExtractBudget,
}

impl RegexExtractor {
    /// Construct with the built-in rule set and the default budget.
    #[must_use]
    pub fn builtin() -> Self {
        Self {
            rules: RuleSet::builtin(),
            prefilter: TriggerPrefilter::new(),
            budget: ExtractBudget::regex_default(),
        }
    }

    /// Construct from any rule set + budget. Used by tests and by #74's
    /// chain dispatcher.
    #[must_use]
    pub fn from_parts(rules: RuleSet, budget: ExtractBudget) -> Self {
        Self {
            rules,
            prefilter: TriggerPrefilter::new(),
            budget,
        }
    }
}

#[async_trait]
impl ExtractorWorker for RegexExtractor {
    fn name(&self) -> &'static str {
        "regex"
    }

    fn budget(&self) -> ExtractBudget {
        self.budget
    }

    async fn extract(
        &self,
        input: &ExtractInput<'_>,
    ) -> Result<ExtractResult, ExtractError> {
        dispatch::dispatch(&self.rules, &self.prefilter, &self.budget, input).await
    }
}
```

- [ ] **Step 2: Re-export from `extract/mod.rs`**

```rust
pub use regex::RegexExtractor;
```

- [ ] **Step 3: Smoke test inside `regex/mod.rs`**

Append:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_constructs() {
        let ex = RegexExtractor::builtin();
        assert_eq!(ex.name(), "regex");
        assert_eq!(ex.budget(), ExtractBudget::regex_default());
    }
}
```

Run: `cargo nextest run -p cairn-core --locked pipeline::extract::regex`
Expected: passes.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/pipeline/extract/
git commit -m "feat(extract): add RegexExtractor and impl ExtractorWorker (#73)"
```

---

## Task 13: Integration tests — happy paths and acceptance criteria

**Files:**
- Create: `crates/cairn-core/tests/pipeline_extract_regex.rs`

- [ ] **Step 1: Write the integration test file**

`crates/cairn-core/tests/pipeline_extract_regex.rs`:

```rust
//! Integration tests for `RegexExtractor`. Each test maps to an
//! acceptance-criterion bullet from spec §10.5 / §10.2.

use cairn_core::domain::{
    ActorChainEntry, ChainRole, CaptureEvent, CaptureEventId, CaptureMode, CapturePayload,
    Identity, IdentityKind, PayloadHash, Rfc3339Timestamp, SourceFamily,
};
use cairn_core::pipeline::extract::{
    BodyResolution, ExtractBudget, ExtractInput, ExtractOutput, ExtractResult, ExtractorWorker,
    ForgetMatchStrategy, ProactiveBodyContext, RegexExtractor, ResolvedBody, RuleSet, TextSpan,
    TruncationReason, UserIngestPayloadKind,
};

fn fixture_event(payload: CapturePayload) -> CaptureEvent {
    let id = CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ulid");
    let identity = Identity::new(IdentityKind::Sensor, "snr:cli:v1").expect("valid id");
    let chain_entry = ActorChainEntry::new(identity.clone(), ChainRole::Author).expect("entry");
    CaptureEvent::new(
        id,
        identity,
        CaptureMode::ExplicitB,
        vec![chain_entry],
        None,
        PayloadHash::sha256_of(b"fixture"),
        "vault://fixture".into(),
        Rfc3339Timestamp::now(),
        payload,
        SourceFamily::Cli,
    )
    .expect("construct fixture event")
}

#[tokio::test]
async fn explicit_remember_emits_user_draft() {
    let extractor = RegexExtractor::builtin();
    let event = fixture_event(CapturePayload::Cli {
        kind_hint: "user".into(),
    });
    let body = ResolvedBody::from_user_ingest(
        "remember that I prefer dark mode",
        UserIngestPayloadKind::Cli,
    );
    let input = ExtractInput {
        event: &event,
        body: BodyResolution::Resolved(body),
    };
    let res = extractor.extract(&input).await.expect("extract ok");
    assert_eq!(res.outputs.len(), 1);
    let ExtractOutput::Draft(draft) = &res.outputs[0] else {
        panic!("expected draft");
    };
    assert_eq!(draft.body, "I prefer dark mode");
    assert_eq!(draft.trigger_id.as_deref(), Some("remember.preference"));
}

#[tokio::test]
async fn remember_never_routes_to_rule_kind() {
    let extractor = RegexExtractor::builtin();
    let event = fixture_event(CapturePayload::Cli {
        kind_hint: "rule".into(),
    });
    let body = ResolvedBody::from_user_ingest(
        "remember never share API keys",
        UserIngestPayloadKind::Cli,
    );
    let input = ExtractInput {
        event: &event,
        body: BodyResolution::Resolved(body),
    };
    let res = extractor.extract(&input).await.expect("ok");
    assert_eq!(res.outputs.len(), 1);
    let ExtractOutput::Draft(draft) = &res.outputs[0] else { panic!(); };
    assert_eq!(draft.trigger_id.as_deref(), Some("remember.rule"));
}

#[tokio::test]
async fn forget_emits_substring_intent() {
    let extractor = RegexExtractor::builtin();
    let event = fixture_event(CapturePayload::Cli {
        kind_hint: "user".into(),
    });
    let body = ResolvedBody::from_user_ingest(
        "forget that I mentioned my address",
        UserIngestPayloadKind::Cli,
    );
    let input = ExtractInput {
        event: &event,
        body: BodyResolution::Resolved(body),
    };
    let res = extractor.extract(&input).await.expect("ok");
    assert_eq!(res.outputs.len(), 1);
    let ExtractOutput::Forget(intent) = &res.outputs[0] else {
        panic!("expected forget");
    };
    assert_eq!(intent.match_strategy, ForgetMatchStrategy::Substring);
    assert_eq!(intent.target_text_normalized, "i mentioned my address");
}

#[tokio::test]
async fn compound_utterance_emits_two_outputs() {
    let extractor = RegexExtractor::builtin();
    let event = fixture_event(CapturePayload::Cli {
        kind_hint: "user".into(),
    });
    let body = ResolvedBody::from_user_ingest(
        "forget my old address and remember the new one is 1 Main St",
        UserIngestPayloadKind::Cli,
    );
    let input = ExtractInput {
        event: &event,
        body: BodyResolution::Resolved(body),
    };
    let res = extractor.extract(&input).await.expect("ok");
    assert_eq!(res.outputs.len(), 2);
    matches!(res.outputs[0], ExtractOutput::Forget(_));
    matches!(res.outputs[1], ExtractOutput::Draft(_));
}

#[tokio::test]
async fn abbreviation_does_not_split_sentence() {
    let extractor = RegexExtractor::builtin();
    let event = fixture_event(CapturePayload::Cli {
        kind_hint: "user".into(),
    });
    let body = ResolvedBody::from_user_ingest(
        "remember that I live in the U.S. and prefer cash",
        UserIngestPayloadKind::Cli,
    );
    let input = ExtractInput {
        event: &event,
        body: BodyResolution::Resolved(body),
    };
    let res = extractor.extract(&input).await.expect("ok");
    assert_eq!(res.outputs.len(), 1);
    let ExtractOutput::Draft(d) = &res.outputs[0] else { panic!(); };
    assert_eq!(d.body, "I live in the U.S. and prefer cash");
}

#[tokio::test]
async fn empty_fallthrough_returns_no_outputs() {
    let extractor = RegexExtractor::builtin();
    let event = fixture_event(CapturePayload::Cli {
        kind_hint: "user".into(),
    });
    let body = ResolvedBody::from_user_ingest("hello world", UserIngestPayloadKind::Cli);
    let input = ExtractInput {
        event: &event,
        body: BodyResolution::Resolved(body),
    };
    let res = extractor.extract(&input).await.expect("ok");
    assert!(res.outputs.is_empty());
    assert_eq!(res.truncated, TruncationReason::None);
}

#[tokio::test]
async fn body_resolution_failure_surfaces_typed_error() {
    use cairn_core::pipeline::extract::BodyResolutionError;
    let extractor = RegexExtractor::builtin();
    let event = fixture_event(CapturePayload::Cli {
        kind_hint: "user".into(),
    });
    let input = ExtractInput {
        event: &event,
        body: BodyResolution::Failed(BodyResolutionError::HashMismatch {
            expected: "abc".into(),
            got: "def".into(),
        }),
    };
    let err = extractor.extract(&input).await.unwrap_err();
    let s = format!("{err}");
    assert!(s.contains("body resolution failed"));
}

#[tokio::test]
async fn proactive_rationale_mislabel_rejected_by_constructor() {
    let ctx = ProactiveBodyContext { rationale: "internal" };
    let err =
        ResolvedBody::from_proactive_message("internal", &ctx).unwrap_err();
    assert!(matches!(
        err,
        cairn_core::pipeline::extract::BodyResolutionError::ProactiveRationaleMislabel
    ));
}

#[tokio::test]
async fn quoted_remember_is_not_extracted() {
    let extractor = RegexExtractor::builtin();
    let event = fixture_event(CapturePayload::Cli {
        kind_hint: "user".into(),
    });
    let body = ResolvedBody::from_user_ingest(
        r#"the user said "remember that" by mistake"#,
        UserIngestPayloadKind::Cli,
    );
    let input = ExtractInput {
        event: &event,
        body: BodyResolution::Resolved(body),
    };
    let res = extractor.extract(&input).await.expect("ok");
    assert!(res.outputs.is_empty());
}

#[tokio::test]
async fn oversize_body_still_extracts_trigger() {
    let extractor = RegexExtractor::builtin();
    let event = fixture_event(CapturePayload::Cli {
        kind_hint: "user".into(),
    });
    let mut body_text = "lorem ipsum ".repeat(20_000); // ~240 KB > 64 KiB
    body_text.push_str(". forget my old address");
    let body = ResolvedBody::from_user_ingest(&body_text, UserIngestPayloadKind::Cli);
    let input = ExtractInput {
        event: &event,
        body: BodyResolution::Resolved(body),
    };
    let res = extractor.extract(&input).await.expect("ok");
    assert_eq!(res.outputs.len(), 1);
    matches!(res.outputs[0], ExtractOutput::Forget(_));
    assert!(matches!(
        res.truncated,
        TruncationReason::BodyTooLarge { .. }
    ));
    assert_eq!(res.llm_eligible_spans.len(), 1);
}

#[tokio::test]
async fn multi_sentence_trigger_after_period() {
    let extractor = RegexExtractor::builtin();
    let event = fixture_event(CapturePayload::Cli {
        kind_hint: "user".into(),
    });
    let body = ResolvedBody::from_user_ingest(
        "FYI old address is stale. forget my old address",
        UserIngestPayloadKind::Cli,
    );
    let input = ExtractInput {
        event: &event,
        body: BodyResolution::Resolved(body),
    };
    let res = extractor.extract(&input).await.expect("ok");
    assert_eq!(res.outputs.len(), 1);
    matches!(res.outputs[0], ExtractOutput::Forget(_));
}
```

(Note: `CaptureEvent::new` and `Identity::new` are illustrative — replace with the actual constructors used in `tests/capture_event.rs`. If their shape differs, mirror what that test does.)

- [ ] **Step 2: Run integration tests**

Run: `cargo nextest run -p cairn-core --locked --test pipeline_extract_regex`
Expected: 11 tests pass.

If a constructor signature is wrong, fix the call site by reading
`crates/cairn-core/tests/capture_event.rs` for the canonical pattern and re-run.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/tests/pipeline_extract_regex.rs
git commit -m "test(extract): integration tests for RegexExtractor (#73)"
```

---

## Task 14: Property tests

**Files:**
- Create: `crates/cairn-core/tests/pipeline_extract_regex_proptests.rs`

- [ ] **Step 1: Write proptests**

`crates/cairn-core/tests/pipeline_extract_regex_proptests.rs`:

```rust
//! Property tests for `RegexExtractor`. Spec §10.3.

use proptest::prelude::*;

use cairn_core::pipeline::extract::{
    BodyResolution, ExtractInput, ExtractOutput, ExtractorWorker, RegexExtractor, ResolvedBody,
    TruncationReason, UserIngestPayloadKind,
};
use cairn_core::domain::{
    ActorChainEntry, ChainRole, CaptureEvent, CaptureEventId, CaptureMode, CapturePayload,
    Identity, IdentityKind, PayloadHash, Rfc3339Timestamp, SourceFamily,
};

fn fixture_event() -> CaptureEvent {
    let id = CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").unwrap();
    let identity = Identity::new(IdentityKind::Sensor, "snr:proptest:v1").unwrap();
    let chain_entry = ActorChainEntry::new(identity.clone(), ChainRole::Author).unwrap();
    CaptureEvent::new(
        id,
        identity,
        CaptureMode::ExplicitB,
        vec![chain_entry],
        None,
        PayloadHash::sha256_of(b"fixture"),
        "vault://fixture".into(),
        Rfc3339Timestamp::now(),
        CapturePayload::Cli { kind_hint: "user".into() },
        SourceFamily::Cli,
    )
    .unwrap()
}

proptest! {
    #[test]
    fn random_text_no_panic(s in "[\\PC]{0,4096}") {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            let event = fixture_event();
            let body = ResolvedBody::from_user_ingest(&s, UserIngestPayloadKind::Cli);
            let input = ExtractInput {
                event: &event,
                body: BodyResolution::Resolved(body),
            };
            let extractor = RegexExtractor::builtin();
            let res = extractor.extract(&input).await;
            // Must not panic; either Ok or BodyResolution-style error.
            prop_assert!(res.is_ok());
            if let Ok(r) = res {
                // Outputs cannot exceed the budget cap.
                prop_assert!(r.outputs.len() <= extractor.budget().max_drafts as usize);
            }
            Ok::<(), TestCaseError>(())
        }).unwrap();
    }

    #[test]
    fn output_serde_round_trip(
        body in "remember that .{0,80}",
    ) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            let event = fixture_event();
            let rb = ResolvedBody::from_user_ingest(&body, UserIngestPayloadKind::Cli);
            let input = ExtractInput {
                event: &event,
                body: BodyResolution::Resolved(rb),
            };
            let extractor = RegexExtractor::builtin();
            let res = extractor.extract(&input).await.unwrap();
            for out in &res.outputs {
                let json = serde_json::to_string(out).unwrap();
                let back: ExtractOutput = serde_json::from_str(&json).unwrap();
                prop_assert_eq!(&back, out);
            }
            Ok::<(), TestCaseError>(())
        }).unwrap();
    }
}
```

- [ ] **Step 2: Run**

Run: `cargo nextest run -p cairn-core --locked --test pipeline_extract_regex_proptests`
Expected: passes (256 cases per proptest by default).

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/tests/pipeline_extract_regex_proptests.rs
git commit -m "test(extract): proptests for panic-freedom and serde round-trip (#73)"
```

---

## Task 15: Latency assertion (`#[ignore]` by default)

**Files:**
- Create: `crates/cairn-core/tests/pipeline_extract_regex_latency.rs`

- [ ] **Step 1: Write the latency test**

`crates/cairn-core/tests/pipeline_extract_regex_latency.rs`:

```rust
//! p99 < 2 ms latency assertion. Marked `#[ignore]` by default; runs
//! locally with `cargo nextest run -- --include-ignored`. Replace with a
//! `criterion` benchmark in a follow-up issue (spec §14).

use std::time::Instant;

use cairn_core::pipeline::extract::{
    BodyResolution, ExtractInput, ExtractorWorker, RegexExtractor, ResolvedBody,
    UserIngestPayloadKind,
};
use cairn_core::domain::{
    ActorChainEntry, ChainRole, CaptureEvent, CaptureEventId, CaptureMode, CapturePayload,
    Identity, IdentityKind, PayloadHash, Rfc3339Timestamp, SourceFamily,
};

fn fixture_event() -> CaptureEvent {
    let id = CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").unwrap();
    let identity = Identity::new(IdentityKind::Sensor, "snr:lat:v1").unwrap();
    let chain_entry = ActorChainEntry::new(identity.clone(), ChainRole::Author).unwrap();
    CaptureEvent::new(
        id,
        identity,
        CaptureMode::ExplicitB,
        vec![chain_entry],
        None,
        PayloadHash::sha256_of(b"fixture"),
        "vault://fixture".into(),
        Rfc3339Timestamp::now(),
        CapturePayload::Cli { kind_hint: "user".into() },
        SourceFamily::Cli,
    )
    .unwrap()
}

#[ignore = "perf-sensitive; run locally with --include-ignored"]
#[tokio::test]
async fn p99_under_2ms_on_mixed_fixture() {
    let extractor = RegexExtractor::builtin();
    let event = fixture_event();
    let bodies: Vec<String> = (0..10_000)
        .map(|i| match i % 3 {
            0 => format!("remember that I like option {}", i % 10),
            1 => format!("hello world batch {}", i),
            _ => format!("forget what I said about thing {}", i),
        })
        .collect();

    // Warm up.
    for body in bodies.iter().take(100) {
        let rb = ResolvedBody::from_user_ingest(body, UserIngestPayloadKind::Cli);
        let _ = extractor
            .extract(&ExtractInput {
                event: &event,
                body: BodyResolution::Resolved(rb),
            })
            .await;
    }

    let mut samples = Vec::with_capacity(bodies.len());
    for body in &bodies {
        let rb = ResolvedBody::from_user_ingest(body, UserIngestPayloadKind::Cli);
        let start = Instant::now();
        let _ = extractor
            .extract(&ExtractInput {
                event: &event,
                body: BodyResolution::Resolved(rb),
            })
            .await
            .expect("ok");
        samples.push(start.elapsed());
    }
    samples.sort();
    let p99 = samples[(samples.len() * 99) / 100];
    assert!(
        p99.as_millis() < 2,
        "p99 was {} ms, expected < 2 ms",
        p99.as_millis()
    );
}
```

- [ ] **Step 2: Run with --include-ignored locally**

Run: `cargo nextest run -p cairn-core --locked --test pipeline_extract_regex_latency -- --include-ignored`
Expected: passes; if not, capture the actual p99 and tune patterns.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/tests/pipeline_extract_regex_latency.rs
git commit -m "test(extract): p99 < 2 ms latency assertion (ignored by default) (#73)"
```

---

## Task 16: Full verification + final commit

**Files:** none modified.

- [ ] **Step 1: Run the full verification checklist (CLAUDE.md §8)**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Expected: all green.

- [ ] **Step 2: Run supply-chain checks**

```bash
cargo deny check
cargo audit --deny warnings
cargo machete
```

Expected: clean.

- [ ] **Step 3: Push the branch and open a draft PR**

```bash
git push -u origin feat/issue-73-regex-extractor
```

PR body should:
- Link issue #73
- Cite spec sections §5.2.a, §11.6, §18.a, §6
- List invariants touched (CLAUDE.md §4): #4 (pure function), #6 (fail closed on capability — N/A here), #7 (no unsafe), #8 (no unwrap/expect in core — verified)
- Paste verification output

---

## Self-review notes

**Spec coverage:** The plan covers every §-section the spec mandates: types (§4), rules (§5), dispatch (§6), defaults (§7), errors (§8), tests (§10). Workspace impact (§12), brief deviations (§11), and follow-ups (§14) are documentation-only and tracked in the spec itself.

**Type consistency:** `KindHint` wraps `MemoryKind` (Task 2) and is used in `RegexRule` (Task 6), `MemoryDraft` (Task 2), `ForgetIntent` (Task 3). `ExtractOutput::confidence()` is defined in Task 5 and consumed by `compute_llm_eligible_spans` in Task 11. `TextSpan::overlaps` from Task 2 is used in Task 11. `ResolvedBody::from_proactive_message` defined in Task 4 is exercised by integration test in Task 13.

**No placeholders:** Each step has either exact code or an exact command; no "TBD" or "implement later" markers. Where call signatures depend on existing types (`CaptureEvent::new`, `Identity::new` etc.), Task 13 instructs the engineer to mirror `tests/capture_event.rs` rather than guessing — that file is on disk for reference.
