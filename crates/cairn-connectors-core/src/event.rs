//! `ConnectorEvent` envelope and supporting types (issue #130, brief §9.1).
//!
//! Connectors emit [`ConnectorEvent`]s; the framework converts them into
//! `CaptureEvent`s after validation + redaction + consent. The two are
//! intentionally distinct so adapter crates never see `CaptureEvent`.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::error::ConnectorError;
use crate::manifest::ConnectorManifest;

/// ULID identifying one envelope. Minted by the connector; must be globally
/// unique and monotonically ordered within a single connector instance.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConnectorEventId(String);

impl ConnectorEventId {
    /// Wrap a string value as a [`ConnectorEventId`] without validation.
    ///
    /// This constructor accepts arbitrary strings and is intended for internal
    /// use (e.g. deserialization from trusted JSON). Callers that will
    /// interpolate the `event_id` into a filesystem path **must** use
    /// [`ConnectorEventId::parse`] instead to prevent path-traversal attacks.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Parse and validate a ULID string, returning a [`ConnectorEventId`].
    ///
    /// Accepts any string that is a valid 26-character Crockford base32 ULID
    /// (case-insensitive, as defined by the ULID specification). Rejects:
    ///
    /// - Strings that are not valid ULIDs (wrong length, illegal characters,
    ///   overflow of the timestamp field, etc.).
    /// - Strings containing `/`, `\`, `..`, or null bytes (defense in depth
    ///   against path-traversal even if the ULID check is somehow bypassed).
    ///
    /// # Errors
    ///
    /// Returns [`ConnectorError::MalformedPayload`] with the offending value
    /// included in the message.
    ///
    /// # Usage
    ///
    /// Call this at every entry-point that will later write the `event_id` into
    /// a filesystem path (e.g. the spool helper). Use [`Self::new`] only when
    /// the source is already trusted (deserialization from a controlled
    /// datastore, internal construction in tests).
    pub fn parse(value: impl Into<String>) -> Result<Self, ConnectorError> {
        let raw: String = value.into();
        // Defense-in-depth: reject obvious path-traversal characters before
        // delegating to the ULID parser. This catches inputs like
        // "../../etc/passwd" or "foo\0bar" that the ULID parser would also
        // reject but whose intent is specifically adversarial.
        if raw.contains('/') || raw.contains('\\') || raw.contains("..") || raw.contains('\0') {
            return Err(ConnectorError::MalformedPayload(format!(
                "invalid event_id: {raw:?} (contains path-traversal characters)"
            )));
        }
        // Primary check: must be a valid ULID.
        ulid::Ulid::from_string(&raw).map_err(|_| {
            ConnectorError::MalformedPayload(format!(
                "invalid event_id: {raw:?} (must be a 26-char Crockford base32 ULID)"
            ))
        })?;
        Ok(Self(raw))
    }

    /// Borrow the inner string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ConnectorEventId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Stable reference to the upstream object that produced this event.
///
/// `kind` names the upstream entity type (`issue`, `pr`, `message`, `page`,
/// `file`); `system_id` is the upstream system's stable identifier for that
/// entity; `sub_id` optionally further scopes within a compound entity (e.g.
/// a specific comment inside an issue).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct SourceRef {
    /// Upstream entity type (e.g. `"issue"`, `"pr"`, `"message"`).
    pub kind: String,
    /// Upstream stable identifier for the entity (e.g. `"gh:owner/repo#42"`).
    pub system_id: String,
    /// Optional sub-identifier within a compound entity (e.g. a comment id).
    pub sub_id: Option<String>,
}

impl SourceRef {
    /// Construct a new [`SourceRef`].
    #[must_use]
    pub fn new(
        kind: impl Into<String>,
        system_id: impl Into<String>,
        sub_id: Option<String>,
    ) -> Self {
        Self {
            kind: kind.into(),
            system_id: system_id.into(),
            sub_id,
        }
    }
}

