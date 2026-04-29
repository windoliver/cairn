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

pub use body::{
    BodyResolution, BodyResolutionError, BodySource, ProactiveBodyContext, ResolvedBody,
    UserIngestPayloadKind,
};
pub use draft::{Confidence, ConfidenceError, KindHint, MemoryDraft, TextSpan};
pub use intent::{ForgetIntent, ForgetMatchStrategy};
