//! Finding A — Manifest budgets enforced by the registry via `RateLimit`.
//!
//! The registry must charge the per-connector `RateLimit` for every event
//! accepted through `process_event`. Events that would exceed the per-hour
//! item budget are rejected with `ConnectorError::BudgetExceeded` and must
//! not reach `PipelineEmit::emit`.
//!
//! Issue #130, brief §9.1 source sensors (Round-2 review, Finding A).

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
// Manifests
// ---------------------------------------------------------------------------

/// Manifest whose budget allows exactly 2 items per hour.
const BUDGET_MANIFEST: &str = r#"
[connector]
name = "budget-test"
contract = "Connector"
contract_version = "0.1.0"
sensor_identity = "snr:local:connector:budget-test:v1"

[capabilities]
poll = true
webhook = false
backfill = false

[oauth]
required_scopes = []
token_lifetime = "1h"
refresh = false

[budget]
max_items_per_hour = 2
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

/// Sibling manifest with a larger budget — used to verify budget isolation.
const SIBLING_MANIFEST: &str = r#"
[connector]
name = "sibling"
contract = "Connector"
contract_version = "0.1.0"
sensor_identity = "snr:local:connector:sibling:v1"

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

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn budget_manifest() -> ConnectorManifest {
    ConnectorManifest::parse_toml(BUDGET_MANIFEST).expect("BUDGET_MANIFEST parses")
}

fn budget_grant() -> cairn_core::contract::connector_consent::ConsentGrant {
    cairn_core::contract::connector_consent::ConsentGrant::new(
        "budget-test",
        budget_manifest().hash(),
        BTreeSet::from(["note".to_string()]),
        vec!["project:*".to_string()],
        1_700_000_000,
        Identity::parse("hmn:alice").expect("invariant: hmn:alice is valid"),
    )
}

fn sibling_manifest() -> ConnectorManifest {
    ConnectorManifest::parse_toml(SIBLING_MANIFEST).expect("SIBLING_MANIFEST parses")
}

fn sibling_grant() -> cairn_core::contract::connector_consent::ConsentGrant {
    cairn_core::contract::connector_consent::ConsentGrant::new(
        "sibling",
        sibling_manifest().hash(),
        BTreeSet::from(["note".to_string()]),
        vec!["project:*".to_string()],
        1_700_000_000,
        Identity::parse("hmn:alice").expect("invariant: hmn:alice is valid"),
    )
}

/// Build a minimal event for the budget-test connector.
fn make_event(id: &str) -> ConnectorEvent {
    ConnectorEvent::new(
        ConnectorEventId::new(id),
        "budget-test",
        SourceRef::new("issue", id, None),
        0,
        BTreeSet::from(["note".to_string()]),
        ConnectorScope::project("owner/repo"),
        ConnectorPayload::Json {
            mime: "application/json".into(),
            body: serde_json::json!({"id": id}),
        },
        DeliveryMode::Poll { cursor: None },
    )
}

/// Recording emit — counts every event that reaches the pipeline.
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
// BudgetConnector — emits a fixed list of events per poll call
// ---------------------------------------------------------------------------

struct BudgetConnector {
    manifest: ConnectorManifest,
    sensor: Identity,
    events: Vec<ConnectorEvent>,
}

impl BudgetConnector {
    fn new(events: Vec<ConnectorEvent>) -> Self {
        Self {
            manifest: budget_manifest(),
            sensor: Identity::parse("snr:local:connector:budget-test:v1")
                .expect("invariant: budget-test sensor identity must parse"),
            events,
        }
    }
}

