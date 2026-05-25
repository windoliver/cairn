//! `ConnectorRegistry` — central lifecycle: register → enable →
//! poll/webhook → disable → shutdown.
//!
//! The registry is the single point of authority over which connectors are
//! active, which consent grants cover them, and whether a poll task is
//! running. It is the only place that wires together
//! [`CredentialStore`], [`ConnectorConsentJournal`], [`PipelineEmit`], and
//! the per-entry poll tasks.
//!
//! # Architecture notes
//!
//! - State reads are lock-free: each [`Entry`] holds an [`ArcSwap`] so
//!   readers never block writers (and vice-versa). The mutable `&mut self`
//!   methods (`register`, `enable`, `disable`) are the only mutation points.
//! - Each poll-capable connector gets its own [`CancellationToken`] and
//!   [`tokio::task::JoinHandle`] stored in the [`Entry`]. `enable` spawns
//!   the task and stores both; `disable` cancels the token, awaits the
//!   handle, then marks the entry [`ConnectorState::Disabled`]. This
//!   ensures disabled connectors never make further upstream calls.
//! - Calling `enable` on an already-enabled connector is rejected with a
//!   [`ConnectorError::Fatal`] — the caller must `disable` first to avoid
//!   spawning a duplicate task.
//! - [`PollScheduler`] is intentionally **not** used by the registry.
//!   Per-entry tasks with per-entry tokens replace it here. `PollScheduler`
//!   remains available for other consumers in the crate.
//!
//! Issue #130, brief §9.1 source sensors, §19 v0.3.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use bon::Builder;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use cairn_core::contract::connector_consent::{
    ConnectorConsentJournal, ConnectorConsentLookup, ConsentGrant, ConsentGrantId,
};
use cairn_core::domain::capture::PayloadHash;

use crate::connector::{Connector, ConnectorPlugin, PollContext};
use crate::credential::{CredentialHandle, CredentialStore};
use crate::emit::{PipelineEmit, build_capture_event};
use crate::error::ConnectorError;
use crate::redact::RedactionPipeline;

// ---------------------------------------------------------------------------
// Internal state types (not part of the public API)
// ---------------------------------------------------------------------------

/// Lifecycle state of one registered connector.
///
/// Stored behind an [`ArcSwap`] so transitions are lock-free.
#[derive(Debug, Clone)]
enum ConnectorState {
    /// Connector is registered but not yet enabled; no consent grant exists.
    Disabled,
    /// Connector is enabled and has a live consent grant.
    Enabled {
        /// Full consent grant stored at enable-time. Used to verify
        /// manifest-hash stability and to check `allowed_labels` on every emit.
        grant: ConsentGrant,
        /// Opaque consent-grant id returned by [`ConnectorConsentJournal::put_grant`].
        grant_id: ConsentGrantId,
    },
}

/// One slot in the registry for a registered connector.
struct Entry {
    /// Shared reference to the connector implementation.
    connector: Arc<dyn Connector>,
    /// Current lifecycle state wrapped in an `Arc` so the poll-task closure can
    /// hold a clone without going through `&Entry`. Reads are lock-free;
    /// writes use [`ArcSwap::store`].
    state: Arc<ArcSwap<ConnectorState>>,
    /// Per-entry cancellation token. Created when a poll task is spawned by
    /// [`ConnectorRegistry::enable`] and cancelled by
    /// [`ConnectorRegistry::disable`]. `None` before first enable and after
    /// disable completes.
    poll_token: Option<CancellationToken>,
    /// Join handle for the running poll task. Taken (set to `None`) by
    /// [`ConnectorRegistry::disable`] so the registry can `.await` the
    /// task's exit before marking the entry `Disabled`.
    poll_handle: Option<JoinHandle<()>>,
}

// ---------------------------------------------------------------------------
// ConnectorRegistry
// ---------------------------------------------------------------------------

