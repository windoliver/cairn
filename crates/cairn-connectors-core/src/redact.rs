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

/// Stateless pipeline that redacts PII / secrets from [`ConnectorEvent`]
/// payloads before they cross the consent gate.
///
/// Construct with [`RedactionPipeline::new`] or via the [`Default`] impl;
/// configuration slots are reserved for future extension (e.g. custom
/// detector sets, allow-lists) without breaking callers.
#[derive(Debug, Default)]
pub struct RedactionPipeline {
    // Reserved for future configuration (custom detectors, allow-lists, etc.).
    // Using a zero-size struct today keeps the `::new()` API stable.
    _reserved: (),
}

impl RedactionPipeline {
    /// Create a new [`RedactionPipeline`] with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self { _reserved: () }
    }

    /// Redact PII and secrets from the event's payload and return the
    /// sanitised event together with the collected spans.
    ///
    /// - [`ConnectorPayload::Text`]: the body string is passed through
    ///   [`cairn_core::pipeline::filter::redact::redact`] once.
    /// - [`ConnectorPayload::Json`]: every string leaf of the JSON value is
    ///   walked recursively and redacted individually.
    /// - [`ConnectorPayload::Binary`]: bytes reside in the spool; the
    ///   envelope carries only metadata and passes through unchanged with
    ///   zero spans.
    ///
    /// This function is infallible at P0 (the underlying redact function
    /// never fails). The `Result` wrapper is kept so future variants can
    /// return errors without changing the signature.
    pub fn redact(&self, mut event: ConnectorEvent) -> Result<Redacted, ConnectorError> {
        let mut spans: Vec<RedactionSpan> = Vec::new();

        match &mut event.payload {
            ConnectorPayload::Text { body, .. } => {
                let result = redact::redact(body);
                spans.extend(result.spans);
                *body = result.text;
            }
            ConnectorPayload::Json { body, .. } => {
                Self::walk_json(body, &mut spans);
            }
            ConnectorPayload::Binary { .. } => {
                // Bytes are spooled outside the envelope; nothing to redact here.
            }
        }

        Ok(Redacted { event, spans })
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Recursively walk a [`serde_json::Value`], calling
    /// [`cairn_core::pipeline::filter::redact::redact`] on every `String`
    /// leaf and accumulating spans. Non-string scalars (`Number`, `Bool`,
    /// `Null`) are no-ops.
    fn walk_json(value: &mut Value, spans: &mut Vec<RedactionSpan>) {
        match value {
            Value::String(s) => {
                let result = redact::redact(s);
                spans.extend(result.spans);
                *s = result.text;
            }
            Value::Array(items) => {
                for item in items.iter_mut() {
                    Self::walk_json(item, spans);
                }
            }
            Value::Object(map) => {
                for v in map.values_mut() {
                    Self::walk_json(v, spans);
                }
            }
            // Number, Bool, Null — no string content to redact.
            _ => {}
        }
    }
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
