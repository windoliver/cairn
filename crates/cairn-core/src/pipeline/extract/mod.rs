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
pub mod draft;
pub mod intent;
pub mod regex;

pub use body::{
    BodyResolution, BodyResolutionError, BodySource, ProactiveBodyContext, ResolvedBody,
    UserIngestPayloadKind,
};
pub use draft::{Confidence, ConfidenceError, KindHint, MemoryDraft, TextSpan};
pub use intent::{ForgetIntent, ForgetMatchStrategy};
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
}

/// Per-extractor budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtractBudget {
    /// Wall-clock budget in milliseconds.
    pub max_wall_ms: u32,
    /// Maximum number of outputs.
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
}
