//! Pre-capture redaction pipeline (issue #130, brief §5.2 + §14).
//!
//! Walks [`ConnectorPayload`] leaves and applies
//! `cairn_core::pipeline::filter::redact::redact` to every text string.
//! Returns a [`Redacted`] envelope containing the post-redaction event and
//! the union of all [`RedactionSpan`]s collected across leaves.
//!
//! # Span offsets
//!
//! Span byte-offsets are **relative to the individual leaf string**, not to
//! the enclosing JSON document or the full event body. Each [`RedactionSpan`]
//! records `start`/`end` byte positions within the original (pre-redaction)
//! text of that leaf.
//!
//! # Binary payloads
//!
//! [`ConnectorPayload::Binary`] bytes are spooled to a temporary path by the
//! framework before reaching this pipeline. The envelope itself contains only
//! metadata (MIME type, SHA-256 hash, spool reference), so no string-walking
//! is necessary or performed; the event passes through unchanged with an empty
//! span list.

use cairn_core::pipeline::filter::redact::{self, RedactionSpan};
use serde_json::Value;

use crate::error::ConnectorError;
use crate::event::{ConnectorEvent, ConnectorPayload};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A connector event after the redaction pipeline has run.
///
/// `event` holds the post-redaction payload (PII replaced with
/// `[REDACTED:<tag>]`). `spans` is the union of every [`RedactionSpan`]
/// collected across all text leaves; span byte offsets reference the
/// **pre-redaction bytes of each individual leaf**, not the JSON document
/// position.
#[derive(Debug)]
pub struct Redacted {
    /// The post-redaction connector event. All string leaves have been
    /// sanitised; it is safe to forward this to the consent gate.
    pub event: ConnectorEvent,
    /// All redaction spans fired across every text leaf.
    ///
    /// Byte offsets (`start`, `end`) are relative to the original
    /// (pre-redaction) content of the individual leaf, not to the serialised
    /// JSON document. When a payload has multiple leaves each span is
    /// independent of the others.
    pub spans: Vec<RedactionSpan>,
}

// ---------------------------------------------------------------------------
// Pipeline
// ---------------------------------------------------------------------------

/// Default maximum JSON nesting depth. Applied when no explicit limit is set
/// via [`RedactionPipeline::with_max_depth`]. Chosen conservatively to prevent
/// stack exhaustion on pathological input while accommodating realistic payloads.
pub const DEFAULT_MAX_DEPTH: u32 = 128;

/// Stateless pipeline that redacts PII / secrets from [`ConnectorEvent`]
/// payloads before they cross the consent gate.
///
/// Construct with [`RedactionPipeline::new`] or via the [`Default`] impl;
/// use [`RedactionPipeline::with_max_depth`] to set a connector-manifest-
/// specified depth limit before calling [`RedactionPipeline::redact`].
#[derive(Debug)]
pub struct RedactionPipeline {
    /// Maximum JSON nesting depth before the redactor returns an error.
    max_depth: u32,
}

impl Default for RedactionPipeline {
    fn default() -> Self {
        Self {
            max_depth: DEFAULT_MAX_DEPTH,
        }
    }
}

impl RedactionPipeline {
    /// Create a new [`RedactionPipeline`] with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the maximum JSON nesting depth.
    ///
    /// If a JSON payload exceeds this depth, [`RedactionPipeline::redact`]
    /// returns [`ConnectorError::MalformedPayload`].  Use
    /// [`DEFAULT_MAX_DEPTH`] when the connector manifest does not declare an
    /// explicit limit.
    #[must_use]
    pub fn with_max_depth(mut self, max_depth: u32) -> Self {
        self.max_depth = max_depth;
        self
    }

    /// Redact PII and secrets from the event's payload and return the
    /// sanitised event together with the collected spans.
    ///
    /// - [`ConnectorPayload::Text`]: the body string is passed through
    ///   [`cairn_core::pipeline::filter::redact::redact`] once.
    /// - [`ConnectorPayload::Json`]: every string leaf of the JSON value is
    ///   walked with a bounded-depth iterator and redacted individually.
    ///   If the JSON exceeds [`Self::max_depth`] (set by
    ///   [`with_max_depth`][Self::with_max_depth]) the method returns
    ///   [`ConnectorError::MalformedPayload`].
    /// - [`ConnectorPayload::Binary`]: bytes reside in the spool; the
    ///   envelope carries only metadata and passes through unchanged with
    ///   zero spans.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectorError::MalformedPayload`] if a JSON payload
    /// exceeds the configured nesting depth limit.
    pub fn redact(&self, mut event: ConnectorEvent) -> Result<Redacted, ConnectorError> {
        let mut spans: Vec<RedactionSpan> = Vec::new();

        match &mut event.payload {
            ConnectorPayload::Text { body, .. } => {
                let result = redact::redact(body);
                spans.extend(result.spans);
                *body = result.text;
            }
            ConnectorPayload::Json { body, .. } => {
                walk_json_bounded(body, &mut spans, self.max_depth, 0)?;
            }
            ConnectorPayload::Binary { .. } => {
                // Bytes are spooled outside the envelope; nothing to redact here.
            }
        }

        Ok(Redacted { event, spans })
    }
}

