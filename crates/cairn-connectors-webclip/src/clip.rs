//! Parse a verified web-clip webhook request into a `ConnectorEvent`.
//!
//! Content negotiation on `Content-Type`:
//! - `application/json` -> structured [`ClipEnvelope`] -> `ConnectorPayload::Json`.
//! - `text/markdown` | `text/plain` -> raw body + `X-Cairn-Clip-*` headers ->
//!   `ConnectorPayload::Text`.

use std::collections::BTreeSet;

use cairn_connectors_core::{
    ConnectorEvent, ConnectorPayload, ConnectorScope, DeliveryMode, SourceRef, WebhookRequest,
};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::WebClipError;
use crate::event_id;

/// Connector name — must match the manifest `[connector] name`.
const CONNECTOR_NAME: &str = "webclip";

/// Structured clip envelope accepted in `application/json` mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClipEnvelope {
    /// Source page URL. Required; its host becomes the consent scope.
    pub url: String,
    /// Optional page title (rides in the payload for downstream use).
    #[serde(default)]
    pub title: Option<String>,
    /// Capture time, Unix seconds. Required.
    pub captured_at: i64,
    /// Selected text/HTML, if the user clipped a selection.
    #[serde(default)]
    pub selection: Option<String>,
    /// Markdown form of the clip body.
    #[serde(default)]
    pub markdown: Option<String>,
    /// Optional free-form user note.
    #[serde(default)]
    pub note: Option<String>,
    /// Optional user tags. Preserved in the payload; NOT promoted to labels.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Accepted clip media types after `Content-Type` normalization.
enum Media {
    /// `application/json` structured envelope.
    Json,
    /// `text/markdown` or `text/plain`; the carried `String` is the concrete
    /// MIME echoed into `ConnectorPayload::Text`.
    Text(String),
}

/// Resolve and normalize the request `Content-Type` into a [`Media`].
fn media_type(req: &WebhookRequest) -> Result<Media, WebClipError> {
    let raw = req
        .header("Content-Type")
        .ok_or(WebClipError::MissingContentType)?;
    // Strip parameters (`; charset=utf-8`) and normalize case/whitespace.
    let base = raw
        .split(';')
        .next()
        .unwrap_or(raw)
        .trim()
        .to_ascii_lowercase();
    match base.as_str() {
        "application/json" => Ok(Media::Json),
        "text/markdown" | "text/plain" => Ok(Media::Text(base)),
        other => Err(WebClipError::UnsupportedContentType(other.to_owned())),
    }
}

/// Read a required header, erroring with [`WebClipError::MissingField`] if absent.
fn header_required(req: &WebhookRequest, name: &'static str) -> Result<String, WebClipError> {
    req.header(name)
        .map(str::to_owned)
        .ok_or(WebClipError::MissingField(name))
}

/// Parse a `captured_at` string as Unix seconds.
fn parse_captured_at(s: &str) -> Result<i64, WebClipError> {
    s.trim()
        .parse::<i64>()
        .map_err(|_| WebClipError::BadCapturedAt(s.to_owned()))
}

