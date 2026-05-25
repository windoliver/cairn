//! Finding J — path-traversal via unvalidated `event_id`.
//!
//! `ConnectorEventId::parse` must reject any string that is not a valid 26-char
//! Crockford base32 ULID, and `process_event` must call it BEFORE writing to the
//! spool. A malicious connector supplying `event_id = "../../etc/passwd"` must
//! never produce a spool file outside the spool subtree.
//!
//! Issue #130, brief §9.1, Round-5 review Finding J.

#![allow(missing_docs)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
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
// Shared manifest (poll only, budget = 10)
// ---------------------------------------------------------------------------

const MANIFEST: &str = r#"
[connector]
name = "id-test"
contract = "Connector"
contract_version = "0.1.0"
sensor_identity = "snr:local:connector:id-test:v1"

[capabilities]
poll = true
webhook = false
backfill = false

[oauth]
required_scopes = []
token_lifetime = "1h"
refresh = false

[budget]
max_items_per_hour = 10
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
"#;

fn make_manifest() -> ConnectorManifest {
    ConnectorManifest::parse_toml(MANIFEST).expect("MANIFEST parses")
}

fn make_grant() -> cairn_core::contract::connector_consent::ConsentGrant {
    cairn_core::contract::connector_consent::ConsentGrant::new(
        "id-test",
        make_manifest().hash(),
        BTreeSet::from(["note".to_string()]),
        vec!["project:*".to_string()],
        1_700_000_000,
        Identity::parse("hmn:alice").expect("invariant"),
    )
}

// ---------------------------------------------------------------------------
// Connector that emits whatever event_id the test supplies
// ---------------------------------------------------------------------------

struct IdTestConnector {
    manifest: ConnectorManifest,
    sensor: Identity,
    event_id: String,
}

impl IdTestConnector {
    fn new(event_id: impl Into<String>) -> Self {
        Self {
            manifest: make_manifest(),
            sensor: Identity::parse("snr:local:connector:id-test:v1").expect("sensor identity"),
            event_id: event_id.into(),
        }
    }
}

