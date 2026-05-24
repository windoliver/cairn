//! Body resolution: encapsulated, source-tagged user-text input for
//! the extractor. See spec §4.1.
//!
//! `ResolvedBody`'s fields are private. Construction goes through one
//! of the named functions below, each tied to a specific `BodySource`.

use serde::{Deserialize, Serialize};

use crate::domain::CapturePayload;

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
    /// A `ResolvedBody::from_*` constructor was called with a payload
    /// variant that does not match the declared body source.
    #[error("payload variant {got} does not match expected {expected}")]
    PayloadVariantMismatch {
        /// The expected payload variant family.
        expected: &'static str,
        /// The actual payload variant the caller passed.
        got: &'static str,
    },
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
    /// Construct from a `CapturePayload::Cli` or `::Mcp` envelope. The
    /// caller must pass the actual payload so the variant family can be
    /// verified at runtime — without this the body source label is
    /// nothing more than caller-asserted metadata.
    ///
    /// # Errors
    ///
    /// Returns [`BodyResolutionError::PayloadVariantMismatch`] if `payload`
    /// is not the `Cli` or `Mcp` variant.
    pub fn from_user_ingest(
        text: &'a str,
        payload: &CapturePayload,
        payload_kind: UserIngestPayloadKind,
    ) -> Result<Self, BodyResolutionError> {
        let matches = matches!(
            (payload_kind, payload),
            (UserIngestPayloadKind::Cli, CapturePayload::Cli { .. })
                | (UserIngestPayloadKind::Mcp, CapturePayload::Mcp { .. })
        );
        if !matches {
            return Err(BodyResolutionError::PayloadVariantMismatch {
                expected: match payload_kind {
                    UserIngestPayloadKind::Cli => "CapturePayload::Cli",
                    UserIngestPayloadKind::Mcp => "CapturePayload::Mcp",
                },
                got: payload_variant_name(payload),
            });
        }
        Ok(Self {
            text,
            source: BodySource::UserIngest,
        })
    }

    /// Construct from a `CapturePayload::Hook` envelope. Only hooks
    /// that canonically carry **user utterance text** are accepted —
    /// tool-output hooks like `PostToolUse` cannot be re-tagged as
    /// `HookUtterance` and persisted as user memory.
    ///
    /// # Errors
    ///
    /// Returns [`BodyResolutionError::PayloadVariantMismatch`] if
    /// `payload` is not the `Hook` variant, its `hook_name` differs
    /// from the caller-supplied `hook_name`, or `hook_name` is not a
    /// recognised user-utterance hook.
    pub fn from_hook_utterance(
        text: &'a str,
        payload: &CapturePayload,
        hook_name: &str,
    ) -> Result<Self, BodyResolutionError> {
        let CapturePayload::Hook {
            hook_name: payload_hook,
            ..
        } = payload
        else {
            return Err(BodyResolutionError::PayloadVariantMismatch {
                expected: "CapturePayload::Hook",
                got: payload_variant_name(payload),
            });
        };
        if payload_hook != hook_name {
            return Err(BodyResolutionError::PayloadVariantMismatch {
                expected: "CapturePayload::Hook",
                got: "CapturePayload::Hook (hook_name mismatch)",
            });
        }
        if !is_user_utterance_hook(hook_name) {
            return Err(BodyResolutionError::PayloadVariantMismatch {
                expected: "CapturePayload::Hook (user-utterance hook)",
                got: "CapturePayload::Hook (non-user-utterance hook)",
            });
        }
        Ok(Self {
            text,
            source: BodySource::HookUtterance,
        })
    }

    /// Construct from hash-verified bytes read from `sources/` by the
    /// trace-capture pipeline (brief §5.0). The caller must have already
    /// verified `sha256(bytes) == event.payload_hash` before calling this
    /// constructor. Intended for hook events that are **not** user-utterance
    /// hooks (e.g. `PreToolUse`, `PostToolUse`, `Stop`) — those hooks carry
    /// tool/agent content rather than direct user text, so `HookUtterance`
    /// is not an appropriate source tag. The source is recorded as
    /// `HookUtterance` because that is the closest match for
    /// harness-captured hook payload bytes; a future task will introduce a
    /// dedicated `HookCapture` or `TracePayload` variant.
    ///
    /// **Do not use outside the `capture_trace` verb pipeline.** Every other
    /// body resolution path should go through the typed constructors
    /// (`from_user_ingest`, `from_hook_utterance`) which enforce
    /// payload-variant invariants.
    #[must_use]
    pub fn from_trace_hook(text: &'a str) -> Self {
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

/// Construct a `ResolvedBody` for **unit tests only**, bypassing the
/// payload-variant checks. This is `pub(crate)` so `pipeline::capture_trace`
/// tests can build a body for non-user-utterance hooks (e.g. `PreTool`)
/// without duplicating its fields into a `CapturePayload::Hook`.
#[cfg(test)]
pub(crate) fn for_test(text: &str) -> ResolvedBody<'_> {
    ResolvedBody {
        text,
        source: BodySource::HookUtterance,
    }
}

