//! Filter stage of the write path (brief §5.2 — `shouldMemorize` + `redact`
//! + `fence`).
//!
//! The Filter sits between Extract and Classify. Its three pure transforms:
//!
//! 1. [`redact()`] — strip PII and secret-shaped text from a payload
//!    **before** any persistence. Returns the masked text plus a
//!    body-free vector of redaction spans for audit.
//! 2. [`fence()`] — wrap prompt-injection patterns in sentinel markers
//!    so downstream LLM extractors do not treat them as instructions.
//!    Body-byte-preserving outside fenced spans.
//! 3. [`should_memorize`] — decide `Proceed` vs one of the §5.2 discard
//!    reasons (`volatile`, `tool_lookup`, `competing_source`,
//!    `low_salience`, `pii_blocked`, `policy_blocked`, `duplicate`).
//! 4. [`default_visibility`] — deterministic visibility floor per
//!    (`IdentityKind` × `CaptureMode` × `SourceFamily` × [`VisibilityPolicy`])
//!    matrix (§6.3).
//!
//! Blocked decisions emit a [`BlockedAuditEntry`] containing only metadata
//! — never the raw body — so downstream observability and consent journals
//! can record what was rejected without re-introducing the bytes the
//! filter just refused.

pub mod audit;
pub mod decision;
pub mod fence;
pub mod redact;
pub mod visibility;

pub use audit::BlockedAuditEntry;
pub use decision::{Decision, DiscardReason, FilterInputs, should_memorize};
pub use fence::{FenceMark, FencedPayload, fence};
pub use redact::{RedactedPayload, RedactionSpan, RedactionTag, redact};
pub use visibility::{VisibilityPolicy, default_visibility};