#[async_trait]
impl Connector for IdTestConnector {
    fn name(&self) -> &'static str {
        "id-test"
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
        Ok(PollOutcome {
            events: vec![ConnectorEvent::new(
                ConnectorEventId::new(self.event_id.clone()),
                "id-test",
                SourceRef::new("issue", "x", None),
                0,
                BTreeSet::from(["note".to_string()]),
                ConnectorScope::project("owner/repo"),
                ConnectorPayload::Json {
                    mime: "application/json".into(),
                    body: serde_json::json!({"id": self.event_id}),
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

impl ConnectorPlugin for IdTestConnector {
    const NAME: &'static str = "id-test";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
}

// ---------------------------------------------------------------------------
// Counting emit — verifies whether emit was called
// ---------------------------------------------------------------------------

#[derive(Default)]
struct CountEmit(StdMutex<usize>);

#[async_trait]
impl PipelineEmit for CountEmit {
    async fn emit(&self, _: CaptureEvent) -> Result<(), ConnectorError> {
        *self.0.lock().expect("mutex unpoisoned") += 1;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Unit tests for ConnectorEventId::parse
// ---------------------------------------------------------------------------

/// `ConnectorEventId::parse` must accept a valid 26-char uppercase ULID.
#[test]
fn parse_valid_ulid_accepted() {
    // A canonical ULID produced by `ulid::Ulid::new()` during development.
    let result = ConnectorEventId::parse("01HX0000000000000000000000");
    assert!(
        result.is_ok(),
        "valid ULID must be accepted by parse, got {result:?}",
    );
    let id = result.unwrap();
    assert_eq!(id.as_str(), "01HX0000000000000000000000");
}

/// `ConnectorEventId::parse` must accept lowercase ULIDs (case-insensitive).
#[test]
fn parse_lowercase_ulid_accepted() {
    let result = ConnectorEventId::parse("01hx0000000000000000000000");
    assert!(
        result.is_ok(),
        "lowercase ULID must be accepted by parse (case-insensitive), got {result:?}",
    );
}

/// `ConnectorEventId::parse` must reject path traversal strings.
#[test]
fn parse_path_traversal_rejected() {
    let result = ConnectorEventId::parse("../../etc/passwd");
    assert!(
        result.is_err(),
        "path traversal event_id must be rejected by parse",
    );
    let err = result.unwrap_err();
    assert!(
        matches!(err, ConnectorError::MalformedPayload(_)),
        "expected MalformedPayload, got {err:?}",
    );
}

/// `ConnectorEventId::parse` must reject arbitrary non-ULID strings.
#[test]
fn parse_non_ulid_rejected() {
    let result = ConnectorEventId::parse("not-a-ulid");
    assert!(result.is_err(), "non-ULID string must be rejected by parse");
    let err = result.unwrap_err();
    assert!(
        matches!(err, ConnectorError::MalformedPayload(_)),
        "expected MalformedPayload, got {err:?}",
    );
}

/// `ConnectorEventId::parse` must reject strings with null bytes.
#[test]
fn parse_null_byte_rejected() {
    let result = ConnectorEventId::parse("01HX000000000\x000000000000");
    assert!(
        result.is_err(),
        "event_id with null byte must be rejected by parse",
    );
}

/// `ConnectorEventId::parse` must reject empty string.
#[test]
fn parse_empty_string_rejected() {
    let result = ConnectorEventId::parse("");
    assert!(result.is_err(), "empty string must be rejected by parse");
}

// ---------------------------------------------------------------------------
// Integration tests: process_event validates event_id before spool write
// ---------------------------------------------------------------------------

/// A path-traversal `event_id` must be rejected by `poll_now` before any spool
/// file is created and before `emit` is called.
#[tokio::test]
async fn path_traversal_event_id_rejected() {
    let emit = Arc::new(CountEmit::default());
    let tmp = tempfile::tempdir().expect("tempdir");

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(emit.clone() as Arc<dyn PipelineEmit>)
        .spool_root(tmp.path().to_path_buf())
        .build();

    reg.register(IdTestConnector::new("../../etc/passwd"))
        .expect("register must succeed");
    reg.enable("id-test", make_grant())
        .await
        .expect("enable must succeed");

    let err = reg
        .poll_now("id-test")
        .await
        .expect_err("path-traversal event_id must be rejected");

    assert!(
        matches!(err, ConnectorError::MalformedPayload(_)),
        "expected MalformedPayload for path-traversal event_id, got {err:?}",
    );

    // Emit must NOT have been called.
    let count = *emit.0.lock().expect("mutex unpoisoned");
    assert_eq!(
        count, 0,
        "emit must not be called when event_id is invalid (got {count})",
    );

    // No spool file should exist under the spool root.
    let connector_spool = tmp.path().join("connector").join("id-test");
    if connector_spool.exists() {
        let entries: Vec<_> = std::fs::read_dir(&connector_spool)
            .expect("read_dir")
            .collect();
        assert!(
            entries.is_empty(),
            "no spool files must be created for invalid event_id (found {entries:?})",
        );
    }

    reg.shutdown().await;
}

/// A non-ULID `event_id` (not path-traversal, just malformed) must also be
/// rejected before emit is called.
#[tokio::test]
async fn non_ulid_event_id_rejected() {
    let emit = Arc::new(CountEmit::default());
    let tmp = tempfile::tempdir().expect("tempdir");

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(emit.clone() as Arc<dyn PipelineEmit>)
        .spool_root(tmp.path().to_path_buf())
        .build();

    reg.register(IdTestConnector::new("not-a-ulid"))
        .expect("register must succeed");
    reg.enable("id-test", make_grant())
        .await
        .expect("enable must succeed");

    let err = reg
        .poll_now("id-test")
        .await
        .expect_err("non-ULID event_id must be rejected");

    assert!(
        matches!(err, ConnectorError::MalformedPayload(_)),
        "expected MalformedPayload for non-ULID event_id, got {err:?}",
    );

    let count = *emit.0.lock().expect("mutex unpoisoned");
    assert_eq!(count, 0, "emit must not be called for non-ULID event_id");

    reg.shutdown().await;
}

/// A valid ULID `event_id` must pass validation and reach `emit` normally.
#[tokio::test]
async fn valid_ulid_event_id_accepted() {
    let emit = Arc::new(CountEmit::default());
    let tmp = tempfile::tempdir().expect("tempdir");

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(emit.clone() as Arc<dyn PipelineEmit>)
        .spool_root(tmp.path().to_path_buf())
        .build();

    reg.register(IdTestConnector::new("01HX0000000000000000000000"))
        .expect("register must succeed");
    reg.enable("id-test", make_grant())
        .await
        .expect("enable must succeed");

    reg.poll_now("id-test")
        .await
        .expect("valid ULID event_id must be accepted");

    let count = *emit.0.lock().expect("mutex unpoisoned");
    assert_eq!(
        count, 1,
        "exactly 1 event must be emitted for valid event_id (got {count})",
    );

    reg.shutdown().await;
}
