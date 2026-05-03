//! Trace-event projector (issue #77, brief §5.0).
//!
//! Pure helpers that turn a `CaptureEvent` plus a hash-verified
//! `ResolvedBody` into a `MemoryRecord` (Task 6) for any of the seven
//! [`TraceEvent`] variants. This module owns body shaping and the cap that
//! keeps trace records bounded; full bytes stay in `sources/` referenced
//! by `payload_hash`.

use crate::domain::trace::{TraceEvent, TraceLinkError};

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
}
