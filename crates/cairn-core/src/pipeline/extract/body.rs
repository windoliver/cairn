//! Body resolution: encapsulated, source-tagged user-text input for
//! the extractor. See spec §4.1.
//!
//! `ResolvedBody`'s fields are private. Construction goes through one
//! of the named functions below, each tied to a specific `BodySource`.

use serde::{Deserialize, Serialize};

/// The trust boundary a resolved body came from.
///
/// **There is deliberately no `Rationale` variant.** The
/// `ProactiveMessage` variant is currently unreachable in this
/// incarnation: `CapturePayload::Proactive` does not yet carry a
/// user-visible message-body field, so no constructor on `ResolvedBody`
/// can prove provenance for one. The variant is retained as a
/// forward-compatible target for the proactive-body follow-up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BodySource {
    /// `cairn ingest` Mode B body — the user-supplied text payload of
    /// a `Cli` or `Mcp` envelope.
    UserIngest,
    /// User utterance captured by a harness hook (e.g.
    /// `UserPromptSubmit`).
    HookUtterance,
    /// Proactive Mode C body — the user-visible message body the agent
    /// produced. Distinct from `Proactive.rationale`, which never
    /// reaches this enum.
    ProactiveMessage,
}

/// Marker for the `Cli` / `Mcp` payload variant the bytes came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserIngestPayloadKind {
    /// `CapturePayload::Cli` (Mode B via the CLI).
    Cli,
    /// `CapturePayload::Mcp` (Mode B via MCP).
    Mcp,
}

// `ProactiveMessageRef` and `ResolvedBody::from_proactive_message` were
// removed in adversarial-review round 3 (#73). `CapturePayload::Proactive`
// does not yet carry a user-visible message-body field, so any
// "verified" wrapper that takes `&str + &CapturePayload` cannot actually
// bind the bytes to message-body provenance — a misbehaving resolver
// could pass arbitrary text. Re-introducing this constructor requires
// the capture envelope to gain a typed message-body field that the
// wrapper can hash/compare against. Tracked for the proactive-body
// follow-up issue.

/// Reasons body resolution may fail.
#[derive(Clone, Debug, thiserror::Error, PartialEq)]
#[non_exhaustive]
pub enum BodyResolutionError {
    /// `payload_ref` did not resolve to any bytes.
    #[error("payload_ref not found: {0}")]
    NotFound(String),
    /// Bytes decoded but did not match `payload_hash`.
    #[error("payload_hash mismatch (expected {expected}, got {got})")]
    HashMismatch {
        /// Hash declared on the envelope.
        expected: String,
        /// Hash computed over the resolved bytes.
        got: String,
    },
    /// Bytes did not decode as UTF-8.
    #[error("payload bytes are not valid UTF-8")]
    NotUtf8,
    /// Transient I/O error reading `payload_ref`.
    #[error("transient I/O error reading payload_ref: {0}")]
    Io(String),
}

/// Resolved body bytes plus their trust-boundary source.
///
/// Fields are private; callers go through the named constructors.
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

    /// The resolved body text.
    #[must_use]
    pub fn text(&self) -> &str {
        self.text
    }

    /// The source the body came from.
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
    /// Body bytes were materialised and verified.
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
    fn body_source_has_no_rationale_variant() {
        // Exhaustive match. If a new variant is added, this test
        // breaks and the reviewer must explicitly justify it.
        // Arms are kept separate (not merged with `|`) so that adding a
        // new variant produces a non-exhaustive-match compile error
        // here — that is the entire point of this guard test.
        #[allow(clippy::match_same_arms)]
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
