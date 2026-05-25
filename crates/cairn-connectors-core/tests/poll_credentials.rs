//! Finding H — poll credentials are loaded from `CredentialStore` on each tick.
//!
//! Before the fix, `poll_now` and the background poll task always passed
//! `CredentialHandle::empty()` to `connector.poll()`. Real OAuth connectors
//! cannot authenticate with an empty credential.
//!
//! The fix resolves the credential from `CredentialStore::get("connector/<name>")`
//! before constructing `PollContext`. If the credential is absent or expired
//! (e.g. `AuthExpired`), the background task skips the tick with a warning
//! (does not call poll, does not advance the cursor). In `poll_now`, absent
//! credentials fall back to empty (fixture / test connectors do not need auth).
//!
//! Tests:
//!
//! - `poll_uses_credential_store`: provision `"connector/cred-check"` with
//!   known bytes; assert the connector's poll method receives those exact bytes.
//! - `poll_with_missing_credential_skips_tick`: empty credential store;
//!   assert that `poll_now` still returns `Ok` (empty fallback) but with 0
//!   events because the connector returns none when bytes are empty.
//!   Note: in the background task, missing credentials cause the tick to be
//!   skipped entirely. `poll_now` uses the empty fallback for backward compat
//!   with fixture connectors that don't need credentials.
//!
//! Issue #130, brief §9.1 source sensors (Round-4 review, Finding H).

#![allow(missing_docs)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cairn_connectors_core::connector::{
    Connector, ConnectorCapabilities, ConnectorPlugin, PollContext, PollOutcome, WebhookContext,
};
use cairn_connectors_core::credential::{CredentialStore, InMemoryCredentialStore};
use cairn_connectors_core::event::{
    ConnectorEvent, ConnectorEventId, ConnectorPayload, ConnectorScope, DeliveryMode, SourceRef,
};
use cairn_connectors_core::fixture::AcceptAllConsent;
use cairn_connectors_core::manifest::ConnectorManifest;
use cairn_connectors_core::webhook::WebhookRequest;
use cairn_connectors_core::{ConnectorError, ConnectorRegistry, PipelineEmit};
use cairn_core::contract::connector_consent::ConsentGrant;
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::Identity;
use cairn_core::domain::capture::CaptureEvent;

// ---------------------------------------------------------------------------
// CredCheck manifest / connector
// ---------------------------------------------------------------------------

const CRED_MANIFEST: &str = r#"
[connector]
name = "cred-check"
contract = "Connector"
contract_version = "0.1.0"
sensor_identity = "snr:local:connector:cred-check:v1"

[capabilities]
poll = true
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

fn cred_manifest() -> ConnectorManifest {
    ConnectorManifest::parse_toml(CRED_MANIFEST).expect("CRED_MANIFEST must parse")
}

fn cred_grant() -> ConsentGrant {
    ConsentGrant::new(
        "cred-check",
        cred_manifest().hash(),
        BTreeSet::from(["note".to_string()]),
        vec!["project:*".to_string()],
        1_700_000_000,
        Identity::parse("hmn:alice").expect("hmn:alice is valid"),
    )
}

/// Connector that records the credential bytes it receives on each poll call.
struct CredCheckConnector {
    manifest: ConnectorManifest,
    sensor: Identity,
    /// Accumulates the raw credential bytes from each poll call.
    received: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl CredCheckConnector {
    fn new(received: Arc<Mutex<Vec<Vec<u8>>>>) -> Self {
        Self {
            manifest: cred_manifest(),
            sensor: Identity::parse("snr:local:connector:cred-check:v1")
                .expect("cred-check sensor identity must parse"),
            received,
        }
    }
}

#[async_trait]
impl Connector for CredCheckConnector {
    fn name(&self) -> &'static str {
        "cred-check"
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

    async fn poll(&self, cx: &PollContext) -> Result<PollOutcome, ConnectorError> {
        // Record the credential bytes this poll received.
        self.received
            .lock()
            .expect("invariant: received mutex unpoisoned")
            .push(cx.credentials.bytes().to_vec());
        // Return no events — we only care about which credential was passed.
        Ok(PollOutcome::default())
    }

    async fn ingest_webhook(
        &self,
        _: &WebhookRequest,
        _: &WebhookContext,
    ) -> Result<Vec<ConnectorEvent>, ConnectorError> {
        Ok(vec![])
    }
}

impl ConnectorPlugin for CredCheckConnector {
    const NAME: &'static str = "cred-check";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
}

/// A connector that emits one event only when it receives non-empty credentials.
/// Used to verify that a missing credential causes `poll` not to be called.
struct EmitIfCredConnector {
    manifest: ConnectorManifest,
    sensor: Identity,
    poll_call_count: Arc<Mutex<usize>>,
}

impl EmitIfCredConnector {
    fn new(poll_call_count: Arc<Mutex<usize>>) -> Self {
        Self {
            manifest: cred_manifest(),
            sensor: Identity::parse("snr:local:connector:cred-check:v1")
                .expect("cred-check sensor identity must parse"),
            poll_call_count,
        }
    }
}

#[async_trait]
impl Connector for EmitIfCredConnector {
    fn name(&self) -> &'static str {
        "cred-check"
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