/// Scope key the manifest matches against.
///
/// `kind` names a pattern family (`project`, `channel`, `workspace`, `path`);
/// `value` is the concrete instance within that family.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ConnectorScope {
    /// Scope family (e.g. `"project"`, `"channel"`, `"workspace"`).
    pub kind: String,
    /// Concrete value within the scope family (e.g. `"owner/repo"`).
    pub value: String,
}

impl ConnectorScope {
    /// Construct a scope of kind `"project"`.
    #[must_use]
    pub fn project(value: impl Into<String>) -> Self {
        Self {
            kind: "project".into(),
            value: value.into(),
        }
    }

    /// Construct an arbitrary scope by kind + value. Use this in external
    /// test code where struct literals are blocked by `#[non_exhaustive]`.
    #[must_use]
    pub fn new(kind: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            value: value.into(),
        }
    }

    /// Return `"<kind>:<value>"` — the form
    /// [`ConnectorConsentJournal::lookup`][crate] accepts as `scope_key`.
    #[must_use]
    pub fn lookup_key(&self) -> String {
        format!("{}:{}", self.kind, self.value)
    }
}

/// Mode through which this event was delivered to the framework.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum DeliveryMode {
    /// Event arrived via periodic polling. `cursor` is the pagination cursor
    /// returned by the previous poll call; `None` on the first call.
    Poll {
        /// Opaque cursor string for the next page, or `None` for initial poll.
        cursor: Option<String>,
    },
    /// Event arrived via an inbound webhook. `signature_id` identifies which
    /// signing key was used to verify the request.
    Webhook {
        /// Identifier of the signing-key used to verify this webhook delivery.
        signature_id: String,
    },
}

/// Payload variant — kept closed so the redactor knows how to walk it.
///
/// `#[non_exhaustive]` because additional variants (e.g. `Multipart`) may be
/// added in minor releases; downstream `match` arms must include a wildcard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum ConnectorPayload {
    /// Structured JSON payload. The `body` is a raw [`serde_json::Value`];
    /// the redactor walks it before the event crosses the consent gate.
    Json {
        /// MIME type of the payload (typically `"application/json"`).
        mime: String,
        /// Raw JSON body as received from the upstream system.
        body: serde_json::Value,
    },
    /// Plain-text payload (e.g. Markdown, plain email body).
    Text {
        /// MIME type of the payload (e.g. `"text/plain"`, `"text/markdown"`).
        mime: String,
        /// Raw text body.
        body: String,
    },
    /// Binary / opaque payload. The bytes are spooled to a temporary path;
    /// `bytes_ref` holds the spool path or a content-addressed key.
    ///
    /// `bytes_len` is the adapter's declaration of the byte length of the
    /// payload referenced by `bytes_ref`. The framework trusts this value for
    /// budget and size-gate enforcement. The spool layer (#131) will verify it
    /// against the on-disk content and reject mismatches before persisting.
    Binary {
        /// MIME type of the payload (e.g. `"application/pdf"`).
        mime: String,
        /// Hex-encoded SHA-256 of the raw bytes (integrity check).
        sha256: String,
        /// Spool path or content-addressed storage key for the bytes.
        bytes_ref: String,
        /// Declared byte length of the content at `bytes_ref`.
        ///
        /// Used by the framework for `payload.max_bytes` enforcement and
        /// per-scope byte-budget gating. The spool layer (#131) will
        /// cross-check this value against the actual spooled file size.
        ///
        /// TODO(#130-followup): wire `max_bytes_per_day` manifest budget
        /// using this field once the byte-budget tracker is implemented.
        bytes_len: u64,
    },
}

impl ConnectorPayload {
    /// Return the MIME type string for this payload variant.
    #[must_use]
    pub fn mime(&self) -> &str {
        match self {
            Self::Json { mime, .. } | Self::Text { mime, .. } | Self::Binary { mime, .. } => mime,
        }
    }