// ---------------------------------------------------------------------------
// Depth-bounded JSON walker (module-level private function)
// ---------------------------------------------------------------------------

/// Walk `value` recursively, calling `redact` on every `String` leaf and
/// accumulating spans. Returns an error if the nesting depth exceeds
/// `max_depth`.
///
/// **Depth semantics:** `current_depth` is the number of container
/// (Object / Array) boundaries entered so far. The root call passes `0`.
/// Entering an Object or Array increments the depth *before* recursing into
/// its children and the incremented depth is checked against `max_depth`.
/// A plain string at depth 0 is always allowed; a string inside a flat
/// `{"k": "v"}` object is at depth 1; a string inside `{"a": {"b": "v"}}`
/// is at depth 2. So `max_depth = 1` allows a single-level flat object.
///
/// Using an explicit depth counter (rather than Rust stack depth) prevents
/// stack exhaustion on pathological input.
fn walk_json_bounded(
    value: &mut Value,
    spans: &mut Vec<RedactionSpan>,
    max_depth: u32,
    current_depth: u32,
) -> Result<(), ConnectorError> {
    match value {
        Value::String(s) => {
            let result = redact::redact(s);
            spans.extend(result.spans);
            *s = result.text;
        }
        Value::Array(items) => {
            let child_depth = current_depth + 1;
            if child_depth > max_depth {
                return Err(ConnectorError::MalformedPayload(format!(
                    "JSON nesting depth {child_depth} exceeds manifest limit {max_depth}",
                )));
            }
            for item in items.iter_mut() {
                walk_json_bounded(item, spans, max_depth, child_depth)?;
            }
        }
        Value::Object(map) => {
            let child_depth = current_depth + 1;
            if child_depth > max_depth {
                return Err(ConnectorError::MalformedPayload(format!(
                    "JSON nesting depth {child_depth} exceeds manifest limit {max_depth}",
                )));
            }
            for v in map.values_mut() {
                walk_json_bounded(v, spans, max_depth, child_depth)?;
            }
        }
        // Number, Bool, Null — no string content to redact.
        _ => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{
        ConnectorEventId, ConnectorPayload, ConnectorScope, DeliveryMode, SourceRef,
    };
    use std::collections::BTreeSet;

    /// Build a minimal [`ConnectorEvent`] with the given payload. All other
    /// fields are set to stable fixture values.
    fn evt(payload: ConnectorPayload) -> ConnectorEvent {
        ConnectorEvent {
            event_id: ConnectorEventId::new("01HX0000000000000000000000"),
            connector: "fixture".into(),
            source_ref: SourceRef::new("issue", "x", None),
            occurred_at: 0,
            labels: BTreeSet::new(),
            scope: ConnectorScope::project("p"),
            payload,
            delivery: DeliveryMode::Poll { cursor: None },
        }
    }

    #[test]
    fn email_in_text_is_redacted() {
        let pipeline = RedactionPipeline::new();
        let event = evt(ConnectorPayload::Text {
            mime: "text/plain".into(),
            body: "reach me at alice@example.com please".into(),
        });
        let out = pipeline.redact(event).unwrap();
        assert!(!out.spans.is_empty(), "must record at least one span");
        if let ConnectorPayload::Text { body, .. } = &out.event.payload {
            assert!(
                !body.contains("alice@example.com"),
                "email must be redacted from body"
            );
        } else {
            panic!("expected text payload");
        }
    }

    #[test]
    fn email_in_json_leaf_is_redacted() {
        let pipeline = RedactionPipeline::new();
        let event = evt(ConnectorPayload::Json {
            mime: "application/json".into(),
            body: serde_json::json!({"author": "alice@example.com", "body": "hi"}),
        });
        let out = pipeline.redact(event).unwrap();
        let json = serde_json::to_string(&out.event).unwrap();
        assert!(
            !json.contains("alice@example.com"),
            "email must be redacted from JSON leaf"
        );
        assert!(!out.spans.is_empty(), "must record at least one span");
    }

    #[test]
    fn binary_payload_passes_through_with_no_spans() {
        let pipeline = RedactionPipeline::new();
        let event = evt(ConnectorPayload::Binary {
            mime: "application/pdf".into(),
            sha256: "deadbeef".into(),
            bytes_ref: "/tmp/spool/abc".into(),
        });
        let original_event = event.clone();
        let out = pipeline.redact(event).unwrap();
        assert!(
            out.spans.is_empty(),
            "binary payload must produce zero spans"
        );
        assert_eq!(
            out.event, original_event,
            "binary event must be passed through unchanged"
        );
    }

    #[test]
    fn non_string_json_scalars_do_not_panic() {
        let pipeline = RedactionPipeline::new();
        let event = evt(ConnectorPayload::Json {
            mime: "application/json".into(),
            body: serde_json::json!({"count": 42, "active": true, "ratio": 0.5, "nothing": null}),
        });
        // Should complete without panic and produce no spans (no strings to scan).
        let out = pipeline.redact(event).unwrap();
        assert!(
            out.spans.is_empty(),
            "non-string scalars must not produce spans"
        );
    }
}
