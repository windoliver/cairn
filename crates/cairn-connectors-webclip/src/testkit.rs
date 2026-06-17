//! Test-only helpers for building signed web-clip webhook requests.
//!
//! Cfg-gated; not part of the production API surface. Integration tests in
//! `tests/` use these to construct `WebhookRequest`s for direct
//! `ingest_webhook` calls.

use cairn_connectors_core::WebhookRequest;
use cairn_connectors_core::webhook::hex_hmac_sha256;

/// Build a signed `application/json` clip request from a raw JSON body.
#[must_use]
pub fn json_clip_request(secret: &[u8], json_body: &str) -> WebhookRequest {
    let sig = format!("sha256={}", hex_hmac_sha256(secret, json_body.as_bytes()));
    WebhookRequest {
        connector: "webclip".into(),
        body: json_body.as_bytes().to_vec(),
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            ("X-Cairn-Signature-256".into(), sig),
        ],
    }
}

/// Build a signed text-mode clip request (`mime` = `text/markdown` or
/// `text/plain`) with the `X-Cairn-Clip-*` metadata headers.
#[must_use]
pub fn text_clip_request(
    secret: &[u8],
    mime: &str,
    url: &str,
    captured_at: i64,
    body: &str,
) -> WebhookRequest {
    let sig = format!("sha256={}", hex_hmac_sha256(secret, body.as_bytes()));
    WebhookRequest {
        connector: "webclip".into(),
        body: body.as_bytes().to_vec(),
        headers: vec![
            ("Content-Type".into(), mime.to_owned()),
            ("X-Cairn-Signature-256".into(), sig),
            ("X-Cairn-Clip-Url".into(), url.to_owned()),
            ("X-Cairn-Clip-Captured-At".into(), captured_at.to_string()),
        ],
    }
}