    /// Return an approximate size in bytes. For `Json`, this is the length of
    /// the serialized JSON string; for `Text`, the UTF-8 byte length; for
    /// `Binary`, the adapter-declared `bytes_len` (trusted by the framework;
    /// verified by the spool layer in #131).
    #[must_use]
    pub fn size_hint(&self) -> usize {
        match self {
            Self::Json { body, .. } => body.to_string().len(),
            Self::Text { body, .. } => body.len(),
            Self::Binary { bytes_len, .. } => {
                // Saturate at usize::MAX on 32-bit targets rather than
                // wrapping. Practical payloads are always well within usize.
                usize::try_from(*bytes_len).unwrap_or(usize::MAX)
            }
        }
    }
}

/// Unified envelope emitted by every connector.
///
/// The framework accepts one `ConnectorEvent` at a time, validates it against
/// the manifest, runs redaction, checks consent, then converts it to a
/// `CaptureEvent` for the core pipeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ConnectorEvent {
    /// ULID uniquely identifying this envelope, minted by the connector.
    pub event_id: ConnectorEventId,
    /// Logical name of the connector that produced this event.
    pub connector: String,
    /// Stable upstream reference for the originating object.
    pub source_ref: SourceRef,
    /// Unix timestamp (seconds) when the event occurred in the upstream system.
    pub occurred_at: i64,
    /// Taxonomy labels to be attached to the resulting `CaptureEvent`.
    /// Every label must appear in the connector's manifest allow-list.
    pub labels: BTreeSet<String>,
    /// Scope key used to match against manifest scope patterns and consent grants.
    pub scope: ConnectorScope,
    /// The actual payload: JSON, text, or a spooled binary.
    pub payload: ConnectorPayload,
    /// How this event was delivered (poll cursor vs. webhook).
    pub delivery: DeliveryMode,
}

impl ConnectorEvent {
    /// Validate this event against the connector's manifest.
    ///
    /// Checks performed (in order):
    ///
    /// 1. `event.connector == manifest.name()` — name integrity.
    /// 2. `event.scope` matches a declared scope pattern.
    /// 3. `event.payload.mime()` is in the manifest's `webhook.allowed_mimes`.
    /// 4. `event.payload.size_hint()` is within `manifest.payload.max_bytes_parsed`.
    ///
    /// Label checks are performed earlier in `process_event` (against both the
    /// grant and the manifest), so they are not repeated here.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectorError::Fatal`] for name mismatch (indicates a
    /// programming error), [`ConnectorError::MalformedPayload`] for scope /
    /// MIME / size violations.
    pub fn validate_against_manifest(
        &self,
        manifest: &ConnectorManifest,
    ) -> Result<(), ConnectorError> {
        // 1. Connector-name integrity.
        if self.connector != manifest.name() {
            return Err(ConnectorError::fatal_msg(format!(
                "event connector \"{}\" does not match manifest name \"{}\"",
                self.connector,
                manifest.name()
            )));
        }

        // 2. Scope must match a declared pattern.
        if !manifest.scope_matches(&self.scope.kind, &self.scope.value) {
            return Err(ConnectorError::MalformedPayload(format!(
                "scope \"{}:{}\" does not match any declared scope pattern",
                self.scope.kind, self.scope.value,
            )));
        }

        // 3. MIME type must be in the allowed list.
        let mime = self.payload.mime();
        if !manifest.allowed_mime(mime) {
            return Err(ConnectorError::MalformedPayload(format!(
                "payload MIME type \"{mime}\" is not in manifest allowed_mimes",
            )));
        }

        // 4. Payload size must be within the limit.
        let size = self.payload.size_hint() as u64;
        let max = manifest.payload.max_bytes_parsed;
        if size > max {
            return Err(ConnectorError::MalformedPayload(format!(
                "payload size {size} bytes exceeds manifest limit {max} bytes",
            )));
        }

        Ok(())
    }

