//! Fix 1 — Consent grant labels and manifest hash enforced.
//!
//! Two test scenarios that were previously unguarded:
//!
//! 1. `manifest_drift_after_enable_triggers_consent_revoked` — a connector
//!    whose manifest changes after the consent grant was issued must be
//!    rejected with `ConsentRevoked`, not silently allowed.
//!
//! 2. `event_label_outside_grant_returns_undeclared_label` — a connector
//!    whose grant allows only `["note"]` emits `["note","secret"]`.
//!    The framework must return `UndeclaredLabel { label: "secret" }`.
//!
//! Issue #130, brief §14 "no silent scope widening".

#![allow(missing_docs)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cairn_connectors_core::connector::{
    Connector, ConnectorCapabilities, ConnectorPlugin, PollContext, PollOutcome, WebhookContext,
};
use cairn_connectors_core::event::{
    ConnectorEvent, ConnectorEventId, ConnectorPayload, ConnectorScope, DeliveryMode, SourceRef,
};
use cairn_connectors_core::manifest::ConnectorManifest;
use cairn_connectors_core::webhook::WebhookRequest;
use cairn_connectors_core::{
    ConnectorError, ConnectorRegistry, InMemoryCredentialStore, PipelineEmit,
};
use cairn_core::contract::connector_consent::{
    ConnectorConsentJournal, ConnectorConsentLookup, ConsentGrant, ConsentGrantId,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::Identity;
use cairn_core::domain::capture::CaptureEvent;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// `PanicEmit` — asserts that the pipeline is never reached in error paths.
struct PanicEmit;

#[async_trait]
impl PipelineEmit for PanicEmit {
    async fn emit(&self, _: CaptureEvent) -> Result<(), ConnectorError> {
        panic!("emit must NOT be called — framework gate should have blocked this");
    }
}

/// Accept-all consent journal — every lookup returns `Granted`.
#[derive(Default)]
struct AcceptAllJournal;

#[async_trait]
impl ConnectorConsentJournal for AcceptAllJournal {
    async fn put_grant(&self, _: ConsentGrant) -> Result<ConsentGrantId, String> {
        Ok(ConsentGrantId::new("gnt:test"))
    }

    async fn lookup(
        &self,
        _connector: &str,
        _scope_key: &str,
    ) -> Result<ConnectorConsentLookup, String> {
        Ok(ConnectorConsentLookup::Granted)
    }

    async fn revoke(&self, _: &ConsentGrantId) -> Result<(), String> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// DriftingConnector
//
// On the first `manifest()` call it returns manifest V1; on all subsequent
// calls it returns manifest V2.  Simulates a connector whose manifest is
// widened after the grant was issued.
// ---------------------------------------------------------------------------

const MANIFEST_V1: &str = r#"
[connector]
name = "drifter"
contract = "Connector"
contract_version = "0.1.0"
sensor_identity = "snr:local:connector:drifter:v1"

[capabilities]
poll = true
webhook = false
backfill = false

[oauth]
required_scopes = []
token_lifetime = "1h"
refresh = false

[budget]
max_items_per_hour = 100
max_bytes_per_day = "1MiB"

[labels]
allowed = ["note"]

[[scopes.declared]]
kind = "project"
pattern = "*"

[webhook]
"signature.algorithm" = "hmac-sha256"
"signature.header" = "X-Sig"
allowed_mimes = ["application/json"]

[poll]
cursor_kind = "opaque-string"
min_interval = "30s"
default_interval = "5m"

[payload]
max_bytes = "1KiB"
max_depth = 4
"#;

/// V2 adds a new label — this widens the manifest after grant time.
const MANIFEST_V2: &str = r#"
[connector]
name = "drifter"
contract = "Connector"
contract_version = "0.1.0"
sensor_identity = "snr:local:connector:drifter:v1"

[capabilities]
poll = true
webhook = false
backfill = false

[oauth]
required_scopes = []
token_lifetime = "1h"
refresh = false

[budget]
max_items_per_hour = 100
max_bytes_per_day = "1MiB"

[labels]
allowed = ["note", "secret"]

[[scopes.declared]]
kind = "project"
pattern = "*"

[webhook]
"signature.algorithm" = "hmac-sha256"
"signature.header" = "X-Sig"
allowed_mimes = ["application/json"]

[poll]
cursor_kind = "opaque-string"
min_interval = "30s"
default_interval = "5m"

[payload]
max_bytes = "1KiB"
max_depth = 4
"#;

// ---------------------------------------------------------------------------
// TwoManifestConnector — for manifest drift test
//
// Holds both V1 and V2 manifests; returns V1 initially, V2 after drift().
// ---------------------------------------------------------------------------

struct TwoManifestConnector {
    v1: ConnectorManifest,
    v2: ConnectorManifest,
    drifted: Mutex<bool>,
    sensor: Identity,
}

impl TwoManifestConnector {
    fn new() -> Self {
        Self {
            v1: ConnectorManifest::parse_toml(MANIFEST_V1)
                .expect("invariant: MANIFEST_V1 must parse"),
            v2: ConnectorManifest::parse_toml(MANIFEST_V2)
                .expect("invariant: MANIFEST_V2 must parse"),
            drifted: Mutex::new(false),
            sensor: Identity::parse("snr:local:connector:drifter:v1")
                .expect("invariant: drifter sensor identity must parse"),
        }
    }

    fn drift(&self) {
        *self.drifted.lock().expect("invariant: mutex unpoisoned") = true;
    }
}

#[async_trait]
impl Connector for TwoManifestConnector {
    fn name(&self) -> &'static str {
        "drifter"
    }

    fn manifest(&self) -> &ConnectorManifest {
        if *self.drifted.lock().expect("invariant: mutex unpoisoned") {
            &self.v2
        } else {
            &self.v1
        }
    }

    fn capabilities(&self) -> &ConnectorCapabilities {
        static C: ConnectorCapabilities = ConnectorCapabilities {
            poll: true,
            webhook: false,
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

    async fn poll(&self, _: &PollContext) -> Result<PollOutcome, ConnectorError> {
        Ok(PollOutcome {
            events: vec![ConnectorEvent::new(
                ConnectorEventId::new("01HX0000000000000000000000"),
                "drifter",
                SourceRef::new("issue", "x", None),
                0,
                BTreeSet::from(["note".to_string()]),
                ConnectorScope::project("p"),
                ConnectorPayload::Text {
                    mime: "text/plain".into(),
                    body: "hello".into(),
                },
                DeliveryMode::Poll { cursor: None },
            )],
            next_cursor: None,
            rate_limit_hint: None,
        })
    }

    async fn ingest_webhook(
        &self,
        _: &WebhookRequest,
        _: &WebhookContext,
    ) -> Result<Vec<ConnectorEvent>, ConnectorError> {
        Ok(vec![])
    }
}

impl ConnectorPlugin for TwoManifestConnector {
    const NAME: &'static str = "drifter";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
}

// ---------------------------------------------------------------------------
// NarrowGrantConnector — emits a label outside the grant's allowed_labels
// ---------------------------------------------------------------------------

const NARROW_MANIFEST: &str = r#"
[connector]
name = "narrow"
contract = "Connector"
contract_version = "0.1.0"
sensor_identity = "snr:local:connector:narrow:v1"

[capabilities]
poll = true
webhook = false
backfill = false

[oauth]
required_scopes = []
token_lifetime = "1h"
refresh = false

[budget]
max_items_per_hour = 100
max_bytes_per_day = "1MiB"

[labels]
allowed = ["note", "secret"]

[[scopes.declared]]
kind = "project"
pattern = "*"

[webhook]
"signature.algorithm" = "hmac-sha256"
"signature.header" = "X-Sig"
allowed_mimes = ["application/json"]

[poll]
cursor_kind = "opaque-string"
min_interval = "30s"
default_interval = "5m"

[payload]
max_bytes = "1KiB"
max_depth = 4
"#;

/// Connector whose manifest allows `["note","secret"]` but the grant only
/// allows `["note"]`. Emits both labels so the grant check triggers.
struct NarrowGrantConnector {
    manifest: ConnectorManifest,
    sensor: Identity,
}

impl NarrowGrantConnector {
    fn new() -> Self {
        Self {
            manifest: ConnectorManifest::parse_toml(NARROW_MANIFEST)
                .expect("invariant: NARROW_MANIFEST must parse"),
            sensor: Identity::parse("snr:local:connector:narrow:v1")
                .expect("invariant: narrow sensor identity must parse"),
        }
    }
}

#[async_trait]
impl Connector for NarrowGrantConnector {
    fn name(&self) -> &'static str {
        "narrow"
    }

    fn manifest(&self) -> &ConnectorManifest {
        &self.manifest
    }

    fn capabilities(&self) -> &ConnectorCapabilities {
        static C: ConnectorCapabilities = ConnectorCapabilities {
            poll: true,
            webhook: false,
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

    async fn poll(&self, _: &PollContext) -> Result<PollOutcome, ConnectorError> {
        // Emit ["note", "secret"] — "secret" is in the manifest but NOT in the
        // grant (which only allows ["note"]).
        Ok(PollOutcome {
            events: vec![ConnectorEvent::new(
                ConnectorEventId::new("01HX0000000000000000000000"),
                "narrow",
                SourceRef::new("issue", "x", None),
                0,
                BTreeSet::from(["note".to_string(), "secret".to_string()]),
                ConnectorScope::project("p"),
                ConnectorPayload::Text {
                    mime: "text/plain".into(),
                    body: "hello".into(),
                },
                DeliveryMode::Poll { cursor: None },
            )],
            next_cursor: None,
            rate_limit_hint: None,
        })
    }

    async fn ingest_webhook(
        &self,
        _: &WebhookRequest,
        _: &WebhookContext,
    ) -> Result<Vec<ConnectorEvent>, ConnectorError> {
        Ok(vec![])
    }
}

impl ConnectorPlugin for NarrowGrantConnector {
    const NAME: &'static str = "narrow";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A connector whose manifest drifts (widens) after the consent grant was
/// issued must be rejected with `ConsentRevoked` on the next `poll_now`.
///
/// This guards against a connector author widening the manifest post-grant
/// to gain access to labels the user never approved (brief §14).
#[tokio::test]
async fn manifest_drift_after_enable_triggers_consent_revoked() {
    let connector = Arc::new(TwoManifestConnector::new());

    // Grant hash matches V1 (the manifest at enable-time).
    let v1_hash = connector.v1.hash();
    let grant = ConsentGrant::new(
        "drifter",
        v1_hash,
        BTreeSet::from(["note".to_string()]),
        vec!["project:*".to_string()],
        1_700_000_000,
        Identity::parse("hmn:alice").expect("invariant: hmn:alice is valid"),
    );

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllJournal))
        .emit(Arc::new(PanicEmit))
        .build();

    // register takes ownership; we use a wrapper that shares the connector Arc
    // so we can call drift() later.
    reg.register(TwoManifestConnector::new())
        .expect("register must succeed");
    reg.enable("drifter", grant)
        .await
        .expect("enable must succeed");

    // Simulate manifest drift: the connector now returns V2 from manifest().
    // We cannot drift the registered connector from outside — instead we build
    // a second registry with the drifted connector to simulate post-drift state.
    // This is the cleanest test approach given the registry takes ownership.

    reg.shutdown().await;

    // Build a second registry where the connector starts drifted.
    // The grant still carries the V1 hash.
    let drifter = TwoManifestConnector::new();
    drifter.drift(); // manifest() now returns V2

    let v1_hash_again = ConnectorManifest::parse_toml(MANIFEST_V1)
        .expect("V1 parses")
        .hash();
    let grant2 = ConsentGrant::new(
        "drifter",
        v1_hash_again, // grant still references V1 hash
        BTreeSet::from(["note".to_string()]),
        vec!["project:*".to_string()],
        1_700_000_000,
        Identity::parse("hmn:alice").expect("invariant: hmn:alice is valid"),
    );

    let mut reg2 = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllJournal))
        .emit(Arc::new(PanicEmit))
        .build();

    reg2.register(drifter).expect("register must succeed");
    reg2.enable("drifter", grant2)
        .await
        .expect("enable must succeed");

    // poll_now must fail with ConsentRevoked because manifest hash drifted.
    let err = reg2
        .poll_now("drifter")
        .await
        .expect_err("poll_now must fail when manifest hash has drifted");

    assert!(
        matches!(err, ConnectorError::ConsentRevoked { ref connector } if connector == "drifter"),
        "expected ConsentRevoked{{connector: \"drifter\"}}, got {err:?}",
    );

    reg2.shutdown().await;
}

/// A connector whose grant allows only `["note"]` must be rejected with
/// `UndeclaredLabel { label: "secret" }` when it tries to emit `["note","secret"]`.
///
/// This ensures the grant's `allowed_labels` is checked, not just the manifest's
/// label allow-list (which may be wider than what the user approved).
#[tokio::test]
async fn event_label_outside_grant_returns_undeclared_label() {
    // Grant allows only "note"; manifest allows "note" + "secret".
    let manifest_hash = ConnectorManifest::parse_toml(NARROW_MANIFEST)
        .expect("NARROW_MANIFEST parses")
        .hash();
    let grant = ConsentGrant::new(
        "narrow",
        manifest_hash,
        BTreeSet::from(["note".to_string()]), // only "note" — NOT "secret"
        vec!["project:*".to_string()],
        1_700_000_000,
        Identity::parse("hmn:alice").expect("invariant: hmn:alice is valid"),
    );

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllJournal))
        .emit(Arc::new(PanicEmit))
        .build();

    reg.register(NarrowGrantConnector::new())
        .expect("register must succeed");
    reg.enable("narrow", grant)
        .await
        .expect("enable must succeed");

    let err = reg
        .poll_now("narrow")
        .await
        .expect_err("poll_now must fail when event label is outside grant");

    assert!(
        matches!(err, ConnectorError::UndeclaredLabel { ref label } if label == "secret"),
        "expected UndeclaredLabel{{label: \"secret\"}}, got {err:?}",
    );

    reg.shutdown().await;
}
