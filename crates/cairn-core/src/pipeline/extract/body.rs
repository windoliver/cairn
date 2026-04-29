//! Body resolution: encapsulated, source-tagged user-text input for
//! the extractor. See spec §4.1.
//!
//! `ResolvedBody`'s fields are private. Construction goes through one
//! of the named functions below, each tied to a specific `BodySource`.

use serde::{Deserialize, Serialize};

use crate::domain::CapturePayload;

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

/// Verified reference to the user-visible message body of a
/// `CapturePayload::Proactive` envelope.
///
/// Constructed only via [`ProactiveMessageRef::from_payload`], which
/// requires an actual `&CapturePayload::Proactive` and rejects text
/// equal to the payload's `rationale`. Binding the type to the payload
/// envelope means callers cannot synthesise a free-floating "proactive
/// message" from arbitrary bytes.
#[derive(Clone, Copy, Debug)]
pub struct ProactiveMessageRef<'a> {
    text: &'a str,
}

impl<'a> ProactiveMessageRef<'a> {
    /// Construct from an already-resolved user-visible message body and
    /// the originating `CapturePayload::Proactive` envelope.
    ///
    /// # Errors
    ///
    /// - [`BodyResolutionError::ProactivePayloadMismatch`] if `payload`
    ///   is not the `Proactive` variant.
    /// - [`BodyResolutionError::ProactiveRationaleMislabel`] if `text`
    ///   is byte-equal to the payload's `rationale`.
    pub fn from_payload(
        text: &'a str,
        payload: &'a CapturePayload,
    ) -> Result<Self, BodyResolutionError> {
        let CapturePayload::Proactive { rationale, .. } = payload else {
            return Err(BodyResolutionError::ProactivePayloadMismatch);
        };
        if text == rationale {
            return Err(BodyResolutionError::ProactiveRationaleMislabel);
        }
        Ok(Self { text })
    }

    /// The verified user-visible message text.
    #[must_use]
    pub fn text(&self) -> &'a str {
        self.text
    }
}

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
    /// `ProactiveMessageRef::from_payload` was called with text equal to
    /// the `rationale` field — refusing to extract internal reasoning
    /// as user memory.
    #[error(
        "ProactiveMessageRef::from_payload called with text equal to rationale — refusing to extract internal reasoning as user memory"
    )]
    ProactiveRationaleMislabel,
    /// `ProactiveMessageRef::from_payload` was called with a payload
    /// variant other than `CapturePayload::Proactive`.
    #[error("ProactiveMessageRef::from_payload requires CapturePayload::Proactive")]
    ProactivePayloadMismatch,
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

    /// Construct from a verified `ProactiveMessageRef`. The reference
    /// itself encodes the trust boundary: it can only be produced via
    /// [`ProactiveMessageRef::from_payload`], which requires an actual
    /// `&CapturePayload::Proactive` envelope.
    #[must_use]
    pub fn from_proactive_message(msg: ProactiveMessageRef<'a>) -> Self {
        Self {
            text: msg.text,
            source: BodySource::ProactiveMessage,
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

    fn proactive_payload(rationale: &str) -> CapturePayload {
        CapturePayload::Proactive {
            kind: "feedback".into(),
            rationale: rationale.into(),
        }
    }

    #[test]
    fn from_proactive_message_accepts_distinct_text() {
        let payload = proactive_payload("internal-reasoning");
        let msg =
            ProactiveMessageRef::from_payload("user-visible message", &payload).expect("distinct");
        let body = ResolvedBody::from_proactive_message(msg);
        assert_eq!(body.source(), BodySource::ProactiveMessage);
    }

    #[test]
    fn from_proactive_message_rejects_rationale_mislabel() {
        let payload = proactive_payload("secret rationale");
        let err = ProactiveMessageRef::from_payload("secret rationale", &payload).unwrap_err();
        assert_eq!(err, BodyResolutionError::ProactiveRationaleMislabel);
    }

    #[test]
    fn from_proactive_message_rejects_non_proactive_payload() {
        let payload = CapturePayload::Cli {
            kind_hint: "user".into(),
        };
        let err = ProactiveMessageRef::from_payload("anything", &payload).unwrap_err();
        assert_eq!(err, BodyResolutionError::ProactivePayloadMismatch);
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
