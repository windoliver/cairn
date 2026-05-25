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
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use bon::Builder;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex as TokioMutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use cairn_core::contract::connector_consent::{
    ConnectorConsentJournal, ConnectorConsentLookup, ConsentGrant, ConsentGrantId,
};
use cairn_core::domain::capture::PayloadHash;

use crate::connector::{Connector, ConnectorPlugin, PollContext, WebhookContext};
use crate::credential::{CredentialHandle, CredentialStore};
use crate::emit::{PipelineEmit, build_capture_event};
use crate::error::ConnectorError;
use crate::event::ConnectorPayload;
use crate::rate_limit::RateLimit;
use crate::redact::RedactionPipeline;
use crate::webhook::{WebhookRequest, verify_hmac_sha256};

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
    /// Per-connector item-rate budget, initialized from
    /// `manifest.budget.max_items_per_hour` when the connector is enabled.
    ///
    /// `Arc` so the poll-task closure can hold a clone. Scopes are added
    /// lazily the first time an event is seen for that scope.
    ///
    /// TODO(#130-followup): wire `max_bytes_per_day` budget once the
    /// byte-budget tracker is implemented.
    rate_limit: Arc<RateLimit>,
    /// Opaque cursor returned by the most recent successful poll tick.
    ///
    /// The cursor is only advanced AFTER all events in the poll outcome have
    /// been successfully processed (at-least-once guarantee, Finding E).
    /// If any event fails, the cursor stays at its previous value so the next
    /// tick re-fetches the same upstream window.
    ///
    /// `Arc<TokioMutex<_>>` so the poll-task closure and the registry can
    /// both access the cursor without blocking across an `.await`.
    cursor: Arc<TokioMutex<Option<String>>>,
}

// ---------------------------------------------------------------------------
// Webhook shared state (Finding G)
// ---------------------------------------------------------------------------

/// Per-connector data cloned into the axum webhook handler via `Arc<State>`.
struct WebhookEntryState {
    /// Connector implementation (for `ingest_webhook` + manifest access).
    connector: Arc<dyn Connector>,
    /// Shared lifecycle state — the handler checks this is `Enabled` before
    /// processing the request.
    state: Arc<ArcSwap<ConnectorState>>,
    /// Per-connector item-rate bucket (shared with poll-task if any).
    rate_limit: Arc<RateLimit>,
    /// Pre-parsed maximum body byte count from `manifest.payload.max_bytes_parsed`.
    max_body_bytes: u64,
    /// HTTP header name that carries the HMAC-SHA256 signature, from
    /// `manifest.webhook.signature_header`.
    signature_header: String,
}

/// State for all webhook routes, shared via `Arc` across `axum::State`.
struct RegistryWebhookState {
    /// Per-connector handler data, keyed by connector name.
    entries: HashMap<String, WebhookEntryState>,
    /// Credential store for `connector/<name>/webhook_secret` lookups.
    credentials: Arc<dyn CredentialStore>,
    /// Consent journal used inside the handler's consent gate.
    consent: Arc<dyn ConnectorConsentJournal>,
    /// Downstream pipeline for emitting accepted events.
    emit: Arc<dyn PipelineEmit>,
    /// Root directory for spooling connector payload bytes (Finding H).
    spool_root: PathBuf,
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
/// 3. [`disable`][Self::disable] — stop the poll task, revoke the grant,
///    and mark the entry disabled.
/// 4. [`shutdown`][Self::shutdown] — cancel all tasks and await them.
#[derive(Builder)]
pub struct ConnectorRegistry {
    /// Credential store used to fetch OAuth tokens for poll calls and
    /// webhook secrets for HMAC verification.
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
    /// Root directory for spooling connector payload bytes before the final
    /// `CaptureEvent` is handed to the pipeline (Finding H).
    ///
    /// Files are written atomically (temp-then-rename) under
    /// `{spool_root}/connector/<name>/<event_id>_<hash8>.<ext>`. Tests
    /// should supply a `tempfile::tempdir()` path.
    ///
    /// Defaults to `.cairn/spool/connectors` relative to the working directory
    /// for production use.
    #[builder(default = PathBuf::from(".cairn/spool/connectors"))]
    spool_root: PathBuf,
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
    /// # Sensor-identity binding (Finding C)
    ///
    /// Three invariants are enforced before the plugin is stored:
    ///
    /// 1. The runtime `plugin.sensor_identity().as_str()` must equal
    ///    `plugin.manifest().connector.sensor_identity` (the value declared in
    ///    the TOML). A mismatch indicates a buggy or malicious adapter that
    ///    returns a different identity at runtime than it declares.
    /// 2. The identity wire form must begin with `"snr:local:connector:"` —
    ///    connectors must occupy the `local:connector:` root family (brief §4.2,
    ///    §19 v0.3).
    /// 3. The name segment immediately following `"snr:local:connector:"` must
    ///    equal `plugin.name()`. This prevents an adapter from borrowing another
    ///    connector's identity by declaring the right prefix but the wrong name
    ///    segment.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectorError::Fatal`] if:
    /// - A connector with the same name has already been registered (duplicate
    ///   names are not allowed).
    /// - Any of the three sensor-identity invariants above is violated.
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

