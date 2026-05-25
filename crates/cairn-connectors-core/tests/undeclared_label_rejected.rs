//! T20a — `UndeclaredLabel` gate.
//!
//! `EvilConnector` emits an event whose label (`"forbidden"`) is not in its
//! manifest's `labels.allowed` list. The framework must reject it with
//! `ConnectorError::UndeclaredLabel` and must never call `PipelineEmit::emit`.
//!
//! Issue #130, brief §9.1 source sensors.

#![allow(missing_docs)]

use std::collections::BTreeSet;
use std::sync::Arc;

use cairn_connectors_core::connector::{
    Connector, ConnectorCapabilities, ConnectorPlugin, PollContext, PollOutcome, WebhookContext,
};
use cairn_connectors_core::event::{
    ConnectorEvent, ConnectorEventId, ConnectorPayload, ConnectorScope, DeliveryMode, SourceRef,
};
use cairn_connectors_core::fixture::AcceptAllConsent;
use cairn_connectors_core::manifest::ConnectorManifest;
use cairn_connectors_core::webhook::WebhookRequest;
use cairn_connectors_core::{
    ConnectorError, ConnectorRegistry, InMemoryCredentialStore, PipelineEmit,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::Identity;
use cairn_core::domain::capture::CaptureEvent;

// ---------------------------------------------------------------------------
// EvilConnector — emits an event with a label not declared in its manifest
// ---------------------------------------------------------------------------

const EVIL_MANIFEST: &str = r#"
[connector]
name = "evil"
contract = "Connector"
contract_version = "0.1.0"
sensor_identity = "snr:local:connector:evil:v1"

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

struct EvilConnector {
    manifest: ConnectorManifest,
    sensor: Identity,
}

impl EvilConnector {
    fn new() -> Self {
        Self {
            manifest: ConnectorManifest::parse_toml(EVIL_MANIFEST)
                .expect("invariant: EVIL_MANIFEST must be valid TOML"),
            // Invariant: canonical local-connector wire form (brief §4.2 + §19 v0.3).
            sensor: Identity::parse("snr:local:connector:evil:v1")
                .expect("invariant: evil sensor identity is a valid snr:local:connector wire form"),
        }
    }
}

#[async_trait::async_trait]
impl Connector for EvilConnector {
    fn name(&self) -> &'static str {
        "evil"
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
        // Emit an event whose label "forbidden" is NOT in the manifest allow-list.
        Ok(PollOutcome {
            events: vec![ConnectorEvent::new(
                ConnectorEventId::new("01HX0000000000000000000000"),
                "evil",
                SourceRef::new("issue", "x", None),
                0,
                BTreeSet::from(["forbidden".to_string()]),
                ConnectorScope::project("p"),
                ConnectorPayload::Text {
                    mime: "text/plain".into(),
                    body: String::new(),
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

impl ConnectorPlugin for EvilConnector {
    const NAME: &'static str = "evil";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
}

// ---------------------------------------------------------------------------
// PanicEmit — test guard: emit must never be called for this path
// ---------------------------------------------------------------------------

struct PanicEmit;

#[async_trait::async_trait]
impl PipelineEmit for PanicEmit {
    async fn emit(&self, _: CaptureEvent) -> Result<(), ConnectorError> {
        panic!("emit must NOT be called for an undeclared-label event");
    }
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn undeclared_label_rejected_at_framework_boundary() {
    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(Arc::new(PanicEmit) as Arc<dyn PipelineEmit>)
        .build();

    reg.register(EvilConnector::new())
        .expect("register must succeed");
    reg.enable("evil", default_grant_for_evil())
        .await
        .expect("enable must succeed");

    let err = reg
        .poll_now("evil")
        .await
        .expect_err("poll_now must fail for undeclared label");

    assert!(
        matches!(err, ConnectorError::UndeclaredLabel { ref label } if label == "forbidden"),
        "expected UndeclaredLabel{{label: \"forbidden\"}}, got {err:?}",
    );

    reg.shutdown().await;
}

/// Build a minimal `ConsentGrant` for the evil connector.
///
/// Mirrors `default_grant()` in the fixture module but covers the "evil"
/// connector name and its allowed label set.
fn default_grant_for_evil() -> cairn_core::contract::connector_consent::ConsentGrant {
    use std::collections::BTreeSet;
    cairn_core::contract::connector_consent::ConsentGrant::new(
        "evil",
        "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        BTreeSet::from(["note".to_string()]),
        vec!["project:*".to_string()],
        1_700_000_000,
        // Invariant: "hmn:alice" is a valid human identity wire form.
        Identity::parse("hmn:alice").expect("invariant: hmn:alice is a valid Identity"),
    )
}
