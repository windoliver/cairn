//! Tests for Finding C: sensor-identity binding validation in
//! [`ConnectorRegistry::register`].
//!
//! The registry must reject plugins whose runtime `sensor_identity()` does not
//! match the manifest's declared `connector.sensor_identity`, or whose identity
//! wire form does not include the correct connector-name segment.

#![allow(missing_docs)]

use std::sync::Mutex;

use cairn_connectors_core::fixture::AcceptAllConsent;
use cairn_connectors_core::{
    ConnectorError, ConnectorRegistry, InMemoryCredentialStore, PipelineEmit,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::Identity;
use cairn_core::domain::capture::CaptureEvent;

use cairn_connectors_core::connector::{
    Connector, ConnectorCapabilities, ConnectorPlugin, PollContext, PollOutcome, WebhookContext,
};
use cairn_connectors_core::event::ConnectorEvent;
use cairn_connectors_core::manifest::ConnectorManifest;
use cairn_connectors_core::webhook::WebhookRequest;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Build a `ConnectorManifest` TOML for a connector named `name` with the
/// given `sensor_identity` declared in the `[connector]` block.
fn manifest_toml(connector_name: &str, sensor_identity: &str) -> String {
    format!(
        r#"
[connector]
name = "{connector_name}"
contract = "Connector"
contract_version = "0.1.0"
sensor_identity = "{sensor_identity}"

[capabilities]
poll = false
webhook = false
backfill = false

[oauth]
required_scopes = ["read"]
token_lifetime = "1h"
refresh = true

[budget]
max_items_per_hour = 100
max_bytes_per_day = "50MiB"

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
max_bytes = "256KiB"
max_depth = 10
"#
    )
}

/// A [`ConnectorPlugin`] whose runtime `sensor_identity()` is set
/// independently from the manifest's declared `connector.sensor_identity`.
///
/// This lets tests exercise the mismatch detection in `register()`.
struct SpoofableConnector {
    /// Connector name returned by `name()` / `ConnectorPlugin::NAME`.
    name: &'static str,
    /// Parsed manifest — its `connector.sensor_identity` is the *declared* form.
    manifest: ConnectorManifest,
    /// Runtime identity returned by `sensor_identity()` — may differ from the
    /// manifest's declared value.
    runtime_sensor: Identity,
}

impl SpoofableConnector {
    fn new(name: &'static str, manifest: ConnectorManifest, runtime_sensor: Identity) -> Self {
        Self {
            name,
            manifest,
            runtime_sensor,
        }
    }
}

#[async_trait::async_trait]
impl Connector for SpoofableConnector {
    fn name(&self) -> &str {
        self.name
    }

    fn manifest(&self) -> &ConnectorManifest {
        &self.manifest
    }

    fn capabilities(&self) -> &ConnectorCapabilities {
        static C: ConnectorCapabilities = ConnectorCapabilities {
            poll: false,
            webhook: false,
            backfill: false,
        };
        &C
    }

    fn sensor_identity(&self) -> &Identity {
        &self.runtime_sensor
    }

    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
    }

    async fn poll(&self, _cx: &PollContext) -> Result<PollOutcome, ConnectorError> {
        Ok(PollOutcome::default())
    }

    async fn ingest_webhook(
        &self,
        _req: &WebhookRequest,
        _cx: &WebhookContext,
    ) -> Result<Vec<ConnectorEvent>, ConnectorError> {
        Ok(vec![])
    }
}

/// `ConnectorPlugin` marker for the `SpoofableConnector`.
///
/// `NAME` is set to `"fixture"` but the runtime sensor can be anything.
impl ConnectorPlugin for SpoofableConnector {
    const NAME: &'static str = "fixture";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
}

/// A no-op [`PipelineEmit`] that discards every event (tests here never reach
/// the emit stage because register fails).
#[derive(Default)]
struct Discard(Mutex<Vec<CaptureEvent>>);

#[async_trait::async_trait]
impl PipelineEmit for Discard {
    async fn emit(&self, event: CaptureEvent) -> Result<(), ConnectorError> {
        self.0
            .lock()
            .expect("invariant: Discard mutex unpoisoned")
            .push(event);
        Ok(())
    }
}

/// Build a minimal registry wired with `AcceptAllConsent` and `Discard`.
fn make_registry() -> ConnectorRegistry {
    use std::sync::Arc;
    ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(Arc::new(Discard::default()))
        .build()
}

// ---------------------------------------------------------------------------
// Finding C — tests
// ---------------------------------------------------------------------------

/// `register` must reject a plugin whose runtime `sensor_identity()` returns a
/// different string than the manifest's `connector.sensor_identity`.
///
/// Scenario:
/// - Manifest declares `snr:local:connector:fixture:v1`.
/// - Runtime returns `snr:local:connector:other:v1`.
/// → `register` must return `ConnectorError::Fatal`.
#[tokio::test]
async fn register_rejects_identity_mismatch_with_manifest() {
    let mut reg = make_registry();

    let manifest =
        ConnectorManifest::parse_toml(&manifest_toml("fixture", "snr:local:connector:fixture:v1"))
            .expect("manifest must parse");

    let runtime_sensor =
        Identity::parse("snr:local:connector:other:v1").expect("runtime sensor must parse");

    let plugin = SpoofableConnector::new("fixture", manifest, runtime_sensor);

    let err = reg
        .register(plugin)
        .expect_err("register must fail when runtime identity != manifest identity");

    assert!(
        matches!(err, ConnectorError::Fatal(_)),
        "expected Fatal on identity mismatch, got {err:?}"
    );
}

/// `register` must reject a plugin whose runtime `sensor_identity()` uses the
/// correct manifest string but where `plugin.name()` does not match the name
/// segment in the wire form.
///
/// Scenario:
/// - Manifest declares `snr:local:connector:other:v1` (name field = "other").
/// - Runtime returns `snr:local:connector:other:v1` (matches manifest).
/// - But `plugin.name()` returns `"fixture"`, not `"other"`.
/// → The `<name>` segment in the identity (`other`) does not match
///   `plugin.name()` (`fixture`) → `register` must return `ConnectorError::Fatal`.
#[tokio::test]
async fn register_rejects_identity_with_wrong_name_segment() {
    let mut reg = make_registry();

    // Manifest name = "other", sensor_identity segment = "other".
    let manifest =
        ConnectorManifest::parse_toml(&manifest_toml("other", "snr:local:connector:other:v1"))
            .expect("manifest must parse");

    // Runtime identity matches manifest exactly …
    let runtime_sensor =
        Identity::parse("snr:local:connector:other:v1").expect("runtime sensor must parse");

    // … but the plugin's `name()` (and `ConnectorPlugin::NAME`) is "fixture".
    let plugin = SpoofableConnector::new("fixture", manifest, runtime_sensor);

    let err = reg
        .register(plugin)
        .expect_err("register must fail when name() != identity name segment");

    assert!(
        matches!(err, ConnectorError::Fatal(_)),
        "expected Fatal on name-segment mismatch, got {err:?}"
    );
}

/// `register` must accept a plugin whose runtime identity matches both the
/// manifest's declared value and the connector's name.
///
/// This is the happy path exercised by `FixtureConnector`.
#[tokio::test]
async fn register_accepts_matching_identity_and_name() {
    use cairn_connectors_core::fixture::FixtureConnector;

    let mut reg = make_registry();

    let result = reg.register(FixtureConnector::with_default_manifest());
    assert!(
        result.is_ok(),
        "register must succeed for FixtureConnector with aligned identity: {result:?}"
    );
}
