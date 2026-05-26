//! `GhError` — adapter-internal error surface.
//!
//! Converts into [`cairn_connectors_core::ConnectorError`] at the substrate
//! boundary (`Connector::poll` / `Connector::ingest_webhook`).

use std::time::Duration;

use cairn_connectors_core::ConnectorError;
use thiserror::Error;

/// Adapter-internal error type. Mapped to [`ConnectorError`] at substrate boundaries.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GhError {
    /// 401 / 403 from GitHub.
    #[error("github auth failure (status {status})")]
    Auth {
        /// HTTP status code returned by GitHub.
        status: u16,
    },

    /// 429 with optional `Retry-After`.
    #[error("github rate limited; retry after {}s", retry_after.as_secs())]
    RateLimited {
        /// Minimum delay before next request.
        retry_after: Duration,
    },

    /// Catch-all transient error.
    ///
    /// Carries a free-form message rather than a `#[source]` chain — callers
    /// must encode any upstream cause as part of the string. Maps to
    /// `ConnectorError::transient_msg` at the substrate boundary. Used for
    /// 5xx responses and as the wildcard sink in the `BadRequest`/`Http`/`Jwt`/`Json`
    /// fallthrough of `From<GhError> for ConnectorError`.
    #[error("github transient: {0}")]
    Transient(String),

    /// 4xx other than 401/403/429.
    #[error("github bad request (status {status}): {message}")]
    BadRequest {
        /// HTTP status.
        status: u16,
        /// GitHub's `message` field, if present.
        message: String,
    },

    /// JSON deserialization failure on response or webhook body.
    #[error("github malformed payload: {0}")]
    Malformed(String),

    /// Underlying `reqwest` failure (network, TLS, etc.).
    #[error("github http error: {0}")]
    Http(#[from] reqwest::Error),

    /// JWT minting failure (App auth path).
    #[error("github jwt error: {0}")]
    Jwt(#[from] jsonwebtoken::errors::Error),

    /// JSON serde failure on cursor or webhook body.
    #[error("github json error: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<GhError> for ConnectorError {
    fn from(e: GhError) -> Self {
        match e {
            GhError::Auth { .. } => ConnectorError::AuthExpired {
                scope: "github".into(),
            },
            GhError::RateLimited { retry_after } => ConnectorError::RateLimited { retry_after },
            GhError::Malformed(m) => ConnectorError::MalformedPayload(m),
            GhError::Transient(m) => ConnectorError::transient_msg(m),
            other => ConnectorError::transient_msg(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_maps_to_auth_expired() {
        let e: ConnectorError = GhError::Auth { status: 401 }.into();
        assert!(matches!(e, ConnectorError::AuthExpired { .. }));
    }

    #[test]
    fn rate_limited_maps_to_rate_limited() {
        let e: ConnectorError = GhError::RateLimited {
            retry_after: Duration::from_secs(30),
        }
        .into();
        match e {
            ConnectorError::RateLimited { retry_after } => {
                assert_eq!(retry_after, Duration::from_secs(30));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn malformed_maps_to_malformed_payload() {
        let e: ConnectorError = GhError::Malformed("bad field `id`".into()).into();
        assert!(matches!(e, ConnectorError::MalformedPayload(s) if s.contains("bad field `id`")));
    }

    #[test]
    fn bad_request_falls_through_to_transient() {
        let e: ConnectorError = GhError::BadRequest {
            status: 422,
            message: "validation failed".into(),
        }
        .into();
        // The wildcard arm wraps via transient_msg, which produces Transient.
        match e {
            ConnectorError::Transient(_) => {}
            other => panic!("expected Transient, got {other:?}"),
        }
    }
}