        // --- Sensor-identity binding (Finding C) ----------------------------
        // Invariant 1: runtime identity must equal the manifest declaration.
        let runtime_id = plugin.sensor_identity().as_str();
        let declared_id = &plugin.manifest().connector.sensor_identity;
        if runtime_id != declared_id.as_str() {
            return Err(ConnectorError::fatal_msg(format!(
                "connector {name}: runtime sensor_identity \"{runtime_id}\" \
                 does not match manifest declaration \"{declared_id}\""
            )));
        }

        // Invariant 2 + 3: wire form must start with "snr:local:connector:<name>:v".
        //
        // Expected format: "snr:local:connector:<name>:v<n>"
        // We check that the prefix "snr:local:connector:" is present, that the
        // next colon-delimited segment equals `plugin.name()`, and that the
        // following segment starts with 'v'.
        let prefix = "snr:local:connector:";
        let rest = runtime_id.strip_prefix(prefix).ok_or_else(|| {
            ConnectorError::fatal_msg(format!(
                "connector {name}: sensor_identity \"{runtime_id}\" \
                 must begin with \"{prefix}\""
            ))
        })?;

        // Split off the name segment (everything before the next ':').
        let (id_name_seg, rest_after_name) = rest.split_once(':').ok_or_else(|| {
            ConnectorError::fatal_msg(format!(
                "connector {name}: sensor_identity \"{runtime_id}\" \
                 is missing the version segment after the connector-name segment"
            ))
        })?;

        if id_name_seg != name {
            return Err(ConnectorError::fatal_msg(format!(
                "connector {name}: sensor_identity \"{runtime_id}\" \
                 has name segment \"{id_name_seg}\" but plugin.name() is \"{name}\""
            )));
        }

        // The version segment must start with 'v'.
        if !rest_after_name.starts_with('v') {
            return Err(ConnectorError::fatal_msg(format!(
                "connector {name}: sensor_identity \"{runtime_id}\" \
                 version segment \"{rest_after_name}\" must start with 'v'"
            )));
        }
        // --- End sensor-identity binding ------------------------------------

