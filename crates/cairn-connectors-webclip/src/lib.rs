//! Generic web-clipper connector adapter for `cairn-connectors-core`.
//!
//! Issue #131 (slice 2), brief §19 v0.3 connector set, §9.1 source sensors.
//!
//! A **webhook-only**, stateless adapter: a browser extension HMAC-signs and
//! POSTs a captured clip to `POST /webhooks/webclip`. The substrate verifies
//! the signature, then this adapter parses the request into exactly one
//! [`cairn_connectors_core::ConnectorEvent`]. There is no upstream to poll, so
//! `capabilities = { poll: false, webhook: true, backfill: false }`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// Embedded `connector.toml` bytes, parsed at `WebClipConnector::new` time.
///
/// Exposed so integration tests can derive the expected `manifest_hash` when
/// constructing a `ConsentGrant` for the registry end-to-end test.
pub const MANIFEST_TOML: &str = include_str!("../connector.toml");

mod clip;
mod connector;
mod error;
mod event_id;

pub use connector::WebClipConnector;
pub use error::WebClipError;

/// Test-only helpers exposed for integration tests. Cfg-gated; not part of the
/// production API.
#[cfg(any(test, feature = "testkit"))]
pub mod testkit;

#[cfg(test)]
mod manifest_tests {
    use super::MANIFEST_TOML;
    use cairn_connectors_core::ConnectorManifest;

    #[test]
    fn manifest_parses_and_declares_webhook_only() {
        let m = ConnectorManifest::parse_toml(MANIFEST_TOML).expect("manifest parses");
        assert_eq!(m.name(), "webclip");
        assert!(!m.capabilities.poll, "web clipper does not poll");
        assert!(m.capabilities.webhook, "web clipper accepts webhooks");
        assert!(!m.capabilities.backfill, "web clipper has no backfill");
        assert!(m.allowed_label("source:web"));
        assert!(m.allowed_label("kind:clip"));
        assert!(m.scope_matches("domain", "en.wikipedia.org"));
        assert!(m.allowed_mime("application/json"));
        assert!(m.allowed_mime("text/markdown"));
        assert!(m.allowed_mime("text/plain"));
    }
}