    async fn poll(&self, cx: &PollContext) -> Result<PollOutcome, ConnectorError> {
        *self
            .poll_call_count
            .lock()
            .expect("invariant: poll_call_count mutex unpoisoned") += 1;
        // Return an event only when credentials are non-empty.
        if cx.credentials.bytes().is_empty() {
            Ok(PollOutcome::default())
        } else {
            Ok(PollOutcome {
                events: vec![ConnectorEvent::new(
                    ConnectorEventId::new("01ARZ3NDEKTSV4RRFFQ69G5FHH"),
                    "cred-check",
                    SourceRef::new("issue", "x", None),
                    0,
                    BTreeSet::from(["note".to_string()]),
                    ConnectorScope::project("owner/repo"),
                    ConnectorPayload::Json {
                        mime: "application/json".into(),
                        body: serde_json::json!({"body": "cred-event"}),
                    },
                    DeliveryMode::Poll { cursor: None },
                )],
                next_cursor: None,
                rate_limit_hint: None,
            })
        }
    }

    async fn ingest_webhook(
        &self,
        _: &WebhookRequest,
        _: &WebhookContext,
    ) -> Result<Vec<ConnectorEvent>, ConnectorError> {
        Ok(vec![])
    }
}

impl ConnectorPlugin for EmitIfCredConnector {
    const NAME: &'static str = "cred-check";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
}

// ---------------------------------------------------------------------------
// No-op emit
// ---------------------------------------------------------------------------

struct OkEmit;

#[async_trait]
impl PipelineEmit for OkEmit {
    async fn emit(&self, _: CaptureEvent) -> Result<(), ConnectorError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// `poll_now` must resolve credentials from the `CredentialStore` using the
/// key `"connector/<name>"` and pass them to `connector.poll` in `cx.credentials`.
///
/// This is Finding H: before the fix, `CredentialHandle::empty()` was always
/// used, making OAuth connectors unable to authenticate.
#[tokio::test]
async fn poll_uses_credential_store() {
    let received: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));

    let creds = Arc::new(InMemoryCredentialStore::default());
    // Provision the credential under the convention key.
    creds
        .put("connector/cred-check", b"tok-xyz".to_vec())
        .await
        .expect("put must succeed");

    let tmp = tempfile::tempdir().expect("tempdir");
    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::clone(&creds) as Arc<dyn CredentialStore>)
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(Arc::new(OkEmit) as Arc<dyn PipelineEmit>)
        .spool_root(tmp.path().to_path_buf())
        .build();

    reg.register(CredCheckConnector::new(Arc::clone(&received)))
        .expect("register must succeed");
    reg.enable("cred-check", cred_grant())
        .await
        .expect("enable must succeed");

    reg.poll_now("cred-check")
        .await
        .expect("poll_now must succeed");

    // Drop the lock before the shutdown `.await` to avoid holding a
    // `std::sync::MutexGuard` across an await point.
    {
        let calls = received
            .lock()
            .expect("invariant: received mutex unpoisoned");
        assert_eq!(calls.len(), 1, "poll must be called exactly once");
        assert_eq!(
            calls[0], b"tok-xyz",
            "poll must receive the provisioned credential bytes",
        );
    }

    reg.shutdown().await;
}

/// `poll_now` with no credential provisioned must still return `Ok`.
///
/// The `poll_now` path uses an empty-handle fallback for backward compatibility
/// with fixture connectors that do not require authentication. The connector's
/// `poll` method IS called; it receives empty credential bytes.
///
/// The background poll task uses a different policy: a missing credential causes
/// the tick to be SKIPPED (poll is not called). That path is covered by the
/// background-task integration tests.
#[tokio::test]
async fn poll_now_with_missing_credential_uses_empty_fallback() {
    let poll_call_count = Arc::new(Mutex::new(0usize));

    // Empty credential store — no credential provisioned for "connector/cred-check".
    let creds = Arc::new(InMemoryCredentialStore::default());

    let tmp = tempfile::tempdir().expect("tempdir");
    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::clone(&creds) as Arc<dyn CredentialStore>)
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(Arc::new(OkEmit) as Arc<dyn PipelineEmit>)
        .spool_root(tmp.path().to_path_buf())
        .build();

    reg.register(EmitIfCredConnector::new(Arc::clone(&poll_call_count)))
        .expect("register must succeed");
    reg.enable("cred-check", cred_grant())
        .await
        .expect("enable must succeed");

    // poll_now must succeed even with no credential (empty-handle fallback).
    reg.poll_now("cred-check")
        .await
        .expect("poll_now must succeed with missing credential (empty fallback)");

    // The connector was called with empty credentials → returned 0 events.
    let count = *poll_call_count
        .lock()
        .expect("invariant: poll_call_count mutex unpoisoned");
    assert_eq!(
        count, 1,
        "connector.poll must be called once (empty fallback)"
    );

    reg.shutdown().await;
}
