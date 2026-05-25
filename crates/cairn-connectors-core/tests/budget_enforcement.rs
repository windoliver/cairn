//! Finding A + Finding G — Manifest budgets enforced by the registry via
//! `RateLimit`.
//!
//! Finding A: the registry must charge the per-connector `RateLimit` for every
//! event accepted through `process_event`. Events that exceed the per-hour item
//! budget are rejected with `ConnectorError::BudgetExceeded`.
//!
//! Finding G fix: the rate-limit charge is now performed AFTER `emit.emit`
//! succeeds. This means a failed emit does NOT permanently consume a budget
//! token — the event can be retried and the budget remains available.
//!
//! Issue #130, brief §9.1 source sensors (Round-2 review, Finding A;
//! Round-4 review, Finding G).

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
///
/// Finding G fix: the rate-limit charge happens AFTER `emit` succeeds, so
/// all 3 events are forwarded to the pipeline before the charge for the 3rd
/// event fails. The token is only consumed on durable acceptance, so a
/// transient emit failure does not permanently reduce available budget.
///
/// Expected: all 3 events reach `emit` (count = 3); `poll_now` returns
/// `Err(BudgetExceeded)` because the charge for the 3rd event fails after
/// emit (budget was already exhausted by the first 2 charges).
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
    let tmp = tempfile::tempdir().expect("tempdir");

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(emit.clone())
        .spool_root(tmp.path().to_path_buf())
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

    // All 3 events are emitted BEFORE the 3rd charge fails (Finding G fix).
    let count = *emit.0.lock().expect("mutex unpoisoned");
    assert_eq!(
        count, 3,
        "all 3 events must reach emit; budget is charged after emit succeeds (got {count})",
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
    let tmp = tempfile::tempdir().expect("tempdir");

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(emit.clone())
        .spool_root(tmp.path().to_path_buf())
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
    let tmp = tempfile::tempdir().expect("tempdir");

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(consent)
        .emit(emit.clone())
        .spool_root(tmp.path().to_path_buf())
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

// ---------------------------------------------------------------------------
// Finding G — failed emit must not consume a budget token
// ---------------------------------------------------------------------------

/// Manifest with budget = 1 item per hour, used by `failed_emit_does_not_consume_budget`.
const BUDGET_ONE_MANIFEST: &str = r#"
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
max_items_per_hour = 1
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

/// A connector that emits a single hardcoded event per poll call, using
/// the `BUDGET_ONE_MANIFEST` (budget = 1).
struct OneItemConnector {
    manifest: ConnectorManifest,
    sensor: Identity,
    event_id: &'static str,
}

impl OneItemConnector {
    fn new(event_id: &'static str) -> Self {
        let manifest = ConnectorManifest::parse_toml(BUDGET_ONE_MANIFEST)
            .expect("BUDGET_ONE_MANIFEST must parse");
        let sensor = Identity::parse("snr:local:connector:budget-test:v1")
            .expect("sensor identity must parse");
        Self {
            manifest,
            sensor,
            event_id,
        }
    }
}

#[async_trait]
impl Connector for OneItemConnector {
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
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
    }
    async fn poll(&self, _: &PollContext) -> Result<PollOutcome, ConnectorError> {
        Ok(PollOutcome {
            events: vec![ConnectorEvent::new(
                ConnectorEventId::new(self.event_id),
                "budget-test",
                SourceRef::new("issue", self.event_id, None),
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

impl ConnectorPlugin for OneItemConnector {
    const NAME: &'static str = "budget-test";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
}

/// Build a consent grant for the `BUDGET_ONE_MANIFEST` connector.
fn one_grant() -> cairn_core::contract::connector_consent::ConsentGrant {
    let manifest =
        ConnectorManifest::parse_toml(BUDGET_ONE_MANIFEST).expect("BUDGET_ONE_MANIFEST must parse");
    cairn_core::contract::connector_consent::ConsentGrant::new(
        "budget-test",
        manifest.hash(),
        BTreeSet::from(["note".to_string()]),
        vec!["project:*".to_string()],
        1_700_000_000,
        Identity::parse("hmn:alice").expect("invariant: hmn:alice is valid"),
    )
}

/// A `PipelineEmit` implementation that fails on the first call and succeeds
/// on all subsequent calls, tracking the total calls made.
#[derive(Default)]
struct OnceFailEmit {
    /// Count of calls to `emit`.
    calls: StdMutex<usize>,
}

#[async_trait]
impl PipelineEmit for OnceFailEmit {
    async fn emit(&self, _: CaptureEvent) -> Result<(), ConnectorError> {
        let mut calls = self.calls.lock().expect("mutex unpoisoned");
        *calls += 1;
        let n = *calls;
        if n == 1 {
            // First call always fails (simulates a transient downstream error).
            Err(ConnectorError::transient_msg(
                "simulated transient emit failure",
            ))
        } else {
            Ok(())
        }
    }
}

/// When `emit` returns an error, the budget token must NOT be consumed
/// (Finding G fix: charge AFTER emit).
///
/// Setup:
/// - Budget = 1 item per hour.
/// - `OnceFailEmit`: first `emit` call returns `Err(Transient)`, all subsequent
///   calls return `Ok`.
///
/// Step 1: `poll_now` → emit fails → budget NOT consumed → `poll_now` returns
///         `Err(Transient)` (from emit), NOT `BudgetExceeded`.
/// Step 2: disable + re-enable; emit now succeeds → budget IS consumed.
/// Step 3: disable + re-enable; budget is now zero → `poll_now` returns
///         `Err(BudgetExceeded)`.
#[allow(clippy::too_many_lines)] // sequential steps; splitting would obscure the narrative
#[tokio::test]
async fn failed_emit_does_not_consume_budget() {
    let fail_emit = Arc::new(OnceFailEmit::default());
    let tmp = tempfile::tempdir().expect("tempdir");

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(fail_emit.clone() as Arc<dyn PipelineEmit>)
        .spool_root(tmp.path().to_path_buf())
        .build();

    // Step 1: emit fails → budget NOT consumed.
    reg.register(OneItemConnector::new("01ARZ3NDEKTSV4RRFFQ69G5FB1"))
        .expect("register must succeed");
    reg.enable("budget-test", one_grant())
        .await
        .expect("enable must succeed");

    let err1 = reg
        .poll_now("budget-test")
        .await
        .expect_err("first poll_now must fail because emit fails");
    assert!(
        matches!(err1, ConnectorError::Transient(_)),
        "expected Transient (from failed emit), got {err1:?}",
    );

    // Step 2: disable and re-enable (same registry, same rate_limit bucket).
    // Emit NOW succeeds (calls=2). Budget should still be 1 because the first
    // emit failure didn't consume a token.
    reg.disable("budget-test")
        .await
        .expect("disable must succeed");
    reg.enable("budget-test", one_grant())
        .await
        .expect("re-enable must succeed");

    reg.poll_now("budget-test")
        .await
        .expect("second poll_now must succeed: emit succeeds and budget still available");

    // Step 3: budget is now exhausted (1 token used by step 2). A third
    // poll_now must fail with BudgetExceeded.
    reg.disable("budget-test")
        .await
        .expect("disable must succeed");
    reg.enable("budget-test", one_grant())
        .await
        .expect("third enable must succeed");

    let err3 = reg
        .poll_now("budget-test")
        .await
        .expect_err("third poll_now must fail because budget is exhausted");
    assert!(
        matches!(err3, ConnectorError::BudgetExceeded { .. }),
        "expected BudgetExceeded on third attempt, got {err3:?}",
    );

    reg.shutdown().await;
}
