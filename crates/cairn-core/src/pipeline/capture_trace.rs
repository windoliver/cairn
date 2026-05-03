//! Trace-event projector (issue #77, brief §5.0).
//!
//! Pure helpers that turn a `CaptureEvent` plus a hash-verified
//! `ResolvedBody` into a `MemoryRecord` (Task 6) for any of the seven
//! [`TraceEvent`] variants. This module owns body shaping and the cap that
//! keeps trace records bounded; full bytes stay in `sources/` referenced
//! by `payload_hash`.

use crate::domain::{
    capture::{CaptureEvent, CapturePayload},
    trace::{TraceEvent, TraceLinkError},
};

/// Maximum size of a stored trace-record body, in bytes. Anything larger
/// is truncated at a UTF-8 char boundary; the full bytes remain in
/// `sources/` referenced by `payload_hash`.
pub const TRACE_BODY_CAP: usize = 4 * 1024;

/// Failure modes for the trace projector.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TraceProjectError {
    /// The provided [`crate::domain::trace::TraceLink`] failed field-level
    /// validation.
    #[error("trace link: {0}")]
    Link(#[from] TraceLinkError),
    /// The capture event does not map to any known trace-event type.
    #[error("cannot classify capture event into a trace event type")]
    Unclassifiable,
}

/// Map a [`CaptureEvent`] to a [`TraceEvent`]. Static rules; no LLM.
///
/// Hook payloads route by hook name (brief §9.3). Non-hook payloads are
/// rejected with [`TraceProjectError::Unclassifiable`] in P0.
///
/// # Errors
///
/// Returns [`TraceProjectError::Unclassifiable`] when the event does not
/// match any known mapping.
pub fn classify(event: &CaptureEvent) -> Result<TraceEvent, TraceProjectError> {
    match &event.payload {
        CapturePayload::Hook { hook_name, .. } => match hook_name.as_str() {
            "UserPromptSubmit" => Ok(TraceEvent::UserMessage),
            "PreToolUse" => Ok(TraceEvent::PreTool),
            "PostToolUse" => Ok(TraceEvent::PostTool),
            "Stop" => Ok(TraceEvent::Stop),
            _ => Err(TraceProjectError::Unclassifiable),
        },
        _ => Err(TraceProjectError::Unclassifiable),
    }
}

/// Render the textual body for a given event type. Pure; no I/O.
///
/// Caller is responsible for passing already-privacy-filtered text. The
/// body is truncated at [`TRACE_BODY_CAP`] on a UTF-8 char boundary.
#[must_use]
// Task 6 wires the dispatch; allow dead_code until then.
#[allow(dead_code)]
pub(crate) fn shape_body(_event: TraceEvent, filtered_text: &str) -> String {
    truncate(filtered_text, TRACE_BODY_CAP)
}

// Called only from shape_body; allow until Task 6 wires usage.
#[allow(dead_code)]
fn truncate(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_owned();
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        capture::{
            CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, CaptureRefs, PayloadHash,
            SourceFamily,
        },
        ActorChainEntry, ChainRole, Identity, Rfc3339Timestamp,
    };

    fn ts() -> Rfc3339Timestamp {
        Rfc3339Timestamp::parse("2026-04-27T00:00:00Z").expect("invariant: valid timestamp")
    }

    /// Build a minimal hook `CaptureEvent` for classification tests.
    ///
    /// Copied from `pipeline::squash::tests::hook_event` (squash.rs ~line 503),
    /// parameterised on `hook_name` instead of a fixed `"PostToolUse"`.
    fn mk_hook_event(hook_name: &str) -> CaptureEvent {
        CaptureEvent {
            event_id: CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV")
                .expect("invariant: fixed ULID is valid"),
            sensor_id: Identity::parse("snr:local:hook:cc-session:v1")
                .expect("invariant: fixed sensor identity is valid"),
            capture_mode: CaptureMode::Auto,
            actor_chain: vec![ActorChainEntry {
                role: ChainRole::Author,
                identity: Identity::parse("snr:local:hook:cc-session:v1")
                    .expect("invariant: fixed sensor identity is valid"),
                at: ts(),
            }],
            refs: Some(CaptureRefs {
                session_id: Some("sess".into()),
                turn_id: Some("turn".into()),
                tool_id: None,
            }),
            payload_hash: PayloadHash::parse(
                "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            )
            .expect("invariant: well-formed sha256 hash"),
            payload_ref: "sources/hook/01ARZ3NDEKTSV4RRFFQ69G5FAV.txt".into(),
            captured_at: ts(),
            payload: CapturePayload::Hook {
                hook_name: hook_name.into(),
                tool_name: None,
            },
            source_family: SourceFamily::Hook,
        }
    }

    #[test]
    fn body_truncates_at_cap() {
        let big = "x".repeat(TRACE_BODY_CAP + 100);
        let body = shape_body(TraceEvent::ToolOutput, &big);
        assert!(body.len() <= TRACE_BODY_CAP);
    }

    #[test]
    fn body_below_cap_unchanged() {
        let body = shape_body(TraceEvent::UserMessage, "hello world");
        assert_eq!(body, "hello world");
    }

    #[test]
    fn truncate_respects_char_boundary() {
        // Build a string whose byte length crosses the cap mid multi-byte char.
        let mut s = "x".repeat(TRACE_BODY_CAP - 2);
        s.push('𝄞'); // 4 bytes (U+1D11E)
        let body = shape_body(TraceEvent::UserMessage, &s);
        assert!(body.is_char_boundary(body.len()));
    }

    #[test]
    fn classifies_user_prompt_submit() {
        assert_eq!(
            classify(&mk_hook_event("UserPromptSubmit")).unwrap(),
            TraceEvent::UserMessage
        );
    }

    #[test]
    fn classifies_pre_tool_use() {
        assert_eq!(
            classify(&mk_hook_event("PreToolUse")).unwrap(),
            TraceEvent::PreTool
        );
    }

    #[test]
    fn classifies_post_tool_use() {
        assert_eq!(
            classify(&mk_hook_event("PostToolUse")).unwrap(),
            TraceEvent::PostTool
        );
    }

    #[test]
    fn classifies_stop() {
        assert_eq!(
            classify(&mk_hook_event("Stop")).unwrap(),
            TraceEvent::Stop
        );
    }

    #[test]
    fn unknown_hook_rejected() {
        assert!(matches!(
            classify(&mk_hook_event("UnknownHook")).unwrap_err(),
            TraceProjectError::Unclassifiable
        ));
    }

    #[test]
    fn session_start_rejected() {
        assert!(matches!(
            classify(&mk_hook_event("SessionStart")).unwrap_err(),
            TraceProjectError::Unclassifiable
        ));
    }
}
