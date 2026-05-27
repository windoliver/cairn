//! AC#3 (issue #161): disable verb prevents subsequent `poll_now` calls.
//!
//! Uses `poll_now` to simulate the next scheduler tick deterministically —
//! the real scheduler interval is 5 minutes, making it impractical to test
//! against real time in an integration test without artificial sleeps.
//!
//! This test proves the full lifecycle:
//! 1. Registry wired with an `InMemoryAdmin` store.
//! 2. First `poll_now` succeeds and emits (connector enabled by default).
//! 3. Operator disables via the `cairn.admin.v1` `disable` verb.
//! 4. Second `poll_now` is a no-op (returns `Ok(())`, no emit) — cursor stays put.

#![allow(missing_docs)]

use std::collections::BTreeSet;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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
use cairn_core::contract::admin_state::{AdminStateStore, ConnectorStateRow};
use cairn_core::contract::memory_store::StoreError;
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::Identity;
use cairn_core::domain::admin::{AdminContext, AdminRole};
use cairn_core::domain::capture::CaptureEvent;
use cairn_core::verbs::admin::connector::{ConnectorDisableRequest, disable as disable_verb};

// ---------------------------------------------------------------------------
// Fixture connector manifest
// ---------------------------------------------------------------------------

const DISABLE_RACE_MANIFEST: &str = r#"
[connector]
name = "disable-race"
contract = "Connector"
contract_version = "0.1.0"
sensor_identity = "snr:local:connector:disable-race:v1"

[capabilities]
poll = true
webhook = false
backfill = false

[oauth]
required_scopes = []
token_lifetime = "1h"
refresh = false

[budget]
max_items_per_hour = 10000
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
max_bytes = "1MiB"
max_depth = 4
"#;

// ---------------------------------------------------------------------------
// Fixture connector
// ---------------------------------------------------------------------------

struct DisableRaceConnector {
    manifest: ConnectorManifest,
    sensor: Identity,
}

impl DisableRaceConnector {
    fn new() -> Self {
        Self {
            manifest: ConnectorManifest::parse_toml(DISABLE_RACE_MANIFEST)
                .expect("DISABLE_RACE_MANIFEST must parse"),
            sensor: Identity::parse("snr:local:connector:disable-race:v1")
                .expect("disable-race sensor identity must parse"),
        }
    }
}

