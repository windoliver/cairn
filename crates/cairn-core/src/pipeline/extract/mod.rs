//! Extract stage of the write pipeline (brief §5.2, §5.2.a).
//!
//! Produces `ExtractOutput` (drafts and forget intents) from a
//! `CaptureEvent` plus a caller-resolved body. Pure functions and pure
//! data — no I/O, no async outside the trait method itself.
//!
//! See `docs/superpowers/specs/2026-04-28-issue-73-regex-extractor-design.md`
//! for the contract this module implements.

// Submodules land in subsequent tasks.

pub mod body;
pub mod chain;
pub mod draft;
pub mod intent;
pub mod regex;

pub use body::{
    BodyResolution, BodyResolutionError, BodySource, ResolvedBody, UserIngestPayloadKind,
    is_user_utterance_hook,
};
pub use chain::{
    ChainResult, ChainRunError, ExtractChain, ExtractChainBuildError, WorkerFailure,
};
pub use draft::{Confidence, ConfidenceError, KindHint, MemoryDraft, TextSpan};
pub use intent::{ForgetIntent, ForgetMatchStrategy};
pub use regex::RegexExtractor;
pub use regex::rule::{RegexRule, ToolFrameFamily};

use crate::domain::CaptureEvent;
use serde::{Deserialize, Serialize};

/// Confidence at or above this value causes a regex output's span to
/// suppress LLM re-extraction on that span. See spec §6.5.
pub const CONFIDENCE_GATE_FOR_SUPPRESSION: f32 = 0.9;

/// Maximum body length, in bytes, that Phase B (user rules) will scan.
/// Bodies above this still run Phase A on prefilter hits; spec §6.3.
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
    /// The capture envelope being processed.
    pub event: &'a CaptureEvent,
    /// The caller-resolved body, with source-tagging.
    pub body: BodyResolution<'a>,
    /// Byte ranges of `body` (in original-body coordinates, inclusive
    /// of nothing-fenced) that this extractor is permitted to inspect.
    /// Initialised by `ExtractChain` and narrowed by each `Gating`
    /// worker. An empty `Vec` means "nothing eligible — extractor
    /// should return `Ok(empty)` without doing work".
    pub eligible_spans: Vec<TextSpan>,
}

/// Per-extractor budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtractBudget {
    /// Wall-clock budget in milliseconds.
    pub max_wall_ms: u32,
    /// Maximum number of **user-rule** outputs in a single extraction.
    ///
    /// Built-in outputs (first-party `remember`/`forget` triggers,
    /// built-in hook events, built-in tool-frame events) are
    /// **not** counted against this cap — explicit user intents must
    /// never be silently dropped under back-pressure. Callers that
    /// need a hard total-output bound must apply it at the chain
    /// dispatcher above this extractor.
    pub max_drafts: u16,
    /// Maximum input prompt size in bytes. Local `DoS` guard, rejected
    /// before provider call.
    pub max_prompt_bytes: Option<u32>,
    /// Maximum input prompt tokens (provider-side hint, passed via
    /// `CompletionRequest.budget`).
    pub max_prompt_tokens: Option<u32>,
    /// Maximum response tokens (provider-side hint, passed via
    /// `CompletionRequest.budget`).
    pub max_response_tokens: Option<u32>,
}

impl ExtractBudget {
    /// Default budget for `RegexExtractor`.
    #[must_use]
    pub const fn regex_default() -> Self {
        Self {
            max_wall_ms: MAX_PHASE_A_WALL_MS,
            max_drafts: 16,
            max_prompt_bytes: None,
            max_prompt_tokens: None,
            max_response_tokens: None,
        }
    }

    /// Default budget for `LLMExtractor`.
    #[must_use]
    pub const fn llm_default() -> Self {
        Self {
            max_wall_ms: 500,
            max_drafts: 16,
            max_prompt_bytes: Some(64 * 1024),
            max_prompt_tokens: Some(2000),
            max_response_tokens: Some(1500),
        }
    }
}

/// One extracted output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExtractOutput {
    /// A `MemoryDraft` from a memorise trigger.
    Draft(MemoryDraft),
    /// A `ForgetIntent` from a forget trigger.
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

/// Why the LLM extractor proposed not memorising a span. Mirrors a
/// subset of brief §5.2 Filter discard reasons — Filter, not the
/// extractor, is authoritative on `pii_blocked`, `policy_blocked`,
/// `duplicate`, so those are not represented here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DiscardReason {
    /// Transient or short-lived signal (e.g. ephemeral status check).
    Volatile,
    /// One-off tool lookup that produces no durable knowledge.
    ToolLookup,
    /// Same fact already covered by another (likely better) source.
    CompetingSource,
    /// Low salience — judged not worth memorising.
    LowSalience,
    /// Other reason (model-supplied free text in `evidence`).
    Other,
}

/// A model-suggested discard, with the same provenance shape as a
/// draft. Filter (a later issue) decides whether to honour the
/// suggestion or override; this type is the wire format between
/// `LLMExtractor` and Filter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiscardCandidate {
    /// Why the model proposes discarding this span.
    pub reason: DiscardReason,
    /// Span the discard applies to (in original-body coordinates).
    pub source_span: TextSpan,
    /// Model-emitted free-text justification, ≤ 512 chars (schema-enforced upstream).
    pub evidence: String,
}

