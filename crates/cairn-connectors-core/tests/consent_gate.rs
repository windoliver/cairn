//! T20b — `ConsentRevoked` gate, Finding F scope-pattern enforcement.
//!
//! `DenyAllJournal` always returns `ConnectorConsentLookup::Revoked`. The
//! framework must reject the emit with `ConnectorError::ConsentRevoked` and
//! must never call `PipelineEmit::emit`.
//!
//! Finding F: grant `scope_patterns` must be enforced in `process_event`.
//! A grant with `scope_patterns = ["project:specific"]` must reject events
//! emitting into `project:other` even when the manifest declares `pattern = "*"`.
//!
//! Issue #130, brief §14 consent gate.

#![allow(missing_docs)]

use std::collections::BTreeSet;
use std::sync::Arc;

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
use cairn_core::contract::connector_consent::{
    ConnectorConsentJournal, ConnectorConsentLookup, ConsentGrant, ConsentGrantId,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::Identity;
use cairn_core::domain::capture::CaptureEvent;

// ---------------------------------------------------------------------------
// DenyAllJournal — always returns Revoked
// ---------------------------------------------------------------------------

#[derive(Default)]
struct DenyAllJournal;

#[async_trait::async_trait]
impl ConnectorConsentJournal for DenyAllJournal {
    async fn put_grant(&self, _: ConsentGrant) -> Result<ConsentGrantId, String> {
        Ok(ConsentGrantId::new("gnt:x"))
    }

    async fn lookup(
        &self,
        _connector: &str,
        _scope_key: &str,
    ) -> Result<ConnectorConsentLookup, String> {
        Ok(ConnectorConsentLookup::Revoked)
    }

    async fn revoke(&self, _: &ConsentGrantId) -> Result<(), String> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// PanicEmit — test guard: emit must never be called when consent is revoked
// ---------------------------------------------------------------------------

struct PanicEmit;

#[async_trait::async_trait]
impl PipelineEmit for PanicEmit {
    async fn emit(&self, _: CaptureEvent) -> Result<(), ConnectorError> {
        panic!("emit must NOT be called when consent is revoked");
    }
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn consent_revoked_blocks_emit() {
    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(DenyAllJournal))
        .emit(Arc::new(PanicEmit) as Arc<dyn PipelineEmit>)
        .build();

    reg.register(FixtureConnector::with_default_manifest())
        .expect("register must succeed");
    reg.enable("fixture", default_grant())
        .await
        .expect("enable must succeed");

    let err = reg
        .poll_now("fixture")
        .await
        .expect_err("poll_now must fail when consent is revoked");

    assert!(
        matches!(err, ConnectorError::ConsentRevoked { ref connector } if connector == "fixture"),
        "expected ConsentRevoked{{connector: \"fixture\"}}, got {err:?}",
    );

    reg.shutdown().await;
}

// ---------------------------------------------------------------------------
// Finding F — grant scope_patterns must be enforced
// ---------------------------------------------------------------------------

/// Connector manifest that declares `scopes.declared = [{kind: "project", pattern: "*"}]`.
/// The grant will be issued with `scope_patterns = ["project:specific"]`, which
/// is narrower than the manifest's wildcard declaration.
const SCOPE_NARROW_MANIFEST: &str = r#"
[connector]
name = "scope-narrow"
contract = "Connector"
contract_version = "0.1.0"
sensor_identity = "snr:local:connector:scope-narrow:v1"

[capabilities]
poll = false
webhook = false
backfill = false

[oauth]
required_scopes = []
token_lifetime = "1h"
refresh = false

[budget]
max_items_per_hour = 1000
max_bytes_per_day = "1GiB"

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
max_depth = 4
"#;

struct ScopeNarrowConnector {
    manifest: ConnectorManifest,
    sensor: Identity,
    /// The scope value this connector will emit into.
    emit_scope_value: String,
}

impl ScopeNarrowConnector {
    fn new(emit_scope_value: impl Into<String>) -> Self {
        let manifest = ConnectorManifest::parse_toml(SCOPE_NARROW_MANIFEST)
            .expect("SCOPE_NARROW_MANIFEST must parse");
        let sensor = Identity::parse("snr:local:connector:scope-narrow:v1")
            .expect("scope-narrow sensor identity must parse");
        Self {
            manifest,
            sensor,
            emit_scope_value: emit_scope_value.into(),
        }
    }
}

#[async_trait::async_trait]
impl Connector for ScopeNarrowConnector {
    fn name(&self) -> &'static str {
        "scope-narrow"
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
        &self.sensor
    }

    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
    }

    async fn poll(&self, _cx: &PollContext) -> Result<PollOutcome, ConnectorError> {
        Ok(PollOutcome {
            events: vec![ConnectorEvent::new(
                ConnectorEventId::new("01ARZ3NDEKTSV4RRFFQ69G5FGG"),
                "scope-narrow",
                SourceRef::new("issue", "x", None),
                0,
                BTreeSet::from(["note".to_string()]),
                ConnectorScope::project(&self.emit_scope_value),
                ConnectorPayload::Json {
                    mime: "application/json".into(),
                    body: serde_json::json!({"body": "event"}),
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

impl ConnectorPlugin for ScopeNarrowConnector {
    const NAME: &'static str = "scope-narrow";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
}

/// A grant with `scope_patterns = ["project:specific"]` must block an event
/// emitted into `project:other`, even though the manifest declares
/// `scopes.declared = [{kind: "project", pattern: "*"}]`.
///
/// This tests Finding F: grant `scope_patterns` must be enforced, not just the
/// manifest scope declarations.
#[tokio::test]
async fn grant_scope_narrowing_blocks_other_scopes() {
    let manifest = ConnectorManifest::parse_toml(SCOPE_NARROW_MANIFEST)
        .expect("SCOPE_NARROW_MANIFEST must parse");

    // Grant covers ONLY "project:specific".
    let narrow_grant = ConsentGrant::new(
        "scope-narrow",
        manifest.hash(),
        BTreeSet::from(["note".to_string()]),
        vec!["project:specific".to_string()], // <-- narrow scope
        1_700_000_000,
        Identity::parse("hmn:alice").expect("hmn:alice is valid"),
    );

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(Arc::new(PanicEmit) as Arc<dyn PipelineEmit>)
        .build();

    // The connector will emit into "project:other" — outside the grant.
    reg.register(ScopeNarrowConnector::new("other"))
        .expect("register must succeed");
    reg.enable("scope-narrow", narrow_grant)
        .await
        .expect("enable must succeed");

    let err = reg
        .poll_now("scope-narrow")
        .await
        .expect_err("poll_now must fail: scope does not match grant scope_patterns");

    assert!(
        matches!(err, ConnectorError::ConsentRevoked { ref connector } if connector == "scope-narrow"),
        "expected ConsentRevoked, got {err:?}",
    );

    reg.shutdown().await;
}

// ---------------------------------------------------------------------------
// Recording emit for the `grant_scope_narrowing_allows_matching_scope` test.
// ---------------------------------------------------------------------------

/// Records the count of emitted events for scope-narrowing tests.
struct RecordEmit(std::sync::Mutex<usize>);

#[async_trait::async_trait]
impl PipelineEmit for RecordEmit {
    async fn emit(&self, _: CaptureEvent) -> Result<(), ConnectorError> {
        *self
            .0
            .lock()
            .expect("invariant: RecordEmit mutex unpoisoned") += 1;
        Ok(())
    }
}

/// A grant with `scope_patterns = ["project:specific"]` must ALLOW an event
/// emitted into `project:specific`.
#[tokio::test]
async fn grant_scope_narrowing_allows_matching_scope() {
    let manifest = ConnectorManifest::parse_toml(SCOPE_NARROW_MANIFEST)
        .expect("SCOPE_NARROW_MANIFEST must parse");

    let narrow_grant = ConsentGrant::new(
        "scope-narrow",
        manifest.hash(),
        BTreeSet::from(["note".to_string()]),
        vec!["project:specific".to_string()],
        1_700_000_000,
        Identity::parse("hmn:alice").expect("hmn:alice is valid"),
    );

    let recorder = Arc::new(RecordEmit(std::sync::Mutex::new(0)));
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(recorder.clone() as Arc<dyn PipelineEmit>)
        .spool_root(tmp.path().to_path_buf())
        .build();

    // The connector emits into "project:specific" — matches the grant.
    reg.register(ScopeNarrowConnector::new("specific"))
        .expect("register must succeed");
    reg.enable("scope-narrow", narrow_grant)
        .await
        .expect("enable must succeed");

    reg.poll_now("scope-narrow")
        .await
        .expect("poll_now must succeed: scope matches grant scope_patterns");

    let count = *recorder
        .0
        .lock()
        .expect("invariant: RecordEmit mutex unpoisoned");
    assert_eq!(
        count, 1,
        "exactly 1 event must be emitted for the matching scope"
    );

    reg.shutdown().await;
}