/// Hook names whose payload body canonically carries direct user
/// utterance text. Adding a new entry is a trust-boundary change —
/// the hook must be one whose payload contains text the user actually
/// typed, not tool output or agent-produced content.
const USER_UTTERANCE_HOOKS: &[&str] = &["UserPromptSubmit"];

/// True when `hook_name` is a hook whose payload body canonically
/// carries direct user utterance text. Single source of truth for the
/// trust-boundary decision: callers anywhere in `cairn-core` (e.g.
/// dispatch's text-bearing payload classifier) MUST consult this
/// instead of duplicating the allowlist.
#[must_use]
pub fn is_user_utterance_hook(hook_name: &str) -> bool {
    USER_UTTERANCE_HOOKS.contains(&hook_name)
}

fn payload_variant_name(p: &CapturePayload) -> &'static str {
    match p {
        CapturePayload::Cli { .. } => "CapturePayload::Cli",
        CapturePayload::Mcp { .. } => "CapturePayload::Mcp",
        CapturePayload::Hook { .. } => "CapturePayload::Hook",
        CapturePayload::Terminal { .. } => "CapturePayload::Terminal",
        CapturePayload::Ide { .. } => "CapturePayload::Ide",
        CapturePayload::Voice { .. } => "CapturePayload::Voice",
        CapturePayload::Screen { .. } => "CapturePayload::Screen",
        CapturePayload::Clipboard { .. } => "CapturePayload::Clipboard",
        CapturePayload::Proactive { .. } => "CapturePayload::Proactive",
        CapturePayload::RecordingBatch { .. } => "CapturePayload::RecordingBatch",
        CapturePayload::External { .. } => "CapturePayload::External",
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
        let payload = CapturePayload::Cli {
            kind_hint: "user".into(),
        };
        let body = ResolvedBody::from_user_ingest("hello", &payload, UserIngestPayloadKind::Cli)
            .expect("matching variant");
        assert_eq!(body.text(), "hello");
        assert_eq!(body.source(), BodySource::UserIngest);
    }

    #[test]
    fn from_user_ingest_rejects_variant_mismatch() {
        let payload = CapturePayload::Hook {
            hook_name: "PostToolUse".into(),
            tool_name: None,
        };
        let err = ResolvedBody::from_user_ingest("hello", &payload, UserIngestPayloadKind::Cli)
            .unwrap_err();
        assert!(matches!(
            err,
            BodyResolutionError::PayloadVariantMismatch { .. }
        ));
    }

    #[test]
    fn from_hook_utterance_tags_correctly() {
        let payload = CapturePayload::Hook {
            hook_name: "UserPromptSubmit".into(),
            tool_name: None,
        };
        let body = ResolvedBody::from_hook_utterance("hi", &payload, "UserPromptSubmit")
            .expect("matching hook");
        assert_eq!(body.source(), BodySource::HookUtterance);
    }

    #[test]
    fn from_hook_utterance_rejects_non_user_hook() {
        // PostToolUse is not a user-utterance hook; even a matching
        // hook_name string must be rejected so tool-output text cannot
        // be persisted as user memory.
        let payload = CapturePayload::Hook {
            hook_name: "PostToolUse".into(),
            tool_name: None,
        };
        let err = ResolvedBody::from_hook_utterance("hi", &payload, "PostToolUse").unwrap_err();
        assert!(matches!(
            err,
            BodyResolutionError::PayloadVariantMismatch { .. }
        ));
    }

    #[test]
    fn from_hook_utterance_rejects_hook_name_mismatch() {
        let payload = CapturePayload::Hook {
            hook_name: "PostToolUse".into(),
            tool_name: None,
        };
        let err =
            ResolvedBody::from_hook_utterance("hi", &payload, "UserPromptSubmit").unwrap_err();
        assert!(matches!(
            err,
            BodyResolutionError::PayloadVariantMismatch { .. }
        ));
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
        let payload = CapturePayload::Cli {
            kind_hint: "user".into(),
        };
        let resolved = BodyResolution::Resolved(
            ResolvedBody::from_user_ingest("hi", &payload, UserIngestPayloadKind::Cli)
                .expect("matching variant"),
        );
        assert!(resolved.allows_text_rules());

        let na: BodyResolution<'_> = BodyResolution::NotApplicable;
        assert!(!na.allows_text_rules());

        let failed: BodyResolution<'_> =
            BodyResolution::Failed(BodyResolutionError::NotFound("nope".into()));
        assert!(!failed.allows_text_rules());
    }
}