/// Why an extraction was truncated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TruncationReason {
    /// No truncation occurred.
    None,
    /// `budget.max_drafts` cap was reached during Phase B.
    MaxDrafts,
    /// `budget.max_wall_ms` was exceeded during Phase B.
    MaxWallMs {
        /// Wall-clock elapsed when the cap fired.
        elapsed_ms: u32,
    },
    /// Body exceeded `MAX_BODY_LEN_FOR_REGEX`; Phase B was skipped.
    BodyTooLarge {
        /// Body length in bytes.
        body_len: u32,
    },
    /// Hit the per-body phrase-window cap (`MAX_PHRASE_WINDOWS`).
    ClauseCapExceeded {
        /// How many windows were processed before the cap.
        processed: u8,
        /// Total body length in bytes.
        body_len: u32,
    },
}

/// Result envelope returned by `ExtractorWorker::extract`.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtractResult {
    /// Drafts and forget intents produced.
    pub outputs: Vec<ExtractOutput>,
    /// Whether and why the extractor truncated.
    pub truncated: TruncationReason,
    /// Byte ranges of the body that the LLM extractor in the chain
    /// SHOULD run on. See spec §6.4 / §6.5.
    pub llm_eligible_spans: Vec<TextSpan>,
}

/// Errors any extractor may surface.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ExtractError {
    /// Wall-clock budget exhausted with zero outputs (chain falls
    /// through to next extractor).
    #[error("extractor `{worker}` exceeded budget after {elapsed_ms} ms")]
    BudgetExceeded {
        /// Name of the extractor whose budget was exhausted.
        worker: &'static str,
        /// Elapsed wall-clock time when the budget was exhausted.
        elapsed_ms: u32,
    },
    /// A user-supplied or built-in rule failed to compile or validate.
    #[error("invalid rule `{rule_id}`: {reason}")]
    InvalidRule {
        /// Stable id of the rejected rule.
        rule_id: String,
        /// Human-readable explanation.
        reason: String,
    },
    /// The caller's body resolution failed.
    #[error("body resolution failed for event {event_id}")]
    BodyResolution {
        /// `CaptureEvent` id (for cross-referencing logs / traces).
        event_id: String,
        /// Underlying resolution error.
        #[source]
        source: BodyResolutionError,
    },
    /// A text-bearing event was passed with `BodyResolution::NotApplicable`.
    /// Returned when the payload variant (e.g. `Cli`, `Mcp`, or a known
    /// user-utterance hook) is one whose body should always be resolved
    /// before extraction. Silently treating the body as absent would
    /// drop explicit `remember`/`forget` intents.
    #[error("missing body for text-bearing event {event_id} (payload variant `{payload_variant}`)")]
    MissingBody {
        /// `CaptureEvent` id (for cross-referencing logs / traces).
        event_id: String,
        /// The payload variant family that should have produced a body.
        payload_variant: &'static str,
    },
    /// Provider returned an error the extractor cannot recover from.
    /// The chain runner captures these into `ChainResult.failures`.
    #[error("provider failure in extractor `{worker}`: {code}")]
    Provider {
        /// Which extractor surfaced the error.
        worker: &'static str,
        /// Stable error class for metrics; matches `LlmError` discriminant
        /// stringified, e.g. `"unreachable"`, `"auth_denied"`.
        code: &'static str,
        /// Underlying provider error.
        #[source]
        source: crate::contract::llm_provider::LlmError,
    },
    /// Model emitted only items the parser had to drop — out-of-range
    /// `region_id`, `text_excerpt` not present in the named region, or
    /// (defence-in-depth) a derived span outside `eligible_spans`.
    /// Per-item drops happen with `tracing::warn!` + a metric; this
    /// error is *only* raised when the model emits at least one item
    /// but zero items survive validation.
    #[error("extractor `{worker}` emitted only items the parser had to drop")]
    SpanOutOfBounds {
        /// Which extractor produced the unsalvageable result.
        worker: &'static str,
    },
}

/// The role of an extractor worker in the extraction chain.
///
/// An extractor's role determines how the chain reacts to its failure and
/// how it manages eligibility bounds for downstream workers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerRole {
    /// The worker narrows eligibility for downstream workers. A failure
    /// in a Gating worker fails the chain closed (`eligibility` → empty).
    ///
    /// `RegexExtractor` is `Gating` because its `llm_eligible_spans`
    /// is the only safety boundary against oversize bodies, clause-cap
    /// truncation, and confidence-gated forget suppression reaching
    /// downstream workers (e.g., the LLM).
    Gating,
    /// The worker adds extraction outputs within already-narrowed
    /// eligibility but does not itself narrow. Failure does not affect
    /// downstream eligibility.
    ///
    /// `LLMExtractor` is `Augmenting` because it operates within the
    /// eligibility bounds set by upstream workers and does not further
    /// restrict what downstream workers can see.
    Augmenting,
}