/// Central registry that drives the connector lifecycle.
///
/// Build with [`ConnectorRegistry::builder()`]:
///
/// ```ignore
/// let mut reg = ConnectorRegistry::builder()
///     .credentials(Arc::new(InMemoryCredentialStore::default()))
///     .consent(Arc::new(my_consent_journal))
///     .emit(Arc::new(my_emit))
///     .build();
/// ```
///
/// Then:
/// 1. [`register`][Self::register] — add a connector plugin.
/// 2. [`enable`][Self::enable] — write a consent grant and start polling.
/// 3. [`disable`][Self::disable] — revoke the grant, cancel the poll task,
///    and await its exit.
/// 4. [`shutdown`][Self::shutdown] — cancel all tasks and await them.
#[derive(Builder)]
pub struct ConnectorRegistry {
    /// Credential store used to fetch OAuth tokens for poll calls.
    ///
    /// Real credential lookup is wired in #131; the field is retained now so
    /// the builder API is stable and callers do not need to change when #131
    /// lands.
    #[allow(dead_code)] // wired in #131 (real credential lookup)
    credentials: Arc<dyn CredentialStore>,
    /// Consent journal used to persist, lookup, and revoke grants.
    consent: Arc<dyn ConnectorConsentJournal>,
    /// Downstream pipeline that receives fully-validated [`CaptureEvent`]s.
    ///
    /// [`CaptureEvent`]: cairn_core::domain::capture::CaptureEvent
    emit: Arc<dyn PipelineEmit>,
    /// Registry-wide shutdown token. Cancelling this stops all per-entry poll
    /// tasks without needing to enumerate the entries.
    ///
    /// Defaults to a fresh token so callers that do not need external
    /// cancellation control can omit this field.
    #[builder(default = CancellationToken::new())]
    shutdown: CancellationToken,
    /// Registered connector entries, keyed by connector name.
    #[builder(skip)]
    entries: HashMap<String, Entry>,
}

