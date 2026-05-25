//! Webhook request envelope + HMAC-SHA256 verifier.
//!
//! The framework owns the HTTP layer (`axum::Router`), but signature
//! verification is a pure function that lives here so connectors and tests
//! can call it directly without pulling in any HTTP machinery.
//!
//! Issue #130, brief §9.1 source sensors, §19 v0.3.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::error::ConnectorError;

/// HMAC-SHA256 keyed by the shared secret.
type HmacSha256 = Hmac<Sha256>;

/// An inbound webhook HTTP request as seen by the framework, stripped of
/// transport-layer details. The framework constructs this from the raw
/// `axum` request before handing it to `Connector::ingest_webhook`.
#[derive(Debug, Clone)]
pub struct WebhookRequest {
    /// Name of the connector that owns this webhook endpoint.
    pub connector: String,
    /// Raw request body bytes (pre-read by the framework).
    pub body: Vec<u8>,
    /// HTTP headers as `(name, value)` pairs. Names are stored in the
    /// casing the upstream sends; use [`WebhookRequest::header`] for
    /// case-insensitive lookup.
    pub headers: Vec<(String, String)>,
}

impl WebhookRequest {
    /// Find a header value by name, ignoring ASCII case.
    ///
    /// Returns the first match, or `None` if no header with that name exists.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Opaque identifier for a verified signature, containing the hex-encoded
/// MAC that was confirmed correct. Stored in `DeliveryMode::Webhook` so
/// replay detection can reference it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureId(pub String);

/// Verify an HMAC-SHA256 signature whose hex form is carried in `header`.
///
/// All four rejection paths (missing header, bad hex, MAC keying failure,
/// MAC mismatch) return [`ConnectorError::SignatureMismatch`] — the caller
/// learns only that verification failed, never *why*, so timing and error
/// messages cannot assist an attacker.
///
/// The MAC comparison uses [`subtle::ConstantTimeEq`] to prevent
/// timing-side-channel leaks.
///
/// # Errors
///
/// Returns [`ConnectorError::SignatureMismatch`] if the signature is absent,
/// unparseable, or does not match.
pub fn verify_hmac_sha256(
    req: &WebhookRequest,
    header: &str,
    secret: &[u8],
) -> Result<SignatureId, ConnectorError> {
    // Rejection path 1: missing header.
    let sig_hex = req
        .header(header)
        .ok_or(ConnectorError::SignatureMismatch)?;

    // Rejection path 2: bad hex.
    let provided = hex::decode(sig_hex).map_err(|_| ConnectorError::SignatureMismatch)?;

    // SHA-256 output is always 32 bytes; any other length is treated as
    // bad hex. Pre-checking keeps the constant-time path on the byte loop,
    // not on the length field of GenericArray vs Vec<u8>.
    if provided.len() != 32 {
        return Err(ConnectorError::SignatureMismatch);
    }

    // Rejection path 3: MAC keying failure (unreachable in practice — only
    // fails if the key length overflows usize, which cannot happen here, but
    // we map it to SignatureMismatch per the single-rejection-reason rule).
    let mut mac =
        HmacSha256::new_from_slice(secret).map_err(|_| ConnectorError::SignatureMismatch)?;

    mac.update(&req.body);
    let computed = mac.finalize().into_bytes();

    // Rejection path 4: MAC mismatch. MUST use constant-time comparison;
    // never use `==` on byte slices here.
    if computed.ct_eq(&provided).into() {
        Ok(SignatureId(sig_hex.to_string()))
    } else {
        Err(ConnectorError::SignatureMismatch)
    }
}

/// Produce the canonical HMAC-SHA256 hex signature for `body` under `secret`.
///
/// Used by tests and adapter crates that need to mint a valid signature to
/// attach to a [`WebhookRequest`].
#[must_use]
pub fn hex_hmac_sha256(secret: &[u8], body: &[u8]) -> String {
    // invariant: HmacSha256 has no key length restriction
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

/// Composes per-connector `axum` routes that the registry mounts under
/// `/webhooks`.
///
/// Left as a thin shell here so the registry can compose it without a circular
/// dependency on adapter crates. Call [`WebhookRouter::mount`] once per
/// connector route, then [`WebhookRouter::into_router`] to obtain the merged
/// `axum::Router` to pass to axum's server builder.
#[derive(Default)]
pub struct WebhookRouter {
    routes: Vec<axum::Router>,
}

impl WebhookRouter {
    /// Create an empty router.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a connector-specific sub-router.
    pub fn mount(&mut self, route: axum::Router) {
        self.routes.push(route);
    }

    /// Merge all registered sub-routers into a single `axum::Router`.
    ///
    /// The returned router has no authentication middleware; the registry
    /// adds those at mount time.
    pub fn into_router(self) -> axum::Router {
        let mut r = axum::Router::new();
        for sub in self.routes {
            r = r.merge(sub);
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_hmac_sha256_signature() {
        let secret = b"shh";
        let body = b"{\"x\":1}";
        let sig = hex_hmac_sha256(secret, body);
        let req = WebhookRequest {
            connector: "fixture".into(),
            body: body.to_vec(),
            headers: vec![("X-Fixture-Signature".into(), sig.clone())],
        };
        let res = verify_hmac_sha256(&req, "X-Fixture-Signature", secret);
        assert!(matches!(res, Ok(SignatureId(s)) if s == sig));
    }

    #[test]
    fn rejects_wrong_signature() {
        let req = WebhookRequest {
            connector: "fixture".into(),
            body: b"{}".to_vec(),
            headers: vec![("X-Fixture-Signature".into(), "deadbeef".into())],
        };
        assert!(matches!(
            verify_hmac_sha256(&req, "X-Fixture-Signature", b"shh"),
            Err(crate::ConnectorError::SignatureMismatch),
        ));
    }

    #[test]
    fn header_lookup_is_case_insensitive() {
        let req = WebhookRequest {
            connector: "fixture".into(),
            body: vec![],
            headers: vec![("X-Sig".into(), "value123".into())],
        };
        // Both casings must resolve to the same value.
        assert_eq!(req.header("X-Sig"), Some("value123"));
        assert_eq!(req.header("x-sig"), Some("value123"));
        assert_eq!(req.header("X-SIG"), Some("value123"));
    }

    #[test]
    fn signature_id_round_trips_through_verifier() {
        let secret = b"round-trip-secret";
        let body = b"payload";
        let expected_hex = hex_hmac_sha256(secret, body);
        let req = WebhookRequest {
            connector: "fixture".into(),
            body: body.to_vec(),
            headers: vec![("X-Hmac".into(), expected_hex.clone())],
        };
        let SignatureId(id) = verify_hmac_sha256(&req, "X-Hmac", secret).expect("should verify");
        // The SignatureId must carry exactly the hex string that was provided.
        assert_eq!(id, expected_hex);
    }
}