/// The pluggable extractor contract — see brief §5.2.a and spec §4.1.
///
/// `#[async_trait]` is used because the chain dispatcher in #74 holds
/// extractors as `Box<dyn ExtractorWorker>`, and a trait with native
/// `async fn` is not object-safe in Rust 1.95. CLAUDE.md §6.3
/// explicitly carves this out.
#[async_trait::async_trait]
pub trait ExtractorWorker: Send + Sync {
    /// Stable name of the extractor (for tracing / dedup).
    fn name(&self) -> &'static str;
    /// The role of this worker in the extraction chain.
    fn role(&self) -> WorkerRole;
    /// Per-extractor budget.
    fn budget(&self) -> ExtractBudget;
    /// Run the extractor.
    async fn extract(&self, input: &ExtractInput<'_>) -> Result<ExtractResult, ExtractError>;
}

#[cfg(test)]
mod mod_tests {
    use super::*;
    use crate::domain::CaptureEventId;
    use crate::domain::taxonomy::MemoryKind;

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
        assert!((draft.confidence().as_f32() - 0.95).abs() < f32::EPSILON);
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
            TruncationReason::ClauseCapExceeded {
                processed: 64,
                body_len: 1024,
            },
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

    #[test]
    fn llm_default_budget_matches_spec() {
        let b = ExtractBudget::llm_default();
        assert_eq!(b.max_wall_ms, 500);
        assert_eq!(b.max_drafts, 16);
        assert_eq!(b.max_prompt_bytes, Some(64 * 1024));
        assert_eq!(b.max_prompt_tokens, Some(2000));
        assert_eq!(b.max_response_tokens, Some(1500));
    }

    #[test]
    fn regex_default_budget_keeps_token_fields_none() {
        let b = ExtractBudget::regex_default();
        assert!(b.max_prompt_bytes.is_none());
        assert!(b.max_prompt_tokens.is_none());
        assert!(b.max_response_tokens.is_none());
    }

    #[tokio::test]
    async fn regex_extractor_is_gating() {
        use crate::pipeline::extract::regex::RegexExtractor;
        let r = RegexExtractor::builtin();
        assert_eq!(r.role(), WorkerRole::Gating);
    }

    #[test]
    fn extract_error_provider_display() {
        let e = ExtractError::Provider {
            worker: "llm",
            code: "unreachable",
            source: crate::contract::llm_provider::LlmError::ProviderUnreachable {
                detail: "connect refused".into(),
            },
        };
        let s = e.to_string();
        assert!(s.contains("provider failure"), "got: {s}");
        assert!(s.contains("unreachable"), "got: {s}");
    }

    #[test]
    fn extract_error_span_out_of_bounds_display() {
        let e = ExtractError::SpanOutOfBounds { worker: "llm" };
        assert!(e.to_string().contains("had to drop"));
    }

    #[test]
    fn discard_candidate_round_trips_via_serde() {
        let dc = DiscardCandidate {
            reason: DiscardReason::Volatile,
            source_span: TextSpan::new(0, 5),
            evidence: "transient lookup".to_owned(),
        };
        let json = serde_json::to_string(&dc).unwrap();
        let back: DiscardCandidate = serde_json::from_str(&json).unwrap();
        assert_eq!(back, dc);
    }

    #[test]
    fn discard_reason_serializes_snake_case() {
        let r = DiscardReason::ToolLookup;
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(s, "\"tool_lookup\"");
    }

    #[test]
    fn extract_input_carries_eligible_spans() {
        use crate::domain::{
            ActorChainEntry, CaptureEventId, CaptureMode, CapturePayload, CaptureRefs, ChainRole,
            Identity, PayloadHash, Rfc3339Timestamp,
        };
        use crate::pipeline::extract::body::BodyResolution;
        let payload = CapturePayload::Hook {
            hook_name: "SessionStart".to_owned(),
            tool_name: None,
        };
        let source_family = payload.source_family();
        let ts = Rfc3339Timestamp::parse("2026-01-01T00:00:00Z").expect("valid ts");
        let sensor = Identity::parse("snr:local:hook:test:v1").expect("valid sensor");
        let event = crate::domain::CaptureEvent {
            event_id: CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ulid"),
            sensor_id: sensor.clone(),
            capture_mode: CaptureMode::Auto,
            actor_chain: vec![ActorChainEntry {
                role: ChainRole::Author,
                identity: sensor,
                at: ts.clone(),
            }],
            refs: Some(CaptureRefs {
                session_id: Some("sess-test".into()),
                turn_id: None,
                tool_id: None,
            }),
            payload_hash: PayloadHash::parse(format!("sha256:{}", "ab".repeat(32)))
                .expect("valid hash"),
            payload_ref: "sources/hook/01ARZ3NDEKTSV4RRFFQ69G5FAV.json".into(),
            captured_at: ts,
            payload,
            source_family,
        };
        let input = ExtractInput {
            event: &event,
            body: BodyResolution::NotApplicable,
            eligible_spans: vec![TextSpan::new(0, 5)],
        };
        assert_eq!(input.eligible_spans.len(), 1);
        assert_eq!(input.eligible_spans[0], TextSpan::new(0, 5));
    }
}
