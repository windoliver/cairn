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

/// The registry must reserve the budget BEFORE emit and reject over-budget events
/// without calling emit (Finding K — reserve/release pattern).
///
/// Budget = 2, events emitted by connector = 3 (all same scope).
///
/// Finding K fix: budget is reserved before `emit`. Over-budget events are
/// rejected with `BudgetExceeded` BEFORE `emit` is called. This prevents
/// duplicate delivery when the same window retries after a `BudgetExceeded`.
///
/// Expected: only 2 events reach `emit` (count = 2); `poll_now` returns
/// `Err(BudgetExceeded)` because the 3rd event cannot reserve a token.
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

    // Finding K fix: the 3rd event is rejected BEFORE emit — only 2 events reach emit.
    let count = *emit.0.lock().expect("mutex unpoisoned");
    assert_eq!(
        count, 2,
        "only events within budget must reach emit; over-budget event must be blocked before emit (got {count})",
    );

    reg.shutdown().await;
}

/// Over-budget events must not reach `emit` (Finding K).
///
/// Budget = 1. First `poll_now` exhausts the budget (count = 1). Second
/// `poll_now` is rejected with `BudgetExceeded` BEFORE `emit` is called, so
/// the `PipelineEmit` recorder count stays at 1.
///
/// Uses two separate `poll_now` calls rather than a single poll returning two
/// events so the test can be expressed without defining local `impl` blocks
/// (which trigger `clippy::items_after_statements`).
#[tokio::test]
async fn over_budget_event_rejected_before_emit() {
    let emit = Arc::new(CountEmit::default());
    let tmp = tempfile::tempdir().expect("tempdir");

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(emit.clone() as Arc<dyn PipelineEmit>)
        .spool_root(tmp.path().to_path_buf())
        .build();

    // OneItemConnector uses BUDGET_ONE_MANIFEST (budget = 1).
    reg.register(OneItemConnector::new("01ARZ3NDEKTSV4RRFFQ69G5FA1"))
        .expect("register must succeed");
    reg.enable("budget-test", one_grant())
        .await
        .expect("enable must succeed");

    // First poll — budget available; emit is called.
    reg.poll_now("budget-test")
        .await
        .expect("first poll_now must succeed");

    {
        let count = *emit.0.lock().expect("mutex unpoisoned");
        assert_eq!(count, 1, "first poll_now must emit exactly 1 event");
    }

    // Second poll — budget is now zero; emit must NOT be called.
    let err = reg
        .poll_now("budget-test")
        .await
        .expect_err("second poll_now must fail: budget exhausted");

    assert!(
        matches!(err, ConnectorError::BudgetExceeded { .. }),
        "expected BudgetExceeded, got {err:?}",
    );

    // emit count must still be 1 — the over-budget event never reached emit.
    let count = *emit.0.lock().expect("mutex unpoisoned");
    assert_eq!(
        count, 1,
        "over-budget event must not reach emit; count must remain 1 (got {count})",
    );

    reg.shutdown().await;
}

