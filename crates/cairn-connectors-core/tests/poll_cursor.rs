//! Finding E — Poll cursor must only advance after all events are durably
//! accepted.
//!
//! If any `process_event` call returns an error (e.g., `emit` fails) the
//! cursor must NOT advance for that tick. The next tick must re-fetch the
//! same upstream window (at-least-once delivery).
//!
//! Tests:
//!
//! - `poll_cursor_holds_on_emit_failure` — wire a `Capturer` whose `emit`
//!   returns `Err`; verify that the per-entry cursor stays `None` (initial
//!   value) after a failed `poll_now` tick.
//! - `poll_cursor_advances_on_success` — happy path: verify cursor advances
//!   to the value returned by the connector after a successful `poll_now`.
//!
//! Issue #130, brief §9.1 source sensors (Round-3 review, Finding E).

#![allow(missing_docs)]

use std::collections::BTreeSet;
use std::sync::Arc;

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
// CursorConnector — always returns one event + a fixed next_cursor
// ---------------------------------------------------------------------------

const CURSOR_MANIFEST: &str = r#"
[connector]
name = "cursor-test"
contract = "Connector"
contract_version = "0.1.0"
sensor_identity = "snr:local:connector:cursor-test:v1"

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

fn cursor_manifest() -> ConnectorManifest {
    ConnectorManifest::parse_toml(CURSOR_MANIFEST).expect("CURSOR_MANIFEST must parse")
}

fn cursor_grant() -> cairn_core::contract::connector_consent::ConsentGrant {
    cairn_core::contract::connector_consent::ConsentGrant::new(
        "cursor-test",
        cursor_manifest().hash(),
        BTreeSet::from(["note".to_string()]),
        vec!["project:*".to_string()],
        1_700_000_000,
        Identity::parse("hmn:alice").expect("hmn:alice is valid"),
    )
}

/// Connector that always returns one event and `next_cursor = Some("tick-1")`.
struct CursorConnector {
    manifest: ConnectorManifest,
    sensor: Identity,
}

impl CursorConnector {
    fn new() -> Self {
        Self {
            manifest: cursor_manifest(),
            sensor: Identity::parse("snr:local:connector:cursor-test:v1")
                .expect("cursor-test sensor identity must parse"),
        }
    }
}

#[async_trait]
impl Connector for CursorConnector {
    fn name(&self) -> &'static str {
        "cursor-test"
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
        Ok(PollOutcome {
            events: vec![ConnectorEvent::new(
                ConnectorEventId::new("01ARZ3NDEKTSV4RRFFQ69G5FEE"),
                "cursor-test",
                SourceRef::new("issue", "x", None),
                0,
                BTreeSet::from(["note".to_string()]),
                ConnectorScope::project("owner/repo"),
                ConnectorPayload::Json {
                    mime: "application/json".into(),
                    body: serde_json::json!({"body": "event"}),
                },
                DeliveryMode::Poll { cursor: None },
            )],
            next_cursor: Some("tick-1".to_owned()),
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

impl ConnectorPlugin for CursorConnector {
    const NAME: &'static str = "cursor-test";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
}

// ---------------------------------------------------------------------------
// FailEmit — always returns an error from emit()
// ---------------------------------------------------------------------------

struct FailEmit;

#[async_trait]
impl PipelineEmit for FailEmit {
    async fn emit(&self, _: CaptureEvent) -> Result<(), ConnectorError> {
        Err(ConnectorError::fatal_msg("simulated emit failure"))
    }
}

// ---------------------------------------------------------------------------
// OkEmit — always succeeds
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

/// When `emit` returns an error, `poll_now` must propagate the error AND the
/// per-entry cursor must NOT advance. The cursor remains at its initial value
/// (`None`) so the next tick retries the same upstream window.
///
/// This tests the at-least-once guarantee: a failed emit must not silently
/// skip events by advancing the cursor.
#[tokio::test]
async fn poll_cursor_holds_on_emit_failure() {
    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(Arc::new(FailEmit))
        .build();

    reg.register(CursorConnector::new())
        .expect("register must succeed");
    reg.enable("cursor-test", cursor_grant())
        .await
        .expect("enable must succeed");

    // poll_now must fail because emit() returns an error.
    let err = reg
        .poll_now("cursor-test")
        .await
        .expect_err("poll_now must fail when emit fails");
    assert!(
        matches!(err, ConnectorError::Fatal(_)),
        "expected Fatal from emit failure, got {err:?}",
    );

    // The cursor must still be None — the failed tick must not advance it.
    let cursor = reg
        .entry_cursor("cursor-test")
        .await
        .expect("entry_cursor must return Some for a registered connector");
    assert_eq!(
        cursor, None,
        "cursor must stay None after a failed tick (at-least-once guarantee)"
    );

    reg.shutdown().await;
}

/// When `emit` succeeds, `poll_now` must advance the cursor to the value
/// returned by the connector's `poll()` call (`next_cursor`).
///
/// This is the happy-path control case for the at-least-once guarantee.
#[tokio::test]
async fn poll_cursor_advances_on_success() {
    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(Arc::new(OkEmit))
        .build();

    reg.register(CursorConnector::new())
        .expect("register must succeed");
    reg.enable("cursor-test", cursor_grant())
        .await
        .expect("enable must succeed");

    // Cursor starts at None before any tick.
    let before = reg
        .entry_cursor("cursor-test")
        .await
        .expect("entry_cursor must return Some for a registered connector");
    assert_eq!(before, None, "cursor must be None before first tick");

    // Successful poll_now.
    reg.poll_now("cursor-test")
        .await
        .expect("poll_now must succeed");

    // Cursor must have advanced to the value the connector returned.
    let after = reg
        .entry_cursor("cursor-test")
        .await
        .expect("entry_cursor must return Some for a registered connector");
    assert_eq!(
        after,
        Some("tick-1".to_owned()),
        "cursor must advance to 'tick-1' after a successful tick"
    );

    reg.shutdown().await;
}
