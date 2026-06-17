//! `WebClipError` — adapter-local parse errors, mapped to `ConnectorError`.

use cairn_connectors_core::ConnectorError;

/// Errors produced while parsing a web-clip webhook request into a
/// [`cairn_connectors_core::ConnectorEvent`].
///
/// Every variant maps to [`ConnectorError::MalformedPayload`] so the substrate
/// webhook handler returns `400 Bad Request`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WebClipError {
    /// The request carried no `Content-Type` header.
    #[error("missing Content-Type header")]
    MissingContentType,
    /// The `Content-Type` is not an accepted clip media type.
    #[error("unsupported Content-Type: {0}")]
    UnsupportedContentType(String),
    /// A required field was absent (url, `captured_at`, or clip body).
    #[error("missing required field: {0}")]
    MissingField(&'static str),
    /// The clip URL could not be parsed or had no host component.
    #[error("invalid clip url: {0}")]
    BadUrl(String),
    /// `captured_at` was not a valid Unix-seconds integer.
    #[error("invalid captured_at: {0}")]
    BadCapturedAt(String),
    /// The JSON envelope failed to deserialize.
    #[error("invalid json clip envelope: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<WebClipError> for ConnectorError {
    fn from(err: WebClipError) -> Self {
        ConnectorError::MalformedPayload(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_to_malformed_payload() {
        let e: ConnectorError = WebClipError::BadUrl("file:///x".into()).into();
        assert!(matches!(e, ConnectorError::MalformedPayload(_)));
        assert_eq!(
            e.to_string(),
            "malformed payload: invalid clip url: file:///x"
        );
    }

    #[test]
    fn json_error_converts_via_from() {
        let json_err = serde_json::from_str::<serde_json::Value>("{bad").unwrap_err();
        let e: WebClipError = json_err.into();
        assert!(matches!(e, WebClipError::Json(_)));
    }
}