/// Parse a verified webhook request into exactly one [`ConnectorEvent`].
///
/// The substrate has already verified the HMAC signature; `signature_id` is the
/// verified signature header value, recorded in `DeliveryMode::Webhook`.
pub(crate) fn parse_request(
    req: &WebhookRequest,
    signature_id: &str,
) -> Result<ConnectorEvent, WebClipError> {
    let (url, captured_at, payload, hash_input) = match media_type(req)? {
        Media::Json => {
            let env: ClipEnvelope = serde_json::from_slice(&req.body)?;
            if env.selection.is_none() && env.markdown.is_none() {
                return Err(WebClipError::MissingField("selection|markdown"));
            }
            let body = serde_json::to_value(&env)?;
            let hash_input = serde_json::to_vec(&body)?;
            (
                env.url.clone(),
                env.captured_at,
                ConnectorPayload::Json {
                    mime: "application/json".to_owned(),
                    body,
                },
                hash_input,
            )
        }
        Media::Text(mime) => {
            let url = header_required(req, "X-Cairn-Clip-Url")?;
            let captured_at =
                parse_captured_at(&header_required(req, "X-Cairn-Clip-Captured-At")?)?;
            let text = String::from_utf8_lossy(&req.body).into_owned();
            let hash_input = req.body.clone();
            (
                url,
                captured_at,
                ConnectorPayload::Text { mime, body: text },
                hash_input,
            )
        }
    };

    // Per-domain consent scope from the URL host.
    let host = Url::parse(&url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .ok_or_else(|| WebClipError::BadUrl(url.clone()))?;

    let captured_str = captured_at.to_string();
    let hash = event_id::payload_hash(&hash_input);
    let event_id = event_id::from_parts("clip", &url, &[&captured_str, &hash]);

    let mut labels = BTreeSet::new();
    labels.insert("source:web".to_owned());
    labels.insert("kind:clip".to_owned());

    Ok(ConnectorEvent::new(
        event_id,
        CONNECTOR_NAME,
        SourceRef::new("clip", url, None),
        captured_at,
        labels,
        ConnectorScope::new("domain", host),
        payload,
        DeliveryMode::Webhook {
            signature_id: signature_id.to_owned(),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(headers: Vec<(&str, &str)>, body: &[u8]) -> WebhookRequest {
        WebhookRequest {
            connector: "webclip".into(),
            body: body.to_vec(),
            headers: headers
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
        }
    }

    #[test]
    fn json_clip_builds_scoped_event() {
        let body =
            br#"{"url":"https://en.wikipedia.org/wiki/Cairn","captured_at":100,"markdown":"hi"}"#;
        let r = req(vec![("Content-Type", "application/json")], body);
        let ev = parse_request(&r, "sig").expect("parses");
        assert_eq!(ev.connector, "webclip");
        assert_eq!(ev.scope.kind, "domain");
        assert_eq!(ev.scope.value, "en.wikipedia.org");
        assert_eq!(ev.source_ref.kind, "clip");
        assert!(ev.labels.contains("source:web") && ev.labels.contains("kind:clip"));
        assert!(matches!(ev.payload, ConnectorPayload::Json { .. }));
    }

    #[test]
    fn json_charset_param_is_accepted() {
        let body = br#"{"url":"https://e.com/a","captured_at":1,"selection":"x"}"#;
        let r = req(
            vec![("Content-Type", "application/json; charset=utf-8")],
            body,
        );
        assert!(parse_request(&r, "sig").is_ok());
    }

    #[test]
    fn text_markdown_uses_headers_for_metadata() {
        let r = req(
            vec![
                ("Content-Type", "text/markdown"),
                ("X-Cairn-Clip-Url", "https://example.com/post/42"),
                ("X-Cairn-Clip-Captured-At", "1748563200"),
            ],
            b"## Heading\nbody",
        );
        let ev = parse_request(&r, "sig").expect("parses");
        assert_eq!(ev.scope.value, "example.com");
        assert_eq!(ev.occurred_at, 1_748_563_200);
        assert!(matches!(ev.payload, ConnectorPayload::Text { .. }));
    }

    #[test]
    fn missing_content_type_is_error() {
        let r = req(vec![], b"{}");
        assert!(matches!(
            parse_request(&r, "s"),
            Err(WebClipError::MissingContentType)
        ));
    }

    #[test]
    fn unsupported_content_type_is_error() {
        let r = req(vec![("Content-Type", "text/html")], b"<p>x</p>");
        assert!(matches!(
            parse_request(&r, "s"),
            Err(WebClipError::UnsupportedContentType(_))
        ));
    }

    #[test]
    fn json_without_body_field_is_error() {
        let body = br#"{"url":"https://e.com/a","captured_at":1}"#;
        let r = req(vec![("Content-Type", "application/json")], body);
        assert!(matches!(
            parse_request(&r, "s"),
            Err(WebClipError::MissingField(_))
        ));
    }

    #[test]
    fn hostless_url_is_error() {
        let body = br#"{"url":"file:///etc/passwd","captured_at":1,"markdown":"x"}"#;
        let r = req(vec![("Content-Type", "application/json")], body);
        assert!(matches!(
            parse_request(&r, "s"),
            Err(WebClipError::BadUrl(_))
        ));
    }

    #[test]
    fn text_missing_url_header_is_error() {
        let r = req(
            vec![
                ("Content-Type", "text/plain"),
                ("X-Cairn-Clip-Captured-At", "1"),
            ],
            b"body",
        );
        assert!(matches!(
            parse_request(&r, "s"),
            Err(WebClipError::MissingField(_))
        ));
    }
}
