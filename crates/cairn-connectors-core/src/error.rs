//! `ConnectorError` — the closed error surface every framework gate returns.
//!
//! Adapters may wrap their own errors as [`ConnectorError::Transient`] or
//! [`ConnectorError::Fatal`]; everything else is named so the registry can
//! act on it without string-matching.
//!
//! # Why `Box<dyn Error>` instead of `anyhow::Error`
//!
//! `cairn-connectors-core` is a library crate. CLAUDE.md §6.2 mandates
//! `thiserror` (not `anyhow`) for library error types so that downstream
//! callers get a stable, typed error surface and can inspect source chains
//! without depending on `anyhow`'s opaque internals. `Box<dyn Error + Send +
//! Sync + 'static>` preserves the full source chain (accessible via
//! `std::error::Error::source()`) while keeping the crate `anyhow`-free.
//! `std` implements `From<String> for Box<dyn Error + Send + Sync>`, so the
//! `transient_msg` / `fatal_msg` helpers work without any extra dependencies.

use std::time::Duration;

/// Errors emitted by `cairn-connectors-core`. `#[non_exhaustive]` so new
/// variants can be added in minor releases without breaking match arms in
/// adapter crates.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConnectorError {
    /// OAuth credentials for the named scope have expired and must be
    /// refreshed before the connector can poll or receive webhooks.
    #[error("auth expired for {scope}")]
    AuthExpired {
        /// The OAuth scope whose token has expired.
        scope: String,
    },

    /// The upstream system returned a rate-limit response. The caller should
    /// back off for at least `retry_after`.
    #[error("rate limited; retry after {}s", retry_after.as_secs())]
    RateLimited {
        /// Minimum duration to wait before retrying.
        retry_after: Duration,
    },

    /// Webhook payload signature verification failed. The request should be
    /// rejected silently (do not reveal *why* to the caller).
    #[error("signature mismatch")]
    SignatureMismatch,

    /// The connector emitted a payload that violates the manifest's
    /// structural rules (wrong MIME type, missing required field, etc.).
    #[error("malformed payload: {0}")]
    MalformedPayload(String),

    /// The connector's per-scope request budget (as declared in its
    /// manifest) has been fully consumed for the current window.
    #[error("budget exceeded for {scope}")]
    BudgetExceeded {
        /// The scope whose budget is exhausted.
        scope: String,
    },

    /// The connector tried to emit an event tagged with a label that is not
    /// listed in its manifest's `labels` allow-list.
    #[error("undeclared label {label}")]
    UndeclaredLabel {
        /// The label that was not declared in the manifest.
        label: String,
    },

    /// No live consent grant exists for the `(connector, scope)` pair.
    /// The connector must not emit events until the user re-grants consent.
    #[error("consent revoked for {connector}")]
    ConsentRevoked {
        /// The connector whose consent has been revoked.
        connector: String,
    },

    /// A recoverable upstream or transport error. The registry will
    /// schedule a retry according to the connector's backoff policy.
    #[error("transient: {0}")]
    Transient(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),

    /// A non-recoverable error; the registry will disable the connector
    /// and surface this in `lint` output.
    #[error("fatal: {0}")]
    Fatal(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),
}

impl ConnectorError {
    /// Wrap a free-form transient error message as [`ConnectorError::Transient`].
    ///
    /// Prefer a more specific variant when one fits (e.g., [`ConnectorError::RateLimited`]
    /// for 429 responses). This helper is for adapter-internal errors that do
    /// not map to a named variant and are considered retryable.
    pub fn transient_msg(msg: impl Into<String>) -> Self {
        let s: String = msg.into();
        Self::Transient(s.into())
    }

    /// Wrap a free-form fatal error message as [`ConnectorError::Fatal`].
    ///
    /// Use when the connector encounters an unrecoverable situation (e.g.,
    /// duplicate connector registration, schema incompatibility). The registry
    /// will disable the connector on receipt of this variant.
    pub fn fatal_msg(msg: impl Into<String>) -> Self {
        let s: String = msg.into();
        Self::Fatal(s.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn display_strings_are_stable() {
        assert_eq!(
            ConnectorError::RateLimited {
                retry_after: Duration::from_secs(7)
            }
            .to_string(),
            "rate limited; retry after 7s",
        );
        assert_eq!(
            ConnectorError::UndeclaredLabel {
                label: "secret".into()
            }
            .to_string(),
            "undeclared label secret",
        );
        assert_eq!(
            ConnectorError::ConsentRevoked {
                connector: "fixture".into()
            }
            .to_string(),
            "consent revoked for fixture",
        );
    }

    #[test]
    fn dyn_compat() {
        let e: Box<dyn std::error::Error + Send + Sync> =
            Box::new(ConnectorError::SignatureMismatch);
        assert!(e.to_string().contains("signature"));
    }

    #[test]
    fn transient_msg_helper_produces_transient_variant() {
        let e = ConnectorError::transient_msg("upstream timeout");
        assert!(e.to_string().contains("transient"));
        assert!(e.to_string().contains("upstream timeout"));
    }

    #[test]
    fn fatal_msg_helper_produces_fatal_variant() {
        let e = ConnectorError::fatal_msg("duplicate connector");
        assert!(e.to_string().contains("fatal"));
        assert!(e.to_string().contains("duplicate connector"));
    }

    #[test]
    fn auth_expired_display() {
        let e = ConnectorError::AuthExpired {
            scope: "calendar:read".into(),
        };
        assert_eq!(e.to_string(), "auth expired for calendar:read");
    }

    #[test]
    fn budget_exceeded_display() {
        let e = ConnectorError::BudgetExceeded {
            scope: "github:issues".into(),
        };
        assert_eq!(e.to_string(), "budget exceeded for github:issues");
    }

    #[test]
    fn malformed_payload_display() {
        let e = ConnectorError::MalformedPayload("missing field `id`".into());
        assert_eq!(e.to_string(), "malformed payload: missing field `id`");
    }

    #[test]
    fn transient_variant_preserves_source_chain() {
        use std::fmt;

        #[derive(Debug)]
        struct Inner;
        impl fmt::Display for Inner {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "inner cause")
            }
        }
        impl std::error::Error for Inner {}

        let inner: Box<dyn std::error::Error + Send + Sync> = Box::new(Inner);
        let e = ConnectorError::Transient(inner);
        // std::error::Error::source() should return Some(&Inner)
        let source = std::error::Error::source(&e);
        assert!(source.is_some());
        assert_eq!(source.unwrap().to_string(), "inner cause");
    }
}