/// A failed emit must refund the reserved budget token (Finding K).
///
/// Budget = 1. `FailOnceEmit` fails on the first call then succeeds.
///
/// Step 1: first event → emit fails → budget refunded → `poll_now` returns Transient.
/// Step 2: disable+re-enable; same event → emit succeeds → budget consumed.
/// Step 3: disable+re-enable; budget zero → `poll_now` returns `BudgetExceeded`.
#[tokio::test]
async fn failed_emit_refunds_budget() {
    let fail_emit = Arc::new(OnceFailEmit::default());
    let tmp = tempfile::tempdir().expect("tempdir");

    // A connector that always emits the same single event.
    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(fail_emit.clone() as Arc<dyn PipelineEmit>)
        .spool_root(tmp.path().to_path_buf())
        .build();

    reg.register(OneItemConnector::new("01ARZ3NDEKTSV4RRFFQ69G5FB1"))
        .expect("register must succeed");
    reg.enable("budget-test", one_grant())
        .await
        .expect("enable must succeed");

    // Step 1: emit fails → budget must be refunded.
    let err1 = reg
        .poll_now("budget-test")
        .await
        .expect_err("first poll_now must fail because emit fails");
    assert!(
        matches!(err1, ConnectorError::Transient(_)),
        "expected Transient (from failed emit), got {err1:?}",
    );

    // Step 2: disable + re-enable; emit now succeeds; budget still available thanks to refund.
    reg.disable("budget-test")
        .await
        .expect("disable must succeed");
    reg.enable("budget-test", one_grant())
        .await
        .expect("re-enable must succeed");

    reg.poll_now("budget-test")
        .await
        .expect("second poll_now must succeed: emit succeeds and budget was refunded");

    // Step 3: budget exhausted after step 2 success.
    reg.disable("budget-test")
        .await
        .expect("disable must succeed");
    reg.enable("budget-test", one_grant())
        .await
        .expect("third enable must succeed");

    let err3 = reg
        .poll_now("budget-test")
        .await
        .expect_err("third poll_now must fail because budget is now exhausted");
    assert!(
        matches!(err3, ConnectorError::BudgetExceeded { .. }),
        "expected BudgetExceeded on third attempt, got {err3:?}",
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

// ---------------------------------------------------------------------------
// Finding M — over-budget events and failed emits must not leave spool files
// ---------------------------------------------------------------------------

/// Over-budget events must not write a spool file (Finding M).
///
/// Setup: connector with budget = 1.
///
/// Step 1: first `poll_now` succeeds → exactly 1 spool file written.
/// Step 2: second `poll_now` returns `BudgetExceeded` → budget reservation
///         fails BEFORE the spool write, so NO new spool file appears.
///
/// This verifies the key ordering invariant: `try_reserve` now runs before
/// `spool_bytes`, closing the race where an over-budget rejection left an
/// orphaned file on disk.
// The inline connector struct accounts for most of the line count here;
// extracting it into a top-level helper would obscure the test's narrative.
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn over_budget_event_leaves_no_spool_file() {
    // Two-tick connector: emits a different event ID each time `poll` is called
    // so the two ticks produce distinct spool filenames (content-addressed by
    // event_id + hash prefix). Without distinct IDs both ticks would hash to
    // the same filename and the second tick would overwrite the first, making
    // the file-count assertion ambiguous.
    struct TwoTickConnector {
        manifest: ConnectorManifest,
        sensor: Identity,
        tick: std::sync::Mutex<u32>,
    }

    impl TwoTickConnector {
        fn new() -> Self {
            Self {
                manifest: ConnectorManifest::parse_toml(BUDGET_ONE_MANIFEST)
                    .expect("BUDGET_ONE_MANIFEST must parse"),
                sensor: Identity::parse("snr:local:connector:budget-test:v1")
                    .expect("sensor identity must parse"),
                tick: std::sync::Mutex::new(0),
            }
        }
    }

    #[async_trait]
    impl Connector for TwoTickConnector {
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
            let mut t = self.tick.lock().expect("tick mutex unpoisoned");
            *t += 1;
            // Alternate between two distinct event IDs.
            let id = if *t == 1 {
                "01ARZ3NDEKTSV4RRFFQ69G5FA1"
            } else {
                "01ARZ3NDEKTSV4RRFFQ69G5FA2"
            };
            Ok(PollOutcome {
                events: vec![ConnectorEvent::new(
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

    impl ConnectorPlugin for TwoTickConnector {
        const NAME: &'static str = "budget-test";
        const SUPPORTED_VERSIONS: VersionRange =
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let spool_dir = tmp.path().join("connector").join("budget-test");

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(Arc::new(CountEmit::default()) as Arc<dyn PipelineEmit>)
        .spool_root(tmp.path().to_path_buf())
        .build();

    reg.register(TwoTickConnector::new())
        .expect("register must succeed");
    reg.enable("budget-test", one_grant())
        .await
        .expect("enable must succeed");

    // Step 1: first poll succeeds — exactly 1 spool file must appear.
    reg.poll_now("budget-test")
        .await
        .expect("first poll_now must succeed");

    let files_after_first: Vec<_> = std::fs::read_dir(&spool_dir)
        .expect("spool dir must exist after first poll")
        .filter_map(std::result::Result::ok)
        .collect();
    assert_eq!(
        files_after_first.len(),
        1,
        "exactly 1 spool file must exist after first poll; found {files_after_first:?}",
    );

    // Step 2: second poll must fail with BudgetExceeded (budget exhausted).
    let err = reg
        .poll_now("budget-test")
        .await
        .expect_err("second poll_now must fail: budget exhausted");
    assert!(
        matches!(err, ConnectorError::BudgetExceeded { .. }),
        "expected BudgetExceeded on second poll, got {err:?}",
    );

    // The spool directory must still have exactly 1 file — no new file was
    // written because `try_reserve` rejected the event before `spool_bytes`.
    let files_after_second: Vec<_> = std::fs::read_dir(&spool_dir)
        .expect("spool dir must exist after second poll")
        .filter_map(std::result::Result::ok)
        .collect();
    assert_eq!(
        files_after_second.len(),
        1,
        "over-budget event must not create a spool file; found {files_after_second:?}",
    );

    reg.shutdown().await;
}

/// After a failed emit the spool directory must contain exactly one committed
/// (orphan) file and zero `.tmp` files (Finding M updated for Finding S).
///
/// Finding S reorders to: write-tmp → rename-tmp-to-final → emit. When emit
/// fails the final file is already on disk; the framework intentionally leaves
/// it there (documented orphan tradeoff — sweep workflow handles it in #131).
/// Only the tmp file, which was renamed to the final path, should be gone.
///
/// The key Finding M invariant that still holds: budget tokens ARE refunded on
/// emit failure so the event can be retried without permanently consuming budget.
#[tokio::test]
async fn failed_emit_leaves_orphan_file_no_tmp() {
    /// `PipelineEmit` that always returns `Err(Transient)`.
    struct AlwaysFailEmit;

    #[async_trait]
    impl PipelineEmit for AlwaysFailEmit {
        async fn emit(
            &self,
            _: cairn_core::domain::capture::CaptureEvent,
        ) -> Result<(), ConnectorError> {
            Err(ConnectorError::transient_msg(
                "simulated emit failure for spool cleanup test",
            ))
        }
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let spool_dir = tmp.path().join("connector").join("budget-test");

    // Use `one_grant` (budget = 1) — budget is not the failing factor here.
    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(Arc::new(AlwaysFailEmit))
        .spool_root(tmp.path().to_path_buf())
        .build();

    reg.register(OneItemConnector::new("01ARZ3NDEKTSV4RRFFQ69G5FC1"))
        .expect("register must succeed");
    reg.enable("budget-test", one_grant())
        .await
        .expect("enable must succeed");

    // poll_now must fail because emit always returns Err.
    let err = reg
        .poll_now("budget-test")
        .await
        .expect_err("poll_now must fail because emit always fails");
    assert!(
        matches!(err, ConnectorError::Transient(_)),
        "expected Transient from failing emit, got {err:?}",
    );

    reg.shutdown().await;

    // Finding S: rename happens before emit. Emit failure leaves the final file
    // on disk (orphan). Exactly 1 committed file, 0 .tmp files.
    let committed = match std::fs::read_dir(&spool_dir) {
        Ok(entries) => entries
            .filter_map(std::result::Result::ok)
            .filter(|e| !e.file_name().to_string_lossy().ends_with(".tmp"))
            .count(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => panic!("unexpected read_dir error: {e}"),
    };
    assert_eq!(
        committed, 1,
        "exactly 1 orphan committed file must exist after a failed emit \
         (Finding S rename-before-emit tradeoff); got {committed}",
    );

    let tmps = match std::fs::read_dir(&spool_dir) {
        Ok(entries) => entries
            .filter_map(std::result::Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .count(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => panic!("unexpected read_dir error on tmp check: {e}"),
    };
    assert_eq!(
        tmps, 0,
        "no .tmp files must remain after a failed emit (tmp was renamed to final); got {tmps}",
    );
}

// ---------------------------------------------------------------------------
// Finding R — daily byte budget must block events that exceed the day cap
// ---------------------------------------------------------------------------

/// Manifest with a 1 KiB daily byte cap — used by Finding R tests.
///
/// The item budget is large so byte exhaustion, not item exhaustion, is
/// the limiting factor.
const BYTE_BUDGET_MANIFEST: &str = r#"
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
max_items_per_hour = 1000
max_bytes_per_day = "1024"

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

fn byte_budget_manifest() -> ConnectorManifest {
    ConnectorManifest::parse_toml(BYTE_BUDGET_MANIFEST).expect("BYTE_BUDGET_MANIFEST must parse")
}

fn byte_budget_grant() -> cairn_core::contract::connector_consent::ConsentGrant {
    cairn_core::contract::connector_consent::ConsentGrant::new(
        "budget-test",
        byte_budget_manifest().hash(),
        BTreeSet::from(["note".to_string()]),
        vec!["project:*".to_string()],
        1_700_000_000,
        Identity::parse("hmn:alice").expect("hmn:alice is valid"),
    )
}

/// Build a JSON-payload event whose serialized byte size is controlled by
/// the length of the `payload_str` argument.
///
/// The body is `{"data": "<payload_str>"}`. For a string of length `n` the
/// serialized JSON is roughly `n + 10` bytes (key + quotes + braces).
fn make_byte_event(id: &str, payload_str: &str) -> ConnectorEvent {
    ConnectorEvent::new(
        ConnectorEventId::new(id),
        "budget-test",
        SourceRef::new("issue", id, None),
        0,
        BTreeSet::from(["note".to_string()]),
        ConnectorScope::project("owner/repo"),
        ConnectorPayload::Json {
            mime: "application/json".into(),
            body: serde_json::json!({"data": payload_str}),
        },
        DeliveryMode::Poll { cursor: None },
    )
}

/// The daily byte budget must block events that would push total emitted bytes
/// past `max_bytes_per_day` (Finding R).
///
/// Setup:
/// - `max_bytes_per_day = 1024` (1 KiB), `max_items_per_hour = 1000` (not limiting).
/// - First event: ~600 bytes of JSON body → accepted (600 ≤ 1024).
/// - Second event: another ~600 bytes → total would be ~1200 > 1024 → rejected.
///
/// Assertions:
/// - First `poll_now` succeeds; emit count = 1.
/// - Second `poll_now` returns `Err(BudgetExceeded)`; emit count stays at 1.
// inline connector struct after variable setup is idiomatic in integration tests
#[allow(clippy::items_after_statements)]
#[tokio::test]
async fn daily_byte_budget_blocks_oversize_total() {
    // Build a 600-character filler string so the JSON body serialises to
    // approximately 600 bytes.
    let filler_600: String = "x".repeat(600);

    let emit = Arc::new(CountEmit::default());
    let tmp = tempfile::tempdir().expect("tempdir");

    // Connector emitting two sequential events via two poll_now calls.
    struct TwoCallConnector {
        manifest: ConnectorManifest,
        sensor: Identity,
        call: std::sync::Mutex<u32>,
        filler: String,
    }

    #[async_trait]
    impl Connector for TwoCallConnector {
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
            let mut call = self.call.lock().expect("call mutex");
            *call += 1;
            let id = if *call == 1 {
                "01ARZ3NDEKTSV4RRFFQ69G5FD1"
            } else {
                "01ARZ3NDEKTSV4RRFFQ69G5FD2"
            };
            Ok(PollOutcome {
                events: vec![make_byte_event(id, &self.filler)],
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

    impl ConnectorPlugin for TwoCallConnector {
        const NAME: &'static str = "budget-test";
        const SUPPORTED_VERSIONS: VersionRange =
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
    }

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(emit.clone() as Arc<dyn PipelineEmit>)
        .spool_root(tmp.path().to_path_buf())
        .build();

    reg.register(TwoCallConnector {
        manifest: byte_budget_manifest(),
        sensor: Identity::parse("snr:local:connector:budget-test:v1")
            .expect("sensor identity must parse"),
        call: std::sync::Mutex::new(0),
        filler: filler_600,
    })
    .expect("register must succeed");
    reg.enable("budget-test", byte_budget_grant())
        .await
        .expect("enable must succeed");

    // First poll — byte budget available; event must be accepted.
    reg.poll_now("budget-test")
        .await
        .expect("first poll_now must succeed — byte budget not yet exceeded");

    {
        let count = *emit.0.lock().expect("mutex");
        assert_eq!(
            count, 1,
            "first poll_now must emit exactly 1 event (got {count})"
        );
    }

    // Second poll — adding another ~600 bytes would exceed 1 KiB total.
    let err = reg
        .poll_now("budget-test")
        .await
        .expect_err("second poll_now must fail — daily byte budget exceeded");

    assert!(
        matches!(err, ConnectorError::BudgetExceeded { .. }),
        "expected BudgetExceeded when daily byte cap is exceeded, got {err:?}",
    );

    // Emit count must still be 1 — the second event was rejected before emit.
    let count = *emit.0.lock().expect("mutex");
    assert_eq!(
        count, 1,
        "over-byte-budget event must not reach emit; count must remain 1 (got {count})",
    );

    reg.shutdown().await;
}

// ---------------------------------------------------------------------------
// Finding U — aggregate budget by connector, not by scope
// ---------------------------------------------------------------------------

/// The daily byte budget must aggregate across all scopes (Finding U).
///
/// Before the fix, each distinct scope key got its own full-capacity byte
/// bucket. A connector with a wildcard scope declaration (`project:*`) could
/// emit N×cap bytes by emitting events from N different scopes. This test
/// verifies the fix: the byte budget is keyed by connector name, so two events
/// from different scopes (`project:foo` and `project:bar`) share one budget.
///
/// Setup:
/// - `max_bytes_per_day = 1024` (1 KiB), `max_items_per_hour = 1000`.
/// - Event 1: scope `project:foo`, ~600 bytes → accepted.
/// - Event 2: scope `project:bar`, ~600 bytes → total would be ~1200 > 1024 →
///   rejected with `BudgetExceeded`.
#[allow(clippy::items_after_statements, clippy::too_many_lines)]
#[tokio::test]
async fn aggregate_byte_budget_across_scopes() {
    let filler_600: String = "x".repeat(600);

    // A connector that emits alternating scopes on each poll_now call.
    struct ScopedConnector {
        manifest: ConnectorManifest,
        sensor: Identity,
        call: std::sync::Mutex<u32>,
        filler: String,
    }

    #[async_trait]
    impl Connector for ScopedConnector {
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
            let mut call = self.call.lock().expect("call mutex");
            *call += 1;
            let (id, scope) = if *call == 1 {
                ("01ARZ3NDEKTSV4RRFFQ69G5FE1", ConnectorScope::project("foo"))
            } else {
                ("01ARZ3NDEKTSV4RRFFQ69G5FE2", ConnectorScope::project("bar"))
            };
            Ok(PollOutcome {
                events: vec![ConnectorEvent::new(
                    ConnectorEventId::new(id),
                    "budget-test",
                    SourceRef::new("issue", id, None),
                    0,
                    BTreeSet::from(["note".to_string()]),
                    scope,
                    ConnectorPayload::Json {
                        mime: "application/json".into(),
                        body: serde_json::json!({"data": self.filler}),
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

    impl ConnectorPlugin for ScopedConnector {
        const NAME: &'static str = "budget-test";
        const SUPPORTED_VERSIONS: VersionRange =
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
    }

    let emit = Arc::new(CountEmit::default());
    let tmp = tempfile::tempdir().expect("tempdir");

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(emit.clone() as Arc<dyn PipelineEmit>)
        .spool_root(tmp.path().to_path_buf())
        .build();

    reg.register(ScopedConnector {
        manifest: byte_budget_manifest(),
        sensor: Identity::parse("snr:local:connector:budget-test:v1")
            .expect("sensor identity must parse"),
        call: std::sync::Mutex::new(0),
        filler: filler_600,
    })
    .expect("register must succeed");
    reg.enable("budget-test", byte_budget_grant())
        .await
        .expect("enable must succeed");

    // First poll (scope = project:foo) — budget available, accepted.
    reg.poll_now("budget-test")
        .await
        .expect("first poll_now (project:foo) must succeed");

    {
        let count = *emit.0.lock().expect("mutex");
        assert_eq!(count, 1, "first event (project:foo) must be emitted");
    }

    // Second poll (scope = project:bar) — byte budget shared across scopes;
    // total would exceed 1 KiB, so this must be rejected.
    let err = reg
        .poll_now("budget-test")
        .await
        .expect_err("second poll_now (project:bar) must fail — shared byte budget exceeded");

    assert!(
        matches!(err, ConnectorError::BudgetExceeded { .. }),
        "expected BudgetExceeded when aggregate byte cap is exceeded (Finding U), got {err:?}",
    );

    // Emit count must remain 1 — the second event was blocked before emit.
    let count = *emit.0.lock().expect("mutex");
    assert_eq!(
        count, 1,
        "second event (project:bar) must not reach emit; aggregate byte cap exceeded (got {count})",
    );

    reg.shutdown().await;
}

/// The hourly item budget must aggregate across all scopes (Finding U).
///
/// Before the fix, each distinct scope key got its own full-capacity item
/// bucket. This test verifies that three events across three different scopes
/// share one connector-level item budget.
///
/// Setup:
/// - `max_items_per_hour = 2`, `max_bytes_per_day = 50MiB` (not limiting).
/// - Events from scopes `project:alpha`, `project:beta`, `project:gamma`.
/// - First two succeed; third returns `BudgetExceeded`.
#[allow(clippy::items_after_statements, clippy::too_many_lines)]
#[tokio::test]
async fn aggregate_item_budget_across_scopes() {
    // A connector that cycles through three scopes on successive poll calls.
    struct ThreeScopeConnector {
        manifest: ConnectorManifest,
        sensor: Identity,
        call: std::sync::Mutex<u32>,
    }

    #[async_trait]
    impl Connector for ThreeScopeConnector {
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
            let mut call = self.call.lock().expect("call mutex");
            *call += 1;
            let (id, scope) = match *call {
                1 => (
                    "01ARZ3NDEKTSV4RRFFQ69G5FF1",
                    ConnectorScope::project("alpha"),
                ),
                2 => (
                    "01ARZ3NDEKTSV4RRFFQ69G5FF2",
                    ConnectorScope::project("beta"),
                ),
                _ => (
                    "01ARZ3NDEKTSV4RRFFQ69G5FF3",
                    ConnectorScope::project("gamma"),
                ),
            };
            Ok(PollOutcome {
                events: vec![ConnectorEvent::new(
                    ConnectorEventId::new(id),
                    "budget-test",
                    SourceRef::new("issue", id, None),
                    0,
                    BTreeSet::from(["note".to_string()]),
                    scope,
                    ConnectorPayload::Json {
                        mime: "application/json".into(),
                        body: serde_json::json!({"id": id}),
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

    impl ConnectorPlugin for ThreeScopeConnector {
        const NAME: &'static str = "budget-test";
        const SUPPORTED_VERSIONS: VersionRange =
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
    }

    let emit = Arc::new(CountEmit::default());
    let tmp = tempfile::tempdir().expect("tempdir");

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(emit.clone() as Arc<dyn PipelineEmit>)
        .spool_root(tmp.path().to_path_buf())
        .build();

    reg.register(ThreeScopeConnector {
        manifest: budget_manifest(), // max_items_per_hour = 2
        sensor: Identity::parse("snr:local:connector:budget-test:v1")
            .expect("sensor identity must parse"),
        call: std::sync::Mutex::new(0),
    })
    .expect("register must succeed");
    reg.enable("budget-test", budget_grant())
        .await
        .expect("enable must succeed");

    // First poll (project:alpha) — budget available.
    reg.poll_now("budget-test")
        .await
        .expect("first poll_now (alpha) must succeed");

    // Second poll (project:beta) — budget has 1 token remaining.
    reg.poll_now("budget-test")
        .await
        .expect("second poll_now (beta) must succeed");

    {
        let count = *emit.0.lock().expect("mutex");
        assert_eq!(count, 2, "first two events must be emitted");
    }

    // Third poll (project:gamma) — item budget exhausted; must be rejected.
    let err = reg
        .poll_now("budget-test")
        .await
        .expect_err("third poll_now (gamma) must fail — shared item budget exhausted");

    assert!(
        matches!(err, ConnectorError::BudgetExceeded { .. }),
        "expected BudgetExceeded on third event across three scopes (Finding U), got {err:?}",
    );

    let count = *emit.0.lock().expect("mutex");
    assert_eq!(
        count, 2,
        "third event must not reach emit; aggregate item budget exceeded (got {count})",
    );

    reg.shutdown().await;
}