#[async_trait]
impl Connector for DisableRaceConnector {
    fn name(&self) -> &'static str {
        "disable-race"
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
                ConnectorEventId::new("01HX0000000000000000000002"),
                "disable-race",
                SourceRef::new("issue", "x", None),
                0,
                BTreeSet::from(["note".to_string()]),
                ConnectorScope::project("owner/repo"),
                ConnectorPayload::Json {
                    mime: "application/json".into(),
                    body: serde_json::json!({"body": "disable-race-tick"}),
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

impl ConnectorPlugin for DisableRaceConnector {
    const NAME: &'static str = "disable-race";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
}

// ---------------------------------------------------------------------------
// In-memory AdminStateStore with real operator tracking
// ---------------------------------------------------------------------------

/// Full in-memory `AdminStateStore` that tracks both role grants and
/// connector state rows — suitable for testing the `disable` verb
/// end-to-end without a real `SQLite` backend.
#[derive(Default)]
struct InMemoryAdmin {
    rows: Mutex<HashMap<String, ConnectorStateRow>>,
    operators: Mutex<std::collections::HashSet<String>>,
}

impl AdminStateStore for InMemoryAdmin {
    fn has_role(&self, actor: &Identity, _: AdminRole) -> Result<bool, StoreError> {
        Ok(self
            .operators
            .lock()
            .expect("InMemoryAdmin operators lock")
            .contains(actor.as_str()))
    }

    fn has_any_operator(&self) -> Result<bool, StoreError> {
        Ok(!self
            .operators
            .lock()
            .expect("InMemoryAdmin operators lock")
            .is_empty())
    }

    fn grant_role(&self, actor: &Identity, _: AdminRole, _: &Identity) -> Result<(), StoreError> {
        self.operators
            .lock()
            .expect("InMemoryAdmin operators lock")
            .insert(actor.as_str().to_owned());
        Ok(())
    }

    fn revoke_role(&self, actor: &Identity, _: AdminRole) -> Result<(), StoreError> {
        self.operators
            .lock()
            .expect("InMemoryAdmin operators lock")
            .remove(actor.as_str());
        Ok(())
    }

    fn set_connector_enabled(
        &self,
        name: &str,
        enabled: bool,
        by: &Identity,
        reason: Option<&str>,
    ) -> Result<ConnectorStateRow, StoreError> {
        let row = ConnectorStateRow::new(
            name.to_owned(),
            enabled,
            chrono::Utc::now(),
            by.clone(),
            reason.map(str::to_owned),
        );
        self.rows
            .lock()
            .expect("InMemoryAdmin rows lock")
            .insert(name.to_owned(), row.clone());
        Ok(row)
    }

    fn get_connector_state(&self, name: &str) -> Result<Option<ConnectorStateRow>, StoreError> {
        Ok(self
            .rows
            .lock()
            .expect("InMemoryAdmin rows lock")
            .get(name)
            .cloned())
    }

    fn list_connector_state(&self) -> Result<Vec<ConnectorStateRow>, StoreError> {
        Ok(self
            .rows
            .lock()
            .expect("InMemoryAdmin rows lock")
            .values()
            .cloned()
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Counting emit
// ---------------------------------------------------------------------------

#[derive(Default)]
struct CountingEmit(std::sync::atomic::AtomicUsize);

#[async_trait]
impl PipelineEmit for CountingEmit {
    async fn emit(&self, _: CaptureEvent) -> Result<(), ConnectorError> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Consent grant helper
// ---------------------------------------------------------------------------

fn disable_race_grant() -> cairn_core::contract::connector_consent::ConsentGrant {
    cairn_core::contract::connector_consent::ConsentGrant::new(
        "disable-race",
        ConnectorManifest::parse_toml(DISABLE_RACE_MANIFEST)
            .expect("DISABLE_RACE_MANIFEST must parse")
            .hash(),
        BTreeSet::from(["note".to_string()]),
        vec!["project:*".to_string()],
        1_700_000_000,
        Identity::parse("hmn:alice").expect("hmn:alice is valid"),
    )
}

// ---------------------------------------------------------------------------
// AC#3 test
// ---------------------------------------------------------------------------

/// AC#3 (issue #161): disable verb stops subsequent polls.
///
/// Verifies that after `cairn.admin.v1.disable` writes a disabled row,
/// `poll_now` respects the gate and returns `Ok(())` without invoking
/// `PipelineEmit::emit`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
async fn disable_verb_stops_subsequent_poll_now() {
    // 1. Build admin store and grant operator role.
    let admin = Arc::new(InMemoryAdmin::default());
    let op = Identity::parse("hmn:op").expect("test fixture");
    let bootstrap = Identity::parse("hmn:bootstrap").expect("test fixture");
    // Grant the operator role so the `disable` verb's role check passes.
    admin
        .grant_role(&op, AdminRole::Operator, &bootstrap)
        .expect("grant_role must succeed");

    let counter = Arc::new(CountingEmit::default());

    // 2. Build registry wired with the admin state store.
    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(counter.clone() as Arc<dyn PipelineEmit>)
        .build()
        .with_admin_state(admin.clone() as Arc<dyn AdminStateStore>);

    // 3. Register and enable the fixture connector.
    reg.register(DisableRaceConnector::new())
        .expect("register must succeed");
    reg.enable("disable-race", disable_race_grant())
        .await
        .expect("enable must succeed");

    // 4. First poll_now: no admin row → defaults to enabled → emits one event.
    reg.poll_now("disable-race")
        .await
        .expect("first poll_now must succeed (no row → enabled)");
    let after_first = counter.0.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(after_first, 1, "expected exactly 1 emit before disable");

    // 5. Disable via the cairn.admin.v1 verb.
    let ctx = AdminContext::new(op.clone(), AdminRole::Operator);
    disable_verb(
        &ctx,
        &ConnectorDisableRequest {
            name: "disable-race".into(),
            reason: Some("AC#3 integration test".into()),
        },
        admin.as_ref(),
    )
    .expect("disable verb must succeed for an operator");

    // 6. Confirm the row was written with enabled = false.
    let row = admin
        .get_connector_state("disable-race")
        .expect("get_connector_state must not error")
        .expect("row must exist after disable");
    assert!(
        !row.enabled,
        "connector must be marked disabled in the store"
    );
    assert_eq!(
        row.reason.as_deref(),
        Some("AC#3 integration test"),
        "reason must be preserved"
    );

    // 7. Second poll_now: admin gate sees disabled row → skips (Ok, no emit).
    reg.poll_now("disable-race")
        .await
        .expect("poll_now for a disabled connector must return Ok (not an error)");
    let after_second = counter.0.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        after_second, 1,
        "poll_now must not emit when the connector is disabled (count must stay at 1)"
    );

    reg.shutdown().await;
}

/// Sanity check: when admin state is wired but the row says `enabled = true`,
/// `poll_now` still emits (regression guard for the gate logic).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
async fn enabled_row_still_allows_poll_now() {
    let admin = Arc::new(InMemoryAdmin::default());
    let op = Identity::parse("hmn:op").expect("test fixture");
    let bootstrap = Identity::parse("hmn:bootstrap").expect("test fixture");
    admin
        .grant_role(&op, AdminRole::Operator, &bootstrap)
        .expect("grant_role");

    // Explicitly write an enabled = true row before polling.
    admin
        .set_connector_enabled("disable-race", true, &op, None)
        .expect("set_connector_enabled");

    let counter = Arc::new(CountingEmit::default());

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(counter.clone() as Arc<dyn PipelineEmit>)
        .build()
        .with_admin_state(admin.clone() as Arc<dyn AdminStateStore>);

    reg.register(DisableRaceConnector::new()).expect("register");
    reg.enable("disable-race", disable_race_grant())
        .await
        .expect("enable");

    reg.poll_now("disable-race")
        .await
        .expect("poll_now must succeed when enabled row is present");

    assert_eq!(
        counter.0.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "must emit once when connector has an explicit enabled = true row"
    );

    reg.shutdown().await;
}
