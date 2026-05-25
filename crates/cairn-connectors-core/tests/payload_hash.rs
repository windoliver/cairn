//! Tests for Finding D: real SHA-256 payload hash emitted by `process_event`.
//!
//! `process_event` previously emitted a placeholder all-zero hash for every
//! event, defeating downstream deduplication, audit, and replay. After the
//! fix it must compute the hash from the post-redaction wire bytes of the
//! payload.

#![allow(missing_docs)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use cairn_connectors_core::connector::{
    Connector, ConnectorCapabilities, ConnectorPlugin, PollContext, PollOutcome, WebhookContext,
};
use cairn_connectors_core::event::{
    ConnectorEvent, ConnectorEventId, ConnectorPayload, ConnectorScope, DeliveryMode, SourceRef,
};
use cairn_connectors_core::fixture::{AcceptAllConsent, FixtureConnector, default_grant};
use cairn_connectors_core::manifest::ConnectorManifest;
use cairn_connectors_core::webhook::WebhookRequest;
use cairn_connectors_core::{
    ConnectorError, ConnectorRegistry, InMemoryCredentialStore, PipelineEmit,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::Identity;
use cairn_core::domain::capture::CaptureEvent;
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// Binary connector fixture
// ---------------------------------------------------------------------------

/// Canonical TOML for a connector that emits Binary payloads.
const BINARY_MANIFEST_TOML: &str = r#"
[connector]
name = "binary-fixture"
contract = "Connector"
contract_version = "0.1.0"
sensor_identity = "snr:local:connector:binary-fixture:v1"

[capabilities]
poll = true
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
"signature.header" = "X-Binary-Sig"
allowed_mimes = ["application/octet-stream"]

[poll]
cursor_kind = "opaque-string"
min_interval = "30s"
default_interval = "5m"

[payload]
max_bytes = "256KiB"
max_depth = 10
"#;

/// A minimal connector that emits a single Binary payload with a
/// caller-supplied `sha256` hash string.
struct BinaryConnector {
    manifest: ConnectorManifest,
    sensor: Identity,
    /// The `sha256` field value to embed in the Binary payload (must be in
    /// `sha256:<64 lowercase hex>` format).
    sha256: String,
}

impl BinaryConnector {
    /// Build a `BinaryConnector` with `BINARY_MANIFEST_TOML` and the given hash.
    fn with_sha256(sha256: impl Into<String>) -> Self {
        let manifest = ConnectorManifest::parse_toml(BINARY_MANIFEST_TOML)
            .expect("invariant: BINARY_MANIFEST_TOML must parse");
        let sensor = Identity::parse("snr:local:connector:binary-fixture:v1")
            .expect("invariant: binary-fixture sensor identity must parse");
        Self {
            manifest,
            sensor,
            sha256: sha256.into(),
        }
    }

    /// Consent grant covering the `binary-fixture` connector.
    fn grant() -> cairn_core::contract::connector_consent::ConsentGrant {
        let manifest_hash = ConnectorManifest::parse_toml(BINARY_MANIFEST_TOML)
            .expect("invariant: BINARY_MANIFEST_TOML must parse")
            .hash();
        cairn_core::contract::connector_consent::ConsentGrant::new(
            "binary-fixture",
            manifest_hash,
            BTreeSet::from(["note".to_string()]),
            vec!["project:*".to_string()],
            1_700_000_000,
            Identity::parse("hmn:alice").expect("invariant: hmn:alice is a valid Identity"),
        )
    }
}

#[async_trait::async_trait]
impl Connector for BinaryConnector {
    // Returning a literal: the lifetime is tied to `&self` by the trait signature,
    // not by necessity. This is a trait constraint we cannot change.
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "binary-fixture"
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

    async fn poll(&self, _cx: &PollContext) -> Result<PollOutcome, ConnectorError> {
        let event = ConnectorEvent::new(
            ConnectorEventId::new("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
            "binary-fixture",
            SourceRef::new("file", "bucket/key", None),
            1_700_000_000,
            BTreeSet::from(["note".to_string()]),
            ConnectorScope::project("owner/repo"),
            ConnectorPayload::Binary {
                mime: "application/octet-stream".into(),
                sha256: self.sha256.clone(),
                bytes_ref: "sources/spool/01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
                bytes_len: 8,
            },
            DeliveryMode::Poll { cursor: None },
        );
        Ok(PollOutcome {
            events: vec![event],
            next_cursor: None,
            rate_limit_hint: None,
        })
    }

    async fn ingest_webhook(
        &self,
        _req: &WebhookRequest,
        _cx: &WebhookContext,
    ) -> Result<Vec<ConnectorEvent>, ConnectorError> {
        Ok(vec![])
    }
}

impl ConnectorPlugin for BinaryConnector {
    const NAME: &'static str = "binary-fixture";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Records every emitted [`CaptureEvent`] for post-test assertions.
#[derive(Default)]
struct Capturer(Mutex<Vec<CaptureEvent>>);

#[async_trait::async_trait]
impl PipelineEmit for Capturer {
    async fn emit(&self, event: CaptureEvent) -> Result<(), ConnectorError> {
        self.0
            .lock()
            .expect("invariant: Capturer mutex unpoisoned")
            .push(event);
        Ok(())
    }
}

/// Emit that panics if called — used in tests where `emit` must NOT be reached.
struct PanicEmit;

#[async_trait::async_trait]
impl PipelineEmit for PanicEmit {
    async fn emit(&self, _: CaptureEvent) -> Result<(), ConnectorError> {
        panic!("emit must NOT be called — framework gate should have blocked this");
    }
}

// ---------------------------------------------------------------------------
// Finding D — tests
// ---------------------------------------------------------------------------

/// Driving the fixture connector through a full `poll_now` cycle must produce
/// an emitted `CaptureEvent` whose `payload_hash` is NOT the all-zero sentinel
/// and IS the correct SHA-256 of the post-redaction JSON body.
///
/// `FixtureConnector::sample_event` returns a JSON payload:
///   `{"body": "hello"}`
/// The expected hash is `sha256:Sha256(serde_json::to_vec(body))`.
#[tokio::test]
async fn text_payload_emits_real_sha256_hash() {
    let capturer = Arc::new(Capturer::default());
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut registry = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(capturer.clone() as Arc<dyn PipelineEmit>)
        .spool_root(tmp.path().to_path_buf())
        .build();

    registry
        .register(FixtureConnector::with_default_manifest())
        .expect("register must succeed");

    registry
        .enable("fixture", default_grant())
        .await
        .expect("enable must succeed");

    registry
        .poll_now("fixture")
        .await
        .expect("poll_now must succeed");

    registry.shutdown().await;

    let events = capturer
        .0
        .lock()
        .expect("invariant: Capturer mutex unpoisoned");
    assert_eq!(events.len(), 1, "expected exactly one emitted CaptureEvent");

    let hash = events[0].payload_hash.as_str();

    // Must not be the placeholder all-zero hash.
    assert_ne!(
        hash, "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        "payload_hash must not be the all-zero placeholder"
    );

    // Must have the sha256: prefix and a 64-hex-char body.
    assert!(
        hash.starts_with("sha256:"),
        "payload_hash must start with sha256:"
    );
    assert_eq!(
        hash.len(),
        "sha256:".len() + 64,
        "payload_hash must be exactly sha256:<64 hex chars>"
    );

    // The fixture emits `{"body": "hello"}` as JSON. The post-redaction body is
    // `serde_json::to_vec(json!({"body": "hello"}))` — same value since there
    // is no PII to redact.
    let expected_body = serde_json::to_vec(&serde_json::json!({"body": "hello"}))
        .expect("json serialization must succeed");
    let expected_hash = format!("sha256:{}", hex::encode(Sha256::digest(&expected_body)));

    assert_eq!(
        hash, expected_hash,
        "payload_hash must equal sha256 of the post-redaction JSON bytes"
    );
}

/// Binary payloads are rejected by the P0 substrate with a clear error
/// referencing issue #131 (Finding N).
///
/// The spool-verification layer that would cross-check the adapter-declared
/// `sha256` against the actual content at `bytes_ref` is deferred to #131.
/// Until then, `process_event` must return `MalformedPayload` so that
/// adapters producing binary content receive a clear signal to wait for the
/// verified spool API rather than silently spoofing hashes and paths.
///
/// This test replaces the former `binary_payload_uses_adapter_declared_sha256`
/// test, which asserted the opposite (Binary accepted) and is now invalid.
#[tokio::test]
async fn binary_payload_rejected_with_clear_error() {
    // A known 64-char lowercase hex string (sha256 of b"deadbeef").
    let declared_sha256 = format!("sha256:{}", hex::encode(Sha256::digest(b"deadbeef")));

    let tmp = tempfile::tempdir().expect("tempdir");
    let mut registry = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        // PanicEmit: emit must NOT be called — Binary is rejected before emit.
        .emit(Arc::new(PanicEmit))
        .spool_root(tmp.path().to_path_buf())
        .build();

    registry
        .register(BinaryConnector::with_sha256(&declared_sha256))
        .expect("register must succeed");

    registry
        .enable("binary-fixture", BinaryConnector::grant())
        .await
        .expect("enable must succeed");

    // poll_now must fail — Binary payloads are rejected at the substrate boundary
    // until issue #131 provides the spool-verified path (Finding N).
    let err = registry
        .poll_now("binary-fixture")
        .await
        .expect_err("poll_now must fail for Binary payloads (rejected until #131)");

    registry.shutdown().await;

    assert!(
        matches!(err, ConnectorError::MalformedPayload(ref msg) if msg.contains("#131")),
        "expected MalformedPayload mentioning #131, got {err:?}",
    );
}