#[async_trait]
impl Connector for BudgetConnector {
    fn name(&self) -> &'static str {
        "budget-test"
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
            events: self.events.clone(),
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

impl ConnectorPlugin for BudgetConnector {
    const NAME: &'static str = "budget-test";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
}

// ---------------------------------------------------------------------------
// SiblingConnector — used to verify per-connector budget isolation
// ---------------------------------------------------------------------------

struct SiblingConnector {
    manifest: ConnectorManifest,
    sensor: Identity,
}

impl SiblingConnector {
    fn new() -> Self {
        Self {
            manifest: sibling_manifest(),
            sensor: Identity::parse("snr:local:connector:sibling:v1")
                .expect("invariant: sibling sensor identity must parse"),
        }
    }
}

#[async_trait]
impl Connector for SiblingConnector {
    fn name(&self) -> &'static str {
        "sibling"
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
                ConnectorEventId::new("01ARZ3NDEKTSV4RRFFQ69G5FAA"),
                "sibling",
                SourceRef::new("issue", "x", None),
                0,
                BTreeSet::from(["note".to_string()]),
                ConnectorScope::project("owner/repo"),
                ConnectorPayload::Json {
                    mime: "application/json".into(),
                    body: serde_json::json!({}),
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

impl ConnectorPlugin for SiblingConnector {
    const NAME: &'static str = "sibling";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The registry must charge the `RateLimit` for every event and reject with
/// `BudgetExceeded` once the per-hour item budget is exhausted.
///
/// Budget = 2, events emitted by connector = 3 (all same scope).
/// Expected: first 2 events pass → `emit` called twice; 3rd event is rejected
/// with `BudgetExceeded` and `emit` is NOT called for it.
///
/// `poll_now` propagates the first error it encounters, so the call returns
/// `Err(BudgetExceeded)` after processing the 3rd event.
#[tokio::test]
async fn registry_charges_budget_per_event() {
    // Three distinct ULIDs to ensure the events are distinct objects.
    let events = vec![
        make_event("01ARZ3NDEKTSV4RRFFQ69G5FA1"),
        make_event("01ARZ3NDEKTSV4RRFFQ69G5FA2"),
        make_event("01ARZ3NDEKTSV4RRFFQ69G5FA3"),
    ];

    let emit = Arc::new(CountEmit::default());

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(emit.clone())
        .build();

    reg.register(BudgetConnector::new(events))
        .expect("register must succeed");
    reg.enable("budget-test", budget_grant())
        .await
        .expect("enable must succeed");

    // poll_now must return an error because the 3rd event exceeds the budget.
    let err = reg
        .poll_now("budget-test")
        .await
        .expect_err("poll_now must fail when budget is exceeded");

    assert!(
        matches!(err, ConnectorError::BudgetExceeded { .. }),
        "expected BudgetExceeded, got {err:?}",
    );

    // The first 2 events must have been emitted; the 3rd must not.
    let count = *emit.0.lock().expect("mutex unpoisoned");
    assert_eq!(
        count, 2,
        "exactly 2 events must reach emit before budget is exhausted (got {count})",
    );

    reg.shutdown().await;
}

/// Events within the budget must be emitted without error.
///
/// Budget = 2, events emitted = 2 → both must succeed.
#[tokio::test]
async fn events_within_budget_are_emitted() {
    let events = vec![
        make_event("01ARZ3NDEKTSV4RRFFQ69G5FA1"),
        make_event("01ARZ3NDEKTSV4RRFFQ69G5FA2"),
    ];

    let emit = Arc::new(CountEmit::default());

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(emit.clone())
        .build();

    reg.register(BudgetConnector::new(events))
        .expect("register must succeed");
    reg.enable("budget-test", budget_grant())
        .await
        .expect("enable must succeed");

    reg.poll_now("budget-test")
        .await
        .expect("poll_now must succeed when budget is not exceeded");

    let count = *emit.0.lock().expect("mutex unpoisoned");
    assert_eq!(
        count, 2,
        "both events within budget must reach emit (got {count})",
    );

    reg.shutdown().await;
}

/// After a budget is exhausted for one connector, a second independently
/// registered connector with its own budget is unaffected.
///
/// This validates that budgets are per-connector, not global.
#[tokio::test]
async fn budget_is_per_connector_not_global() {
    let emit = Arc::new(CountEmit::default());
    let consent = Arc::new(AcceptAllConsent::default());

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(consent)
        .emit(emit.clone())
        .build();

    // Register budget-test (budget=2) with 3 events — will exceed budget.
    reg.register(BudgetConnector::new(vec![
        make_event("01ARZ3NDEKTSV4RRFFQ69G5FA1"),
        make_event("01ARZ3NDEKTSV4RRFFQ69G5FA2"),
        make_event("01ARZ3NDEKTSV4RRFFQ69G5FA3"),
    ]))
    .expect("register budget-test must succeed");

    // Register sibling (budget=10) — must be unaffected by budget-test exhaustion.
    reg.register(SiblingConnector::new())
        .expect("register sibling must succeed");

    reg.enable("budget-test", budget_grant())
        .await
        .expect("enable budget-test must succeed");
    reg.enable("sibling", sibling_grant())
        .await
        .expect("enable sibling must succeed");

    // budget-test exhausts its budget at event 3 — ignore the error.
    let _ = reg.poll_now("budget-test").await;

    // Sibling's poll must succeed independently.
    reg.poll_now("sibling")
        .await
        .expect("sibling poll_now must succeed despite budget-test being exhausted");

    reg.shutdown().await;
}