    /// Construct a new [`ConnectorEvent`].
    ///
    /// Use this constructor in external code where `#[non_exhaustive]` blocks
    /// struct literals. Internal and same-crate code may use struct literal
    /// syntax directly.
    #[must_use]
    #[allow(clippy::too_many_arguments)] // all 8 fields are required; no optional args
    pub fn new(
        event_id: ConnectorEventId,
        connector: impl Into<String>,
        source_ref: SourceRef,
        occurred_at: i64,
        labels: BTreeSet<String>,
        scope: ConnectorScope,
        payload: ConnectorPayload,
        delivery: DeliveryMode,
    ) -> Self {
        Self {
            event_id,
            connector: connector.into(),
            source_ref,
            occurred_at,
            labels,
            scope,
            payload,
            delivery,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn json_payload_round_trips() {
        let event = ConnectorEvent {
            event_id: ConnectorEventId::new("01HX0000000000000000000000"),
            connector: "fixture".into(),
            source_ref: SourceRef::new("issue", "gh:owner/repo#42", None),
            occurred_at: 1_700_000_000,
            labels: BTreeSet::from(["note".to_string()]),
            scope: ConnectorScope::project("owner/repo"),
            payload: ConnectorPayload::Json {
                mime: "application/json".into(),
                body: serde_json::json!({"text": "hello"}),
            },
            delivery: DeliveryMode::Poll {
                cursor: Some("c1".into()),
            },
        };
        let s = serde_json::to_string(&event).unwrap();
        let back: ConnectorEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(event.connector, back.connector);
        assert_eq!(event.labels, back.labels);
    }

    #[test]
    fn rejects_unknown_fields() {
        let bogus = r#"{
          "event_id":"01HX0000000000000000000000","connector":"f","source_ref":{"kind":"x","system_id":"y","sub_id":null},
          "occurred_at":0,"labels":[],"scope":{"kind":"project","value":"x"},
          "payload":{"kind":"text","mime":"text/plain","body":""},
          "delivery":{"kind":"poll","cursor":null},
          "extra":true
        }"#;
        assert!(serde_json::from_str::<ConnectorEvent>(bogus).is_err());
    }

    #[test]
    fn mime_helper_returns_correct_value() {
        assert_eq!(
            ConnectorPayload::Text {
                mime: "text/plain".into(),
                body: "hi".into()
            }
            .mime(),
            "text/plain"
        );
        assert_eq!(
            ConnectorPayload::Binary {
                mime: "application/pdf".into(),
                sha256: "abc".into(),
                bytes_ref: "/tmp/x".into(),
                bytes_len: 0,
            }
            .mime(),
            "application/pdf"
        );
    }

    /// `size_hint` for `Binary` must return the adapter-declared `bytes_len`,
    /// not `0`. This is the fix for Finding B (Round-2 review).
    #[test]
    fn size_hint_binary_returns_bytes_len() {
        assert_eq!(
            ConnectorPayload::Binary {
                mime: "application/octet-stream".into(),
                sha256: "abc".into(),
                bytes_ref: "/tmp/x".into(),
                bytes_len: 8192,
            }
            .size_hint(),
            8192,
        );
        // Zero declared length still works.
        assert_eq!(
            ConnectorPayload::Binary {
                mime: "application/octet-stream".into(),
                sha256: "abc".into(),
                bytes_ref: "/tmp/x".into(),
                bytes_len: 0,
            }
            .size_hint(),
            0,
        );
    }

    #[test]
    fn connector_scope_lookup_key() {
        let scope = ConnectorScope::project("owner/repo");
        assert_eq!(scope.lookup_key(), "project:owner/repo");

        let channel = ConnectorScope::new("channel", "#general");
        assert_eq!(channel.lookup_key(), "channel:#general");
    }

    #[test]
    fn connector_event_id_display() {
        let id = ConnectorEventId::new("01HX0000000000000000000000");
        assert_eq!(id.to_string(), "01HX0000000000000000000000");
        assert_eq!(id.as_str(), "01HX0000000000000000000000");
    }

    #[test]
    fn delivery_mode_webhook_round_trips() {
        let d = DeliveryMode::Webhook {
            signature_id: "key-1".into(),
        };
        let s = serde_json::to_string(&d).unwrap();
        let back: DeliveryMode = serde_json::from_str(&s).unwrap();
        assert_eq!(d, back);
    }
}
