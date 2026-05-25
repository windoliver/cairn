//! In-tree connector + consent journal used by every framework test.
//!
//! Cfg-gated to keep the substrate crate runtime-clean for downstream
//! consumers that opt in with `features = ["fixture"]`.
//!
//! # Usage
//! ```rust,no_run
//! use cairn_connectors_core::fixture::{AcceptAllConsent, FixtureConnector, default_grant};
//! let connector = FixtureConnector::with_default_manifest();
//! let grant = default_grant();
//! let consent = AcceptAllConsent::default();
//! ```

use std::collections::BTreeSet;
use std::sync::Mutex;

use async_trait::async_trait;
use cairn_core::contract::connector_consent::{
    ConnectorConsentJournal, ConnectorConsentLookup, ConsentGrant, ConsentGrantId,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::Identity;

use crate::connector::{
    Connector, ConnectorCapabilities, ConnectorPlugin, PollContext, PollOutcome, WebhookContext,
};
use crate::error::ConnectorError;
use crate::event::{
    ConnectorEvent, ConnectorEventId, ConnectorPayload, ConnectorScope, DeliveryMode, SourceRef,
};
use crate::manifest::ConnectorManifest;
use crate::webhook::WebhookRequest;

// ---------------------------------------------------------------------------
// FixtureConnector
// ---------------------------------------------------------------------------

/// Minimal in-tree connector used by framework tests.
///
/// Implements both poll and webhook capabilities; each invocation returns
/// exactly one [`sample_event`]. No network I/O, no credentials required.
#[derive(Debug)]
pub struct FixtureConnector {
    manifest: ConnectorManifest,
    sensor: Identity,
}

impl FixtureConnector {
    /// Build a `FixtureConnector` from [`DEFAULT_MANIFEST`].
    ///
    /// # Panics
    /// Panics only if `DEFAULT_MANIFEST` is malformed (compile-time
    /// invariant — covered by `default_manifest_parses` unit test).
    #[must_use]
    pub fn with_default_manifest() -> Self {
        // Invariant: DEFAULT_MANIFEST is a valid manifest TOML; parse errors
        // indicate a programming mistake, not a runtime condition.
        let manifest = ConnectorManifest::parse_toml(DEFAULT_MANIFEST)
            .expect("invariant: DEFAULT_MANIFEST must be valid TOML");
        // Invariant: "snr:local:connector:fixture:v1" is a valid sensor
        // identity in the canonical local-connector wire form (brief §4.2 + §19 v0.3).
        let sensor = Identity::parse("snr:local:connector:fixture:v1")
            .expect("invariant: fixture sensor identity is a valid snr:local:connector wire form");
        Self { manifest, sensor }
    }
}

#[async_trait]
impl Connector for FixtureConnector {
    fn name(&self) -> &str {
        self.manifest.name()
    }

    fn manifest(&self) -> &ConnectorManifest {
        &self.manifest
    }

    fn capabilities(&self) -> &ConnectorCapabilities {
        static C: ConnectorCapabilities = ConnectorCapabilities {
            poll: true,
            webhook: true,
            backfill: false,
        };
        &C
    }

    fn sensor_identity(&self) -> &Identity {
        &self.sensor
    }

    fn supported_contract_versions(&self) -> VersionRange {
        Self::SUPPORTED_VERSIONS
    }

    async fn poll(&self, _cx: &PollContext) -> Result<PollOutcome, ConnectorError> {
        Ok(PollOutcome {
            events: vec![sample_event()],
            next_cursor: Some("c1".into()),
            rate_limit_hint: None,
        })
    }

    async fn ingest_webhook(
        &self,
        _req: &WebhookRequest,
        _cx: &WebhookContext,
    ) -> Result<Vec<ConnectorEvent>, ConnectorError> {
        Ok(vec![sample_event()])
    }
}

impl ConnectorPlugin for FixtureConnector {
    const NAME: &'static str = "fixture";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
}

// ---------------------------------------------------------------------------
// sample_event helper
// ---------------------------------------------------------------------------

/// Build one deterministic fixture event for use in poll/webhook responses.
fn sample_event() -> ConnectorEvent {
    ConnectorEvent::new(
        ConnectorEventId::new("01HX0000000000000000000000"),
        "fixture",
        SourceRef::new("issue", "gh:owner/repo#1", None),
        1_700_000_000,
        BTreeSet::from(["note".to_string()]),
        ConnectorScope::project("owner/repo"),
        ConnectorPayload::Json {
            mime: "application/json".into(),
            body: serde_json::json!({"body": "hello"}),
        },
        DeliveryMode::Poll {
            cursor: Some("c0".into()),
        },
    )
}

// ---------------------------------------------------------------------------
// default_grant
// ---------------------------------------------------------------------------

/// Build a minimal [`ConsentGrant`] covering the fixture connector.
///
/// Used by integration tests that need a pre-populated consent journal entry
/// without calling `ConnectorRegistry::enable`.
#[must_use]
pub fn default_grant() -> ConsentGrant {
    ConsentGrant::new(
        "fixture",
        "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        BTreeSet::from(["note".to_string(), "comment".to_string()]),
        vec!["project:*".to_string()],
        1_700_000_000,
        // Invariant: "hmn:alice" is a valid human identity wire form.
        Identity::parse("hmn:alice").expect("invariant: hmn:alice is a valid Identity"),
    )
}

// ---------------------------------------------------------------------------
// AcceptAllConsent
// ---------------------------------------------------------------------------

/// Accept-all [`ConnectorConsentJournal`] for use in tests.
///
/// Every `put_grant` call succeeds and records the grant; every `lookup` for
/// a connector with a recorded grant returns [`ConnectorConsentLookup::Granted`];
/// `revoke` clears all recorded grants for simplicity (sufficient for tests).
///
/// Uses `std::sync::Mutex` because no lock is held across an `.await` point.
#[derive(Debug, Default)]
pub struct AcceptAllConsent {
    grants: Mutex<Vec<ConsentGrant>>,
}

#[async_trait]
impl ConnectorConsentJournal for AcceptAllConsent {
    async fn put_grant(&self, g: ConsentGrant) -> Result<ConsentGrantId, String> {
        let id = ConsentGrantId::new(format!("gnt:{}:fixture", g.connector));
        self.grants
            .lock()
            .expect("invariant: AcceptAllConsent mutex unpoisoned")
            .push(g);
        Ok(id)
    }

    async fn lookup(
        &self,
        connector: &str,
        _scope_key: &str,
    ) -> Result<ConnectorConsentLookup, String> {
        let granted = self
            .grants
            .lock()
            .expect("invariant: AcceptAllConsent mutex unpoisoned")
            .iter()
            .any(|g| g.connector == connector);
        Ok(if granted {
            ConnectorConsentLookup::Granted
        } else {
            ConnectorConsentLookup::Revoked
        })
    }

    async fn revoke(&self, _id: &ConsentGrantId) -> Result<(), String> {
        self.grants
            .lock()
            .expect("invariant: AcceptAllConsent mutex unpoisoned")
            .clear();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// DEFAULT_MANIFEST constant
// ---------------------------------------------------------------------------

/// Canonical fixture connector manifest TOML.
///
/// `sensor_identity` uses `snr:local:connector:fixture:v1` — the canonical
/// local-connector wire form (brief §4.2 + §19 v0.3, aligned with
/// `cairn-core::domain::capture_manifest::P0_SENSOR_LABEL_PREFIXES`).
pub const DEFAULT_MANIFEST: &str = r#"
[connector]
name = "fixture"
contract = "Connector"
contract_version = "0.1.0"
sensor_identity = "snr:local:connector:fixture:v1"

[capabilities]
poll = true
webhook = true
backfill = false

[oauth]
required_scopes = ["read:fixture"]
token_lifetime = "1h"
refresh = true

[budget]
max_items_per_hour = 600
max_bytes_per_day = "50MiB"

[labels]
allowed = ["note", "comment"]

[[scopes.declared]]
kind = "project"
pattern = "*"

[webhook]
"signature.algorithm" = "hmac-sha256"
"signature.header" = "X-Fixture-Signature"
allowed_mimes = ["application/json"]

[poll]
cursor_kind = "opaque-string"
min_interval = "30s"
default_interval = "5m"

[payload]
max_bytes = "256KiB"
max_depth = 12
"#;

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::connector::Connector;

    /// `DEFAULT_MANIFEST` must parse without errors and carry the correct
    /// `sensor_identity` wire form.
    #[test]
    fn default_manifest_parses() {
        let m = ConnectorManifest::parse_toml(DEFAULT_MANIFEST).expect("DEFAULT_MANIFEST parses");
        assert_eq!(m.name(), "fixture");
        assert_eq!(
            m.connector.sensor_identity, "snr:local:connector:fixture:v1",
            "sensor_identity must use the canonical snr:local:connector wire form"
        );
    }

    /// `FixtureConnector` must advertise both poll and webhook capabilities.
    #[test]
    fn fixture_connector_advertises_poll_and_webhook() {
        let c = FixtureConnector::with_default_manifest();
        let caps = c.capabilities();
        assert!(caps.poll, "FixtureConnector must advertise poll");
        assert!(caps.webhook, "FixtureConnector must advertise webhook");
    }

    /// `default_grant` must reference the fixture connector and have a
    /// non-empty `allowed_labels` set.
    #[test]
    fn default_grant_has_required_fields() {
        let g = default_grant();
        assert_eq!(
            g.connector, "fixture",
            "default_grant connector must be \"fixture\""
        );
        assert!(
            !g.allowed_labels.is_empty(),
            "default_grant must have at least one allowed label"
        );
    }

    /// `FixtureConnector` must be `Send + Sync` (required by `Arc<dyn Connector>`).
    #[test]
    fn fixture_connector_is_arc_dyn_connector() {
        let _: Arc<dyn Connector> = Arc::new(FixtureConnector::with_default_manifest());
    }
}