        // Initialize the per-connector rate limit from the manifest budget.
        // Scopes are added lazily on first event via `RateLimit::ensure_scope`.
        let budget_capacity = plugin.manifest().budget.max_items_per_hour;
        let rate_limit = Arc::new(RateLimit::empty_per_hour(budget_capacity));
        self.entries.insert(
            name,
            Entry {
                connector: Arc::new(plugin),
                state: Arc::new(ArcSwap::from_pointee(ConnectorState::Disabled)),
                poll_token: None,
                poll_handle: None,
                rate_limit,
                cursor: Arc::new(TokioMutex::new(None)),
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
            let rate_limit = Arc::clone(&entry.rate_limit);
            let cursor_ref = Arc::clone(&entry.cursor);
            let spool_root = self.spool_root.clone();
            let name_owned = name.to_owned();
            let interval = Duration::from_mins(5);
            // Clone the token for the task; store the original for cancel.
            let task_token = entry_token.clone();

            // # At-least-once delivery guarantee (Finding E)
            //
            // The cursor is only advanced AFTER all events returned by a poll
            // tick have been successfully processed. If any `process_event`
            // call fails (validation, consent, rate-limit, redaction, or emit),
            // the cursor remains at its previous value and the next tick
            // re-fetches the same upstream window.
            //
            // Upstream sources may therefore deliver duplicate events across
            // tick boundaries — that is the accepted trade-off for at-least-once
            // semantics. `BudgetExceeded` is treated identically to transient
            // failures: the cursor stays put and the operator can adjust the
            // budget so events are re-attempted on the next tick.
            let handle = tokio::spawn(async move {
                loop {
                    tokio::select! {
                        // Cancellation arm: per-entry token or registry shutdown.
                        () = task_token.cancelled() => break,
                        () = tokio::time::sleep(interval) => {
                            // Read the current cursor before calling poll so
                            // the upstream window starts from where we left off.
                            let last_cursor = cursor_ref.lock().await.clone();
                            let cx = PollContext {
                                credentials: Arc::new(CredentialHandle::empty()),
                                last_cursor,
                                budget_remaining_items: u32::MAX,
                            };
                            match connector.poll(&cx).await {
                                Ok(outcome) => {
                                    // Track whether all events were accepted.
                                    let mut all_accepted = true;
                                    for event in outcome.events {
                                        if let Err(err) = process_event(
                                            event, &connector, &state, &consent, &emit,
                                            &rate_limit, &spool_root,
                                        ).await {
                                            tracing::warn!(
                                                connector = %name_owned,
                                                ?err,
                                                "process_event error; cursor held — next tick will retry",
                                            );
                                            all_accepted = false;
                                            // Do not advance cursor; break out
                                            // of the event loop for this tick.
                                            break;
                                        }
                                    }
                                    // Only advance the cursor when every event
                                    // in this tick was durably accepted.
                                    if all_accepted {
                                        *cursor_ref.lock().await = outcome.next_cursor;
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

    /// Disable a connector: stop the poll task, then revoke its consent grant.
    ///
    /// # Ordering guarantee (Finding F)
    ///
    /// The steps are intentionally ordered so that the local poll task is
    /// stopped **before** the consent journal is contacted:
    ///
    /// 1. Capture the current `grant_id` (if any) from the entry state.
    /// 2. Flip the entry state to [`Disabled`][ConnectorState::Disabled].
    /// 3. Cancel the per-entry [`CancellationToken`] so the poll task exits at
    ///    its next `select!` branch.
    /// 4. Await the [`JoinHandle`] — the task has actually exited.
    /// 5. Call [`ConnectorConsentJournal::revoke`] — if this returns an error,
    ///    surface it to the caller, but the local stop has already succeeded.
    ///
    /// This ordering means an operator who calls `disable` on a misbehaving
    /// connector is guaranteed that the task stops even if the consent journal
    /// is temporarily unavailable. The revoke error is surfaced so the caller
    /// knows the journal entry was not cleaned up and can retry.
    ///
    /// After this call returns (whether `Ok` or `Err` from revoke), no further
    /// poll ticks or upstream HTTP calls will be made by the stopped task.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectorError::Fatal`] if:
    /// - The connector name is not registered.
    /// - The poll task panicked (`JoinError`).
    /// - The consent journal fails to revoke the grant (surfaced after the
    ///   local stop has succeeded).
    pub async fn disable(&mut self, name: &str) -> Result<(), ConnectorError> {
        let entry = self
            .entries
            .get_mut(name)
            .ok_or_else(|| ConnectorError::fatal_msg(format!("unknown connector {name}")))?;

        // Step 1: capture the grant_id before mutating state.
        let grant_id = match (**entry.state.load()).clone() {
            ConnectorState::Enabled { grant_id, .. } => Some(grant_id),
            ConnectorState::Disabled => None,
        };

        // Step 2: flip state to Disabled immediately so in-flight process_event
        // calls see the new state and the poll task stops accepting new work.
        entry.state.store(Arc::new(ConnectorState::Disabled));

        // Steps 3 + 4: cancel the per-entry poll task and await its exit.
        // This ensures the task has truly stopped before we return — no
        // further upstream calls can be in flight after this point.
        if let Some(token) = entry.poll_token.take() {
            token.cancel();
        }
        if let Some(handle) = entry.poll_handle.take() {
            // A JoinError here means the task panicked — treat as Fatal.
            handle.await.map_err(|e| {
                ConnectorError::fatal_msg(format!(
                    "poll task for {name} panicked during disable: {e}"
                ))
            })?;
        }

        // Step 5: revoke the consent grant in the journal. This happens after
        // the local stop so a journal outage never leaves the task running.
        // Surface any revoke error to the caller so they know the journal
        // entry was not cleaned up.
        if let Some(gid) = grant_id {
            self.consent
                .revoke(&gid)
                .await
                .map_err(ConnectorError::fatal_msg)?;
        }

        Ok(())
    }

    /// Build the composed `axum::Router` for all currently-enabled webhook
    /// connectors (Finding G).
    ///
    /// Each enabled connector that advertises `capabilities().webhook == true`
    /// gets a `POST /webhooks/<name>` route that:
    ///
    /// 1. Reads the request body, bounded by `manifest.payload.max_bytes_parsed`
    ///    (returns `413 Payload Too Large` if the body exceeds the cap).
    /// 2. Looks up the webhook secret via
    ///    `CredentialStore::get("connector/<name>/webhook_secret")`.
    ///    Returns `401 Unauthorized` if the secret is not provisioned.
    /// 3. Verifies the HMAC-SHA256 signature using
    ///    `manifest.webhook.signature_header` (returns `401` on mismatch).
    /// 4. Calls `connector.ingest_webhook` and routes each returned event
    ///    through `process_event` (consent, label, redaction, spool, emit).
    /// 5. Returns `204 No Content` on full success; maps errors to appropriate
    ///    HTTP status codes per the single-rejection-reason rule.
    ///
    /// Disabled connectors are not mounted; their paths return `404 Not Found`
    /// from axum's fallback.
    ///
    /// Callers (e.g. `cairn-cli`) mount the returned router into their axum
    /// server without any additional webhook-specific middleware — all gates
    /// are applied inside the registry.
    pub fn webhook_router(&self) -> axum::Router {
        use crate::webhook::WebhookRouter;
        use axum::extract::State;
        use axum::routing::post;

        // Collect per-connector state for enabled webhook connectors.
        let mut entry_states: HashMap<String, WebhookEntryState> = HashMap::new();
        for (name, entry) in &self.entries {
            if !entry.connector.capabilities().webhook {
                continue;
            }
            if !matches!(**entry.state.load(), ConnectorState::Enabled { .. }) {
                continue;
            }
            entry_states.insert(
                name.clone(),
                WebhookEntryState {
                    connector: Arc::clone(&entry.connector),
                    state: Arc::clone(&entry.state),
                    rate_limit: Arc::clone(&entry.rate_limit),
                    max_body_bytes: entry.connector.manifest().payload.max_bytes_parsed,
                    signature_header: entry.connector.manifest().webhook.signature_header.clone(),
                },
            );
        }

        // Wrap shared state in Arc for axum State injection.
        let shared = Arc::new(RegistryWebhookState {
            entries: entry_states,
            credentials: Arc::clone(&self.credentials),
            consent: Arc::clone(&self.consent),
            emit: Arc::clone(&self.emit),
            spool_root: self.spool_root.clone(),
        });

        let mut wr = WebhookRouter::new();
        // Mount one POST route per enabled webhook connector.
        // The connector name is captured in the closure rather than extracted
        // from the path, because each route is mounted at a fixed path
        // (e.g. `/webhooks/fixture`) with no route parameter — axum's `Path`
        // extractor requires a `{param}` placeholder in the route template.
        for name in shared.entries.keys() {
            let path = format!("/webhooks/{name}");
            let shared_clone = Arc::clone(&shared);
            let connector_name = name.clone();
            let route = axum::Router::new()
                .route(
                    &path,
                    post(
                        move |State(st): State<Arc<RegistryWebhookState>>,
                              req: axum::http::Request<axum::body::Body>| {
                            handle_webhook(st, connector_name, req)
                        },
                    ),
                )
                .with_state(shared_clone);
            wr.mount(route);
        }
        wr.into_router()
    }

    /// **Test-only.** Triggers one poll cycle without waiting for the
    /// scheduler interval. Real production use happens via the scheduler
    /// spawned by `enable`.
    ///
    /// Applies the same cursor-advance semantics as the background poll task:
    /// the per-entry cursor is only updated if all events in the outcome are
    /// successfully processed. A failure on any event leaves the cursor
    /// unchanged so the next call retries from the same position.
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

        let last_cursor = entry.cursor.lock().await.clone();
        let cx = crate::connector::PollContext {
            credentials: Arc::new(crate::credential::CredentialHandle::empty()),
            last_cursor,
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
                &entry.rate_limit,
                &self.spool_root,
            )
            .await?;
        }
        // Only advance the cursor if all events were accepted (no early return
        // above via `?`).
        *entry.cursor.lock().await = outcome.next_cursor;
        Ok(())
    }

    /// **Test-only.** Return the current per-entry cursor for `name`, or
    /// `None` if no tick has completed successfully yet.
    ///
    /// Returns `None` (the outer `Option`) if no connector with the given name
    /// is registered.
    #[cfg(any(test, feature = "fixture"))]
    pub async fn entry_cursor(&self, name: &str) -> Option<Option<String>> {
        let entry = self.entries.get(name)?;
        Some(entry.cursor.lock().await.clone())
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
// Payload spool helper (Finding H)
// ---------------------------------------------------------------------------

/// Extract the post-redaction wire bytes from a `Text` or `Json` payload and
/// return them with the appropriate file extension for the spool filename.
///
/// - [`ConnectorPayload::Text`] → UTF-8 bytes + `"txt"`.
/// - [`ConnectorPayload::Json`] → `serde_json::to_vec` bytes + `"json"`.
///   Deterministic for the same value (`serde_json` preserves key order).
///
/// **Binary payloads must NOT be passed to this function.** Binary payloads
/// are handled separately in `process_event` using the adapter-declared
/// `sha256` field (Finding D).
///
/// # Errors
///
/// - `Json` → `ConnectorError::Fatal` if `serde_json` fails (unreachable for a
///   value already parsed from JSON).
/// - `Binary` → `ConnectorError::Fatal` (programming error — callers must
///   branch on `Binary` before calling this function).
fn payload_to_bytes(payload: &ConnectorPayload) -> Result<(Vec<u8>, &'static str), ConnectorError> {
    match payload {
        ConnectorPayload::Text { body, .. } => Ok((body.as_bytes().to_vec(), "txt")),
        ConnectorPayload::Json { body, .. } => {
            let bytes = serde_json::to_vec(body).map_err(|e| {
                ConnectorError::fatal_msg(format!(
                    "JSON serialisation failed during spool byte extraction: {e}"
                ))
            })?;
            Ok((bytes, "json"))
        }
        ConnectorPayload::Binary { .. } => Err(ConnectorError::fatal_msg(
            "invariant: payload_to_bytes must not be called with a Binary payload; \
             Binary payloads are handled via the adapter-declared sha256 field (Finding D)"
                .to_owned(),
        )),
    }
}

/// Write `bytes` to `{spool_root}/{relative_path}` atomically.
///
/// Uses a write-to-temp-then-rename pattern so readers never observe a
/// partially-written file. The temp file is placed in the same directory as
/// the final path (same filesystem for rename) with a unique per-invocation
/// suffix to prevent races when multiple writers target the same path.
///
/// # Errors
///
/// Returns [`ConnectorError::Fatal`] if directory creation, the write, or the
/// rename fails.
async fn spool_bytes(
    spool_root: &std::path::Path,
    relative_path: &str,
    bytes: &[u8],
) -> Result<(), ConnectorError> {
    use std::sync::atomic::{AtomicU64, Ordering};

    // Per-process monotonic counter, combined with the PID, gives a unique
    // suffix for the temp file so concurrent writers never collide.
    static SPOOL_SEQ: AtomicU64 = AtomicU64::new(0);

    let final_path = spool_root.join(relative_path);
    let parent = final_path.parent().ok_or_else(|| {
        ConnectorError::fatal_msg(format!(
            "spool path {} has no parent directory",
            final_path.display()
        ))
    })?;
    tokio::fs::create_dir_all(parent).await.map_err(|e| {
        ConnectorError::fatal_msg(format!(
            "failed to create spool dir {}: {e}",
            parent.display()
        ))
    })?;
    // Use a per-invocation unique suffix to avoid races between concurrent
    // writers that share a target path (e.g. parallel integration tests run
    // in separate processes via nextest).
    //
    // The suffix combines the OS process ID (unique per-process) with a
    // process-local atomic counter (unique within a process), giving a suffix
    // that is unique across all concurrent writers regardless of scheduling.
    let seq = SPOOL_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let tmp_name = format!(
        "{}.{pid}.{seq}.tmp",
        final_path.file_name().unwrap_or_default().to_string_lossy()
    );
    let tmp_path = parent.join(tmp_name);
    tokio::fs::write(&tmp_path, bytes).await.map_err(|e| {
        ConnectorError::fatal_msg(format!(
            "failed to write spool temp {}: {e}",
            tmp_path.display()
        ))
    })?;
    tokio::fs::rename(&tmp_path, &final_path)
        .await
        .map_err(|e| {
            ConnectorError::fatal_msg(format!(
                "failed to rename spool temp {} → {}: {e}",
                tmp_path.display(),
                final_path.display()
            ))
        })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared event-processing helper
// ---------------------------------------------------------------------------

/// Validate, redact, consent-check, spool, and emit one [`ConnectorEvent`].
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
/// 5. Spool the post-redaction bytes to `{spool_root}/connector/<name>/…` and
///    compute a real SHA-256 hash from those bytes (Finding H). The hash
///    is guaranteed to match the bytes on disk.
/// 6. Call [`build_capture_event`] with the spooled path and real hash.
/// 7. Call `emit.emit(captured)` to hand the event to the pipeline.
///
/// [`ConnectorEvent`]: crate::event::ConnectorEvent
async fn process_event(
    event: crate::event::ConnectorEvent,
    connector: &Arc<dyn Connector>,
    state: &Arc<ArcSwap<ConnectorState>>,
    consent: &Arc<dyn ConnectorConsentJournal>,
    emit: &Arc<dyn PipelineEmit>,
    rate_limit: &Arc<RateLimit>,
    spool_root: &std::path::Path,
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

    // 3c. Rate-limit charge — deduct one item token from the per-scope bucket
    //     (Finding A). Scopes are registered lazily on first event so the
    //     registry does not need to enumerate them up front.
    rate_limit.ensure_scope(&scope_key);
    rate_limit.charge(&scope_key, 1)?;

    // 4. Redact PII before the event crosses any boundary (brief §5.2 + §14).
    //    Use the manifest's max_depth limit for the JSON walker.
    let redacted = RedactionPipeline::new()
        .with_max_depth(connector.manifest().payload.max_depth)
        .redact(event)?;

    // 5. Compute the payload hash and spool the post-redaction bytes (Finding D +
    //    Finding H).
    //
    //    - Text / Json: compute a real SHA-256 from the wire bytes and write them
    //      atomically to `spool_root`. The hash is guaranteed to match the bytes on
    //      disk once the spool write completes.
    //    - Binary: the adapter declares the content's SHA-256 in the payload
    //      `sha256` field. The framework trusts this value and uses it directly as
    //      the `PayloadHash` (no re-hash — the spool verification layer in #131
    //      will cross-check it against the actual content addressed by `bytes_ref`).
    //      The `bytes_ref` is forwarded as the `payload_ref` so downstream can
    //      locate the content.
    let connector_name = &redacted.event.connector;
    let event_id = redacted.event.event_id.as_str();

    let (payload_ref, payload_hash) = match &redacted.event.payload {
        ConnectorPayload::Binary {
            sha256, bytes_ref, ..
        } => {
            // Binary: use the adapter-declared hash; forward bytes_ref under the
            // `sources/` vault prefix required by `build_capture_event` (brief §3).
            // Spool write is deferred to #131 (full binary spool verification).
            let hash_str = if sha256.starts_with("sha256:") {
                sha256.clone()
            } else {
                format!("sha256:{sha256}")
            };
            let ph = PayloadHash::parse(hash_str).map_err(|e| {
                ConnectorError::fatal_msg(format!("Binary sha256 parse error: {e}"))
            })?;
            // Ensure the vault-relative path starts with `sources/` (brief §3).
            let pr = if bytes_ref.starts_with("sources/") {
                bytes_ref.clone()
            } else {
                format!("sources/{bytes_ref}")
            };
            (pr, ph)
        }
        text_or_json => {
            // Text / Json: compute real SHA-256 from the wire bytes and spool them.
            let (bytes, ext) = payload_to_bytes(text_or_json)?;
            let digest = Sha256::digest(&bytes);
            let hash_hex = hex::encode(digest);
            let hash_str = format!("sha256:{hash_hex}");
            // Use the first 8 hex chars in the spool filename for fast duplicate
            // detection by name (without parsing the hash field).
            let hash_short = &hash_hex[..8];

            // Relative path inside the spool root; mirrors `payload_ref` without the
            // leading `sources/` so the same relative path works for both the spool
            // file and the vault reference.
            let spool_relative =
                format!("connector/{connector_name}/{event_id}_{hash_short}.{ext}");

            // Atomically write to spool so the hash invariant holds: a reader that
            // observes the file sees exactly the bytes we hashed.
            spool_bytes(spool_root, &spool_relative, &bytes).await?;

            // Vault-relative path starts with `sources/` (brief §3 trust boundary).
            let pr = format!("sources/{spool_relative}");
            let ph = PayloadHash::parse(hash_str)
                .map_err(|e| ConnectorError::fatal_msg(format!("payload hash parse error: {e}")))?;
            (pr, ph)
        }
    };

    // 6. Build a CaptureEvent with the real hash and spooled path.
    let captured = build_capture_event(
        &redacted.event,
        connector.sensor_identity(),
        redacted.spans,
        &payload_ref,
        payload_hash,
    )?;

    // 7. Hand the event to the downstream pipeline.
    emit.emit(captured).await
}

// ---------------------------------------------------------------------------
// Webhook handler (Finding G)
// ---------------------------------------------------------------------------

/// `POST /webhooks/<connector_name>` — full HMAC verification + consent gate
/// + ingest + spool + emit pipeline.
///
/// Injected with `Arc<RegistryWebhookState>` via axum `State`. The connector
/// name comes from the URL path parameter.
///
/// # Response codes
///
/// | Condition                              | Status |
/// |----------------------------------------|--------|
/// | Body exceeds `max_bytes_parsed`        | 413    |
/// | Secret not provisioned in store        | 401    |
/// | Credential lookup error                | 401    |
/// | HMAC signature mismatch                | 401    |
/// | `ingest_webhook` rate-limited          | 429    |
/// | `ingest_webhook` auth error            | 401    |
/// | `ingest_webhook` malformed payload     | 400    |
/// | `ingest_webhook` budget exceeded       | 429    |
/// | `ingest_webhook` consent revoked       | 403    |
/// | `ingest_webhook` fatal/other           | 500    |
/// | `process_event` rate/budget            | 429    |
/// | `process_event` consent revoked        | 403    |
/// | `process_event` payload/label error    | 422    |
/// | `process_event` auth error             | 401    |
/// | `process_event` other                  | 422    |
/// | All events accepted                    | 204    |
async fn handle_webhook(
    state: Arc<RegistryWebhookState>,
    connector_name: String,
    req: axum::http::Request<axum::body::Body>,
) -> axum::http::Response<axum::body::Body> {
    use axum::body::Body;
    use axum::http::{Response, StatusCode};

    /// Build a plain-text response.
    fn resp(status: StatusCode, body: &'static str) -> Response<Body> {
        Response::builder()
            .status(status)
            .body(Body::from(body))
            .expect("invariant: static response builder cannot fail")
    }

    // Look up the connector entry.
    let Some(entry) = state.entries.get(&connector_name) else {
        return resp(StatusCode::NOT_FOUND, "connector not found");
    };

    // Guard: connector must still be enabled.
    if !matches!(**entry.state.load(), ConnectorState::Enabled { .. }) {
        return resp(StatusCode::NOT_FOUND, "connector not enabled");
    }

    // Collect headers before consuming the body.
    let raw_headers: Vec<(String, String)> = req
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|v| (name.as_str().to_owned(), v.to_owned()))
        })
        .collect();

    // Read body with a size cap (Finding G §1). axum 0.8 signature:
    // `axum::body::to_bytes(body, limit) -> Result<Bytes, _>`.
    // Saturate at `usize::MAX` on 32-bit targets — practical manifests are
    // well within 4 GiB so this branch is never taken in production.
    let body_limit = usize::try_from(entry.max_body_bytes).unwrap_or(usize::MAX);
    let Ok(body_bytes) = axum::body::to_bytes(req.into_body(), body_limit).await else {
        return resp(StatusCode::PAYLOAD_TOO_LARGE, "body exceeds limit");
    };

    // Fetch the webhook secret from the credential store (Finding G §3).
    let secret_key = format!("connector/{connector_name}/webhook_secret");
    let credential = match state.credentials.get(&secret_key).await {
        Ok(c) => c,
        Err(ConnectorError::AuthExpired { .. }) => {
            return resp(StatusCode::UNAUTHORIZED, "webhook secret not provisioned");
        }
        Err(_) => return resp(StatusCode::UNAUTHORIZED, "credential lookup failed"),
    };

    // Verify HMAC-SHA256 (Finding G §4). Single rejection reason per spec.
    let webhook_req = WebhookRequest {
        connector: connector_name.clone(),
        body: body_bytes.to_vec(),
        headers: raw_headers,
    };
    if verify_hmac_sha256(&webhook_req, &entry.signature_header, credential.bytes()).is_err() {
        return resp(StatusCode::UNAUTHORIZED, "signature mismatch");
    }

    // Build WebhookContext and call ingest_webhook (Finding G §5-6).
    let webhook_cx = WebhookContext {
        credentials: credential,
        budget_remaining_items: u32::MAX,
    };
    let events = match entry
        .connector
        .ingest_webhook(&webhook_req, &webhook_cx)
        .await
    {
        Ok(evs) => evs,
        Err(ConnectorError::RateLimited { .. } | ConnectorError::BudgetExceeded { .. }) => {
            return resp(StatusCode::TOO_MANY_REQUESTS, "rate limited");
        }
        Err(ConnectorError::SignatureMismatch | ConnectorError::AuthExpired { .. }) => {
            return resp(StatusCode::UNAUTHORIZED, "auth error");
        }
        Err(ConnectorError::MalformedPayload(_)) => {
            return resp(StatusCode::BAD_REQUEST, "malformed payload");
        }
        Err(ConnectorError::ConsentRevoked { .. }) => {
            return resp(StatusCode::FORBIDDEN, "consent revoked");
        }
        Err(_) => return resp(StatusCode::INTERNAL_SERVER_ERROR, "ingest error"),
    };

    // Route each event through process_event (Finding G §7-8).
    for event in events {
        match process_event(
            event,
            &entry.connector,
            &entry.state,
            &state.consent,
            &state.emit,
            &entry.rate_limit,
            &state.spool_root,
        )
        .await
        {
            Ok(()) => {}
            Err(ConnectorError::RateLimited { .. } | ConnectorError::BudgetExceeded { .. }) => {
                return resp(StatusCode::TOO_MANY_REQUESTS, "rate limited");
            }
            Err(ConnectorError::ConsentRevoked { .. }) => {
                return resp(StatusCode::FORBIDDEN, "consent revoked");
            }
            Err(ConnectorError::SignatureMismatch | ConnectorError::AuthExpired { .. }) => {
                return resp(StatusCode::UNAUTHORIZED, "auth error");
            }
            // 422 for payload/label/other errors; don't leak which gate failed.
            Err(_) => return resp(StatusCode::UNPROCESSABLE_ENTITY, "event rejected"),
        }
    }

    // All events accepted → 204 No Content (Finding G §9).
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .body(Body::empty())
        .expect("invariant: 204 response builder cannot fail")
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

        let tmp = tempfile::tempdir().expect("tempdir");
        let rate_limit = Arc::new(RateLimit::empty_per_hour(u32::MAX));
        let err = process_event(
            stub_event(),
            &connector,
            &state,
            &consent,
            &emit,
            &rate_limit,
            tmp.path(),
        )
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

        let tmp = tempfile::tempdir().expect("tempdir");
        let rate_limit = Arc::new(RateLimit::empty_per_hour(u32::MAX));
        let err = process_event(
            spoofed,
            &connector,
            &state,
            &consent,
            &emit,
            &rate_limit,
            tmp.path(),
        )
        .await
        .expect_err("process_event must fail on connector-name mismatch");

        assert!(
            matches!(err, ConnectorError::Fatal(_)),
            "expected Fatal on name mismatch, got {err:?}"
        );
    }
}