impl ConnectorRegistry {
    /// Register a connector plugin.
    ///
    /// The connector starts in the [`Disabled`][ConnectorState::Disabled]
    /// state. Call [`enable`][Self::enable] to make it active.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectorError::Fatal`] if a connector with the same name has
    /// already been registered (duplicate names are not allowed).
    pub fn register<P: ConnectorPlugin + 'static>(
        &mut self,
        plugin: P,
    ) -> Result<(), ConnectorError> {
        let name = plugin.name().to_owned();
        if self.entries.contains_key(&name) {
            return Err(ConnectorError::fatal_msg(format!(
                "duplicate connector {name}"
            )));
        }
        self.entries.insert(
            name,
            Entry {
                connector: Arc::new(plugin),
                state: Arc::new(ArcSwap::from_pointee(ConnectorState::Disabled)),
                poll_token: None,
                poll_handle: None,
            },
        );
        Ok(())
    }

    /// Enable a connector and write a consent grant to the journal.
    ///
    /// If the connector advertises `capabilities().poll == true`, a dedicated
    /// per-entry poll task is spawned. The task is bound to a per-entry
    /// [`CancellationToken`] so that [`disable`][Self::disable] can stop it
    /// precisely without cancelling other connectors.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectorError::Fatal`] if:
    /// - The connector name is not registered.
    /// - The connector is already enabled (call `disable` first).
    /// - The consent journal fails to write the grant.
    pub async fn enable(&mut self, name: &str, grant: ConsentGrant) -> Result<(), ConnectorError> {
        let entry = self
            .entries
            .get_mut(name)
            .ok_or_else(|| ConnectorError::fatal_msg(format!("unknown connector {name}")))?;

        // Reject double-enable: spawning a second task would leak the first.
        if matches!(**entry.state.load(), ConnectorState::Enabled { .. }) {
            return Err(ConnectorError::fatal_msg(format!(
                "connector {name} is already enabled; call disable() first"
            )));
        }

        let grant_id = self
            .consent
            .put_grant(grant.clone())
            .await
            .map_err(ConnectorError::fatal_msg)?;

        entry
            .state
            .store(Arc::new(ConnectorState::Enabled { grant, grant_id }));

        // Spawn a per-entry poll task if the connector supports polling.
        if entry.connector.capabilities().poll {
            // Per-entry token, child of the registry-wide shutdown token so
            // `shutdown()` also stops all per-entry tasks.
            let entry_token = self.shutdown.child_token();
            let connector = entry.connector.clone();
            let emit = self.emit.clone();
            let consent = self.consent.clone();
            let state = Arc::clone(&entry.state);
            let name_owned = name.to_owned();
            let interval = Duration::from_mins(5);
            // Clone the token for the task; store the original for cancel.
            let task_token = entry_token.clone();

            let handle = tokio::spawn(async move {
                let mut cursor: Option<String> = None;
                loop {
                    tokio::select! {
                        // Cancellation arm: per-entry token or registry shutdown.
                        () = task_token.cancelled() => break,
                        () = tokio::time::sleep(interval) => {
                            let cx = PollContext {
                                credentials: Arc::new(CredentialHandle::empty()),
                                last_cursor: cursor.clone(),
                                budget_remaining_items: u32::MAX,
                            };
                            match connector.poll(&cx).await {
                                Ok(outcome) => {
                                    cursor = outcome.next_cursor.clone();
                                    for event in outcome.events {
                                        if let Err(err) = process_event(
                                            event, &connector, &state, &consent, &emit,
                                        ).await {
                                            tracing::warn!(
                                                connector = %name_owned,
                                                ?err,
                                                "process_event returned an error; retrying on next interval",
                                            );
                                        }
                                    }
                                }
                                Err(err) => {
                                    tracing::warn!(
                                        connector = %name_owned,
                                        ?err,
                                        "poll tick returned an error; retrying on next interval",
                                    );
                                }
                            }
                        }
                    }
                }
            });

            entry.poll_token = Some(entry_token);
            entry.poll_handle = Some(handle);
        }
        Ok(())
    }

    /// Disable a connector: revoke its consent grant, cancel and await the
    /// poll task, then mark the entry [`Disabled`][ConnectorState::Disabled].
    ///
    /// After this call returns, the connector is fully stopped — no further
    /// poll ticks or upstream HTTP calls will be made.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectorError::Fatal`] if the connector name is not
    /// registered, or if the consent journal fails to revoke the grant.
    pub async fn disable(&mut self, name: &str) -> Result<(), ConnectorError> {
        let entry = self
            .entries
            .get_mut(name)
            .ok_or_else(|| ConnectorError::fatal_msg(format!("unknown connector {name}")))?;

        let state = (**entry.state.load()).clone();
        if let ConnectorState::Enabled { grant_id, .. } = state {
            self.consent
                .revoke(&grant_id)
                .await
                .map_err(ConnectorError::fatal_msg)?;
        }

        // Cancel the per-entry poll task and wait for it to exit cleanly.
        if let Some(token) = entry.poll_token.take() {
            token.cancel();
        }
        if let Some(handle) = entry.poll_handle.take() {
            // A JoinError here means the task panicked — treat as Fatal.
            handle.await.map_err(|e| ConnectorError::fatal_msg(format!(
                "poll task for {name} panicked during disable: {e}"
            )))?;
        }

        entry.state.store(Arc::new(ConnectorState::Disabled));
        Ok(())
    }

    /// **Test-only.** Triggers one poll cycle without waiting for the
    /// scheduler interval. Real production use happens via the scheduler
    /// spawned by `enable`.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectorError::Fatal`] if the connector name is not
    /// registered, or if any event fails to pass validation / consent / emit.
    #[cfg(any(test, feature = "fixture"))]
    pub async fn poll_now(&self, name: &str) -> Result<(), ConnectorError> {
        let entry = self
            .entries
            .get(name)
            .ok_or_else(|| ConnectorError::fatal_msg(format!("unknown connector {name}")))?;

        let cx = crate::connector::PollContext {
            credentials: Arc::new(crate::credential::CredentialHandle::empty()),
            last_cursor: None,
            budget_remaining_items: u32::MAX,
        };
        let outcome = entry.connector.poll(&cx).await?;
        for event in outcome.events {
            process_event(
                event,
                &entry.connector,
                &entry.state,
                &self.consent,
                &self.emit,
            )
            .await?;
        }
        Ok(())
    }

    /// Cancel all per-entry poll tasks and await them.
    ///
    /// Cancels the registry-wide shutdown token (which is the parent of all
    /// per-entry tokens) and then awaits every running task's
    /// [`JoinHandle`][tokio::task::JoinHandle]. Panics in tasks are logged at
    /// `error` level but do not propagate — `shutdown` is a best-effort drain.
    ///
    /// Consumes `self` so the registry cannot be reused after shutdown.
    pub async fn shutdown(mut self) {
        // Cancel all per-entry tasks via the registry-wide token (parent of
        // every child token created in `enable`).
        self.shutdown.cancel();
        // Await each running handle; log panics but do not propagate them.
        for (name, entry) in &mut self.entries {
            let Some(handle) = entry.poll_handle.take() else {
                continue;
            };
            if let Err(e) = handle.await {
                tracing::error!(
                    connector = %name,
                    error = %e,
                    "poll task panicked during registry shutdown",
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Shared event-processing helper
// ---------------------------------------------------------------------------

/// Validate, redact, consent-check, and emit one [`ConnectorEvent`].
///
/// This function is shared between the poll scheduler closure (in
/// [`ConnectorRegistry::enable`]) and the `poll_now` helper that T18 adds for
/// integration testing. Keeping the logic in one place ensures both paths
/// behave identically.
///
/// # Steps
///
/// 0. **Integrity check:** verify `event.connector == connector.name()`.
///    Return [`ConnectorError::Fatal`] if the names differ — a misbehaving
///    connector must not be able to spoof another connector's name to bypass
///    consent checks.
/// 1. Verify `state` is `Enabled`; return [`ConnectorError::Fatal`] if it
///    is `Disabled`. This uses the *real* `ArcSwap<ConnectorState>` shared
///    with the registry, so a `disable()` call propagates immediately.
/// 2. Check every label in `event.labels` against
///    `connector.manifest().allowed_label(label)`; return
///    [`ConnectorError::UndeclaredLabel`] on the first violation.
/// 3. Call `consent.lookup(connector, scope_key)` to enforce the live-grant
///    requirement; return [`ConnectorError::ConsentRevoked`] if the journal
///    returns `Revoked` or an error.
/// 4. Run [`RedactionPipeline::new().redact(event)`] to strip PII.
/// 5. Call [`build_capture_event`] with placeholder spool references (real
///    spool path is written in #131).
/// 6. Call `emit.emit(captured)` to hand the event to the pipeline.
///
/// [`ConnectorEvent`]: crate::event::ConnectorEvent
async fn process_event(
    event: crate::event::ConnectorEvent,
    connector: &Arc<dyn Connector>,
    state: &Arc<ArcSwap<ConnectorState>>,
    consent: &Arc<dyn ConnectorConsentJournal>,
    emit: &Arc<dyn PipelineEmit>,
) -> Result<(), ConnectorError> {
    // 0. Connector-name integrity: a misbehaving connector must not be able
    //    to forge another connector's name and bypass the consent gate.
    if event.connector != connector.name() {
        return Err(ConnectorError::fatal_msg(format!(
            "connector {} cannot emit events claiming to come from {}",
            connector.name(),
            event.connector,
        )));
    }

    // 1. Fail fast if the connector is not currently enabled.
    //    We read from the shared ArcSwap (not a stale snapshot) so a
    //    concurrent `disable()` call is visible immediately.
    let current_state = (**state.load()).clone();
    let grant = match current_state {
        ConnectorState::Disabled => {
            return Err(ConnectorError::fatal_msg(format!(
                "connector {} is Disabled; cannot process events",
                event.connector
            )));
        }
        ConnectorState::Enabled { grant, .. } => grant,
    };

    // 1a. Manifest-hash check (brief §14 "no silent scope widening").
    //     If the connector's manifest was changed after the consent grant was
    //     issued, the hash will differ and the emit is rejected immediately.
    let current_hash = connector.manifest().hash();
    if current_hash != grant.manifest_hash {
        return Err(ConnectorError::ConsentRevoked {
            connector: event.connector.clone(),
        });
    }

    // 1b. Grant label check — every emitted label must appear in the
    //     grant's `allowed_labels` (the set the user approved at enable time).
    //     This is checked before the manifest allow-list so that a grant
    //     narrower than the manifest is also enforced.
    for label in &event.labels {
        if !grant.allowed_labels.contains(label) {
            return Err(ConnectorError::UndeclaredLabel {
                label: label.clone(),
            });
        }
    }

    // 2. Label allow-list check against manifest (defense-in-depth,
    //    brief §9.1). Same check, from the manifest's perspective, in case
    //    the grant was wider than the manifest somehow.
    for label in &event.labels {
        if !connector.manifest().allowed_label(label) {
            return Err(ConnectorError::UndeclaredLabel {
                label: label.clone(),
            });
        }
    }

    // 3. Consent gate — fail closed (brief §14).
    let scope_key = event.scope.lookup_key();
    let lookup_result = consent
        .lookup(&event.connector, &scope_key)
        .await
        .map_err(ConnectorError::fatal_msg)?;

    if !matches!(lookup_result, ConnectorConsentLookup::Granted) {
        return Err(ConnectorError::ConsentRevoked {
            connector: event.connector.clone(),
        });
    }

    // 3b. Payload validation — scope, MIME, size (brief §130 Fix 4).
    event.validate_against_manifest(connector.manifest())?;

    // 4. Redact PII before the event crosses any boundary (brief §5.2 + §14).
    //    Use the manifest's max_depth limit for the JSON walker.
    let redacted = RedactionPipeline::new()
        .with_max_depth(connector.manifest().payload.max_depth)
        .redact(event)?;

    // 5. Build a CaptureEvent with placeholder spool references.
    //    Real spool path + hash are written by the spool layer in #131.
    let placeholder_ref = format!(
        "sources/connector/{}/{}",
        redacted.event.connector, redacted.event.event_id
    );
    let placeholder_hash = PayloadHash::parse(
        "sha256:0000000000000000000000000000000000000000000000000000000000000000",
    )
    .expect("invariant: zero-hash literal must parse");

    let captured = build_capture_event(
        &redacted.event,
        connector.sensor_identity(),
        redacted.spans,
        &placeholder_ref,
        placeholder_hash,
    )?;

    // 6. Hand the event to the downstream pipeline.
    emit.emit(captured).await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Mutex;

    use cairn_core::contract::connector_consent::{
        ConnectorConsentJournal, ConnectorConsentLookup, ConsentGrant, ConsentGrantId,
    };
    use cairn_core::domain::Identity;
    use cairn_core::domain::capture::CaptureEvent;

    use crate::connector::{
        ConnectorCapabilities, ConnectorPlugin, PollContext, PollOutcome, WebhookContext,
    };
    use crate::credential::InMemoryCredentialStore;
    use crate::emit::PipelineEmit;
    use crate::event::{
        ConnectorEvent, ConnectorEventId, ConnectorPayload, ConnectorScope, DeliveryMode, SourceRef,
    };
    use crate::manifest::ConnectorManifest;
    use crate::webhook::WebhookRequest;

    // -----------------------------------------------------------------------
    // Inline stubs (T17 will provide canonical versions in crate::fixture)
    // -----------------------------------------------------------------------

    /// Minimal TOML for the stub connector manifest.
    const STUB_MANIFEST_TOML: &str = r#"
[connector]
name = "stub"
contract = "Connector"
contract_version = "0.1.0"
sensor_identity = "snr:local:connector:stub:v1"

[capabilities]
poll = false
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
"signature.header" = "X-Stub-Signature"
allowed_mimes = ["application/json"]

[poll]
cursor_kind = "opaque-string"
min_interval = "30s"
default_interval = "5m"

[payload]
max_bytes = "256KiB"
max_depth = 10
"#;

    /// Parse the stub manifest once.
    fn stub_manifest() -> ConnectorManifest {
        ConnectorManifest::parse_toml(STUB_MANIFEST_TOML)
            .expect("invariant: stub manifest must parse")
    }

    /// Sensor identity for the stub connector.
    fn stub_sensor() -> Identity {
        Identity::parse("snr:local:connector:stub:v1")
            .expect("invariant: stub sensor identity must parse")
    }

    /// Minimal non-poll connector stub.
    struct StubConnector {
        manifest: ConnectorManifest,
        caps: ConnectorCapabilities,
        sensor: Identity,
    }

    impl StubConnector {
        fn new() -> Self {
            Self {
                manifest: stub_manifest(),
                caps: ConnectorCapabilities {
                    poll: false,
                    webhook: false,
                    backfill: false,
                },
                sensor: stub_sensor(),
            }
        }
    }

    #[async_trait::async_trait]
    impl Connector for StubConnector {
        fn name(&self) -> &'static str {
            "stub"
        }

        fn manifest(&self) -> &ConnectorManifest {
            &self.manifest
        }

        fn capabilities(&self) -> &ConnectorCapabilities {
            &self.caps
        }

        fn sensor_identity(&self) -> &Identity {
            &self.sensor
        }

        fn supported_contract_versions(&self) -> cairn_core::contract::version::VersionRange {
            StubConnector::SUPPORTED_VERSIONS
        }

        async fn poll(&self, _cx: &PollContext) -> Result<PollOutcome, ConnectorError> {
            Ok(PollOutcome::default())
        }

        async fn ingest_webhook(
            &self,
            _req: &WebhookRequest,
            _cx: &WebhookContext,
        ) -> Result<Vec<crate::event::ConnectorEvent>, ConnectorError> {
            Ok(vec![])
        }
    }

    impl ConnectorPlugin for StubConnector {
        const NAME: &'static str = "stub";
        const SUPPORTED_VERSIONS: cairn_core::contract::version::VersionRange =
            cairn_core::contract::version::VersionRange::new(
                cairn_core::contract::version::ContractVersion::new(0, 1, 0),
                cairn_core::contract::version::ContractVersion::new(0, 2, 0),
            );
    }

    /// Accept-all consent journal — every `put_grant` and `lookup` succeeds.
    #[derive(Default)]
    struct AcceptAllConsent {
        grants: Mutex<BTreeMap<String, ConsentGrant>>,
    }

    #[async_trait::async_trait]
    impl ConnectorConsentJournal for AcceptAllConsent {
        async fn put_grant(&self, grant: ConsentGrant) -> Result<ConsentGrantId, String> {
            let id = ConsentGrantId::new(format!("gnt:{}:test", grant.connector));
            self.grants
                .lock()
                .expect("invariant: AcceptAllConsent mutex unpoisoned")
                .insert(id.as_str().to_owned(), grant);
            Ok(id)
        }

        async fn lookup(
            &self,
            _connector: &str,
            _scope_key: &str,
        ) -> Result<ConnectorConsentLookup, String> {
            Ok(ConnectorConsentLookup::Granted)
        }

        async fn revoke(&self, id: &ConsentGrantId) -> Result<(), String> {
            self.grants
                .lock()
                .expect("invariant: AcceptAllConsent mutex unpoisoned")
                .remove(id.as_str());
            Ok(())
        }
    }

    /// Build a minimal `ConsentGrant` for the stub connector.
    ///
    /// Uses the real manifest hash so tests that exercise `process_event`
    /// beyond step 0 do not fail on the manifest-drift check.
    fn stub_grant() -> ConsentGrant {
        let manifest_hash = stub_manifest().hash();
        ConsentGrant::new(
            "stub",
            manifest_hash,
            BTreeSet::from(["note".to_string()]),
            vec!["project:*".to_string()],
            1_700_000_000,
            Identity::parse("hmn:local:test-user")
                .expect("invariant: test user identity must parse"),
        )
    }

    /// Recording `PipelineEmit` that stores every emitted event.
    #[derive(Default)]
    struct NoopEmit {
        events: Mutex<Vec<CaptureEvent>>,
    }

    #[async_trait::async_trait]
    impl PipelineEmit for NoopEmit {
        async fn emit(&self, event: CaptureEvent) -> Result<(), ConnectorError> {
            self.events
                .lock()
                .expect("invariant: NoopEmit mutex unpoisoned")
                .push(event);
            Ok(())
        }
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    /// A connector can be registered, enabled, and then disabled.
    /// Verifies the basic lifecycle without exercising poll execution.
    #[tokio::test]
    async fn register_then_enable_then_disable() {
        let mut reg = ConnectorRegistry::builder()
            .credentials(Arc::new(InMemoryCredentialStore::default()))
            .consent(Arc::new(AcceptAllConsent::default()))
            .emit(Arc::new(NoopEmit::default()))
            .build();

        reg.register(StubConnector::new())
            .expect("first register must succeed");

        reg.enable("stub", stub_grant())
            .await
            .expect("enable must succeed");

        reg.disable("stub").await.expect("disable must succeed");
    }

    /// Registering the same connector name twice must return a Fatal error.
    #[tokio::test]
    async fn duplicate_register_rejected() {
        let mut reg = ConnectorRegistry::builder()
            .credentials(Arc::new(InMemoryCredentialStore::default()))
            .consent(Arc::new(AcceptAllConsent::default()))
            .emit(Arc::new(NoopEmit::default()))
            .build();

        reg.register(StubConnector::new())
            .expect("first register must succeed");

        let err = reg
            .register(StubConnector::new())
            .expect_err("duplicate register must fail");

        assert!(
            matches!(err, ConnectorError::Fatal(_)),
            "expected Fatal, got {err:?}"
        );
    }

    /// Enabling a connector name that was never registered must fail.
    #[tokio::test]
    async fn enable_unknown_connector_fails() {
        let mut reg = ConnectorRegistry::builder()
            .credentials(Arc::new(InMemoryCredentialStore::default()))
            .consent(Arc::new(AcceptAllConsent::default()))
            .emit(Arc::new(NoopEmit::default()))
            .build();

        let err = reg
            .enable("does-not-exist", stub_grant())
            .await
            .expect_err("enabling unknown connector must fail");

        assert!(
            matches!(err, ConnectorError::Fatal(_)),
            "expected Fatal, got {err:?}"
        );
    }

    /// Disabling a connector name that was never registered must fail.
    #[tokio::test]
    async fn disable_unknown_connector_fails() {
        let mut reg = ConnectorRegistry::builder()
            .credentials(Arc::new(InMemoryCredentialStore::default()))
            .consent(Arc::new(AcceptAllConsent::default()))
            .emit(Arc::new(NoopEmit::default()))
            .build();

        let err = reg
            .disable("does-not-exist")
            .await
            .expect_err("disabling unknown connector must fail");

        assert!(
            matches!(err, ConnectorError::Fatal(_)),
            "expected Fatal, got {err:?}"
        );
    }

    /// Build a minimal valid `ConnectorEvent` for the stub connector.
    fn stub_event() -> ConnectorEvent {
        ConnectorEvent::new(
            ConnectorEventId::new("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
            "stub",
            SourceRef::new("issue", "gh:owner/repo#1", None),
            1_700_000_000,
            BTreeSet::from(["note".to_string()]),
            ConnectorScope::project("owner/repo"),
            ConnectorPayload::Text {
                mime: "text/plain".into(),
                body: "hello".into(),
            },
            DeliveryMode::Poll { cursor: None },
        )
    }

    /// `process_event` must return `Fatal` when the connector's state is
    /// `Disabled`, even if the event itself is well-formed.
    ///
    /// This covers the path that the poll-task closure takes on every tick
    /// after `disable()` has been called (the closure holds the real
    /// `Arc<ArcSwap<ConnectorState>>` and will observe the state change).
    #[tokio::test]
    async fn disabled_connector_poll_returns_fatal() {
        let consent: Arc<dyn ConnectorConsentJournal> = Arc::new(AcceptAllConsent::default());
        let emit: Arc<dyn PipelineEmit> = Arc::new(NoopEmit::default());
        let connector: Arc<dyn Connector> = Arc::new(StubConnector::new());

        // Start as Disabled (the default after register, before enable).
        let state: Arc<ArcSwap<ConnectorState>> =
            Arc::new(ArcSwap::from_pointee(ConnectorState::Disabled));

        let err = process_event(stub_event(), &connector, &state, &consent, &emit)
            .await
            .expect_err("process_event must fail when state is Disabled");

        assert!(
            matches!(err, ConnectorError::Fatal(_)),
            "expected Fatal, got {err:?}"
        );
    }

    /// `process_event` must return `Fatal` when `event.connector` does not
    /// match `connector.name()`, preventing a misbehaving connector from
    /// spoofing another's name to bypass consent checks.
    #[tokio::test]
    async fn connector_name_mismatch_returns_fatal() {
        let consent: Arc<dyn ConnectorConsentJournal> = Arc::new(AcceptAllConsent::default());
        let emit: Arc<dyn PipelineEmit> = Arc::new(NoopEmit::default());
        let connector: Arc<dyn Connector> = Arc::new(StubConnector::new()); // name == "stub"

        // The connector is enabled so state is not the failure cause.
        let state: Arc<ArcSwap<ConnectorState>> =
            Arc::new(ArcSwap::from_pointee(ConnectorState::Enabled {
                grant: stub_grant(),
                grant_id: ConsentGrantId::new("gnt:stub:test"),
            }));

        // Build an event that claims to come from a *different* connector.
        let mut spoofed = stub_event();
        spoofed.connector = "connector_b".to_owned();

        let err = process_event(spoofed, &connector, &state, &consent, &emit)
            .await
            .expect_err("process_event must fail on connector-name mismatch");

        assert!(
            matches!(err, ConnectorError::Fatal(_)),
            "expected Fatal on name mismatch, got {err:?}"
        );
    }
}
