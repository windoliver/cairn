//! Finding Z — event-id reuse with different content must be detected and rejected.
//!
//! If a connector emits two events with the same `event_id` but different
//! payload bytes, the second attempt would write a different spool path (because
//! the hash differs) AND emit a second `CaptureEvent` with the same logical
//! identity — producing inconsistent capture-event identity.
//!
//! The fix adds a per-connector in-memory event-id index that maps each
//! `event_id` to the first hash seen for that id.  Before every spool write the
//! index is consulted:
//!
//! - Absent: insert and proceed (first time).
//! - Present, hash matches: idempotent retry (same content), proceed.
//! - Present, hash differs: return `ConnectorError::Fatal`; no spool file,
//!   no `CaptureEvent`.
//!
//! Issue #130, brief §9.1, Round-6 review Finding Z.

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
// Shared manifest (poll only)
// ---------------------------------------------------------------------------

/// The `event_id` used in both tests — a valid ULID.
const REUSE_EVENT_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

const MANIFEST: &str = r#"
[connector]
name = "reuse-test"
contract = "Connector"
contract_version = "0.1.0"
sensor_identity = "snr:local:connector:reuse-test:v1"

[capabilities]
poll = true
webhook = false
backfill = false

[oauth]
required_scopes = []
token_lifetime = "1h"
refresh = false

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
        "reuse-test",
        make_manifest().hash(),
        BTreeSet::from(["note".to_string()]),
        vec!["project:*".to_string()],
        1_700_000_000,
        Identity::parse("hmn:alice").expect("invariant"),
    )
}

// ---------------------------------------------------------------------------
// Connector that emits the SAME event_id with a configurable payload body
// ---------------------------------------------------------------------------

/// A connector whose `poll` returns a single event with `event_id =
/// REUSE_EVENT_ID` and a payload body controlled by `body_value`.
///
/// The `body_value` is shared via `Arc<StdMutex<u32>>` so the test can change
/// what the connector returns between successive `poll_now` calls without
/// needing to re-register the connector.
struct ReuseTestConnector {
    manifest: ConnectorManifest,
    sensor: Identity,
    /// The integer value embedded in the JSON payload `{"v": <body_value>}`.
    /// Wrapped in `Arc<Mutex>` so the test can change it between polls.
    body_value: Arc<StdMutex<u32>>,
}

impl ReuseTestConnector {
    fn new(body_value: u32) -> (Self, Arc<StdMutex<u32>>) {
        let shared = Arc::new(StdMutex::new(body_value));
        let connector = Self {
            manifest: make_manifest(),
            sensor: Identity::parse("snr:local:connector:reuse-test:v1").expect("sensor identity"),
            body_value: Arc::clone(&shared),
        };
        (connector, shared)
    }
}

#[async_trait]
impl Connector for ReuseTestConnector {
    fn name(&self) -> &'static str {
        "reuse-test"
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
        let v = *self.body_value.lock().expect("body_value mutex unpoisoned");
        Ok(PollOutcome {
            events: vec![ConnectorEvent::new(
                ConnectorEventId::new(REUSE_EVENT_ID),
                "reuse-test",
                SourceRef::new("issue", "x", None),
                0,
                BTreeSet::from(["note".to_string()]),
                ConnectorScope::project("owner/repo"),
                ConnectorPayload::Json {
                    mime: "application/json".into(),
                    body: serde_json::json!({"v": v}),
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

impl ConnectorPlugin for ReuseTestConnector {
    const NAME: &'static str = "reuse-test";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
}

// ---------------------------------------------------------------------------
// Counting emit — verifies how many CaptureEvents were emitted
// ---------------------------------------------------------------------------

/// Records how many `CaptureEvent`s were emitted.
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
// Tests
// ---------------------------------------------------------------------------

/// Emitting the same `event_id` with DIFFERENT payload content must be rejected
/// with `ConnectorError::Fatal`.
///
/// The second `poll_now` call must:
/// - Return `Err(Fatal)`.
/// - Not emit a second `CaptureEvent`.
/// - Not write a second spool file.
///
/// Finding Z: the per-connector event-id index detects the hash mismatch before
/// the spool write and returns `Fatal` immediately, preserving capture-event
/// identity integrity.
#[tokio::test]
async fn event_id_reuse_with_different_content_rejected() {
    let emit = Arc::new(CountEmit::default());
    let tmp = tempfile::tempdir().expect("tempdir");

    // Build a registry with a connector whose body_value can be mutated
    // between polls via the shared `Arc<Mutex<u32>>`.
    let (connector, body_ctrl) = ReuseTestConnector::new(1);
    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(emit.clone() as Arc<dyn PipelineEmit>)
        .spool_root(tmp.path().to_path_buf())
        .build();

    reg.register(connector).expect("register must succeed");
    reg.enable("reuse-test", make_grant())
        .await
        .expect("enable must succeed");

    // First poll: body `{"v": 1}` — must succeed.
    reg.poll_now("reuse-test")
        .await
        .expect("first poll_now with body {v:1} must succeed");

    let count_after_first = *emit.0.lock().expect("mutex unpoisoned");
    assert_eq!(
        count_after_first, 1,
        "exactly 1 CaptureEvent must be emitted after the first poll (got {count_after_first})"
    );

    // Change the body to `{"v": 2}` — same event_id, different content.
    *body_ctrl.lock().expect("body_ctrl mutex unpoisoned") = 2;

    // Second poll: same event_id, body `{"v": 2}` → different hash → must fail.
    let err = reg
        .poll_now("reuse-test")
        .await
        .expect_err("second poll_now with different content must be rejected");

    assert!(
        matches!(err, ConnectorError::Fatal(_)),
        "expected ConnectorError::Fatal for event_id reuse with different content, got {err:?}"
    );

    // No additional CaptureEvent must have been emitted.
    let count_after_second = *emit.0.lock().expect("mutex unpoisoned");
    assert_eq!(
        count_after_second, 1,
        "no second CaptureEvent must be emitted when event_id is reused with different content \
         (got {count_after_second}; expected 1)"
    );

    // No second spool file for the same event_id with a different hash should exist.
    // The spool directory may contain the first committed file, but must not contain
    // a second file for body_value=2 (different hash → different filename).
    // The Finding Z check fires BEFORE the spool write, so no orphan file is created.
    let connector_spool = tmp.path().join("connector").join("reuse-test");
    if connector_spool.exists() {
        let entries: Vec<_> = std::fs::read_dir(&connector_spool)
            .expect("read_dir")
            .filter_map(std::result::Result::ok)
            .filter(|e| !e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        // Only the first committed file should exist (for body_value=1).
        assert_eq!(
            entries.len(),
            1,
            "only 1 committed spool file must exist after rejecting the reuse attempt \
             (found {}: {:?})",
            entries.len(),
            entries
                .iter()
                .map(std::fs::DirEntry::file_name)
                .collect::<Vec<_>>()
        );
    }

    reg.shutdown().await;
}

/// Emitting the same `event_id` with IDENTICAL payload content must succeed on
/// the second call (idempotent retry).
///
/// Finding Z idempotent path: if the hash matches the stored value, the event-id
/// index treats the call as a legitimate retry and allows it to proceed.  Both
/// `poll_now` calls must succeed, each emitting one `CaptureEvent`.
///
/// The spool rename is idempotent for identical content (Finding X): the second
/// rename either collides on the same filename (content-addressed) or the
/// collision-check path deletes the tmp file and falls through — either way, no
/// error is returned.
#[tokio::test]
async fn event_id_reuse_with_identical_content_is_idempotent() {
    let emit = Arc::new(CountEmit::default());
    let tmp = tempfile::tempdir().expect("tempdir");

    // Both polls emit the SAME event_id with the SAME body (body_value=1).
    let (connector, _body_ctrl) = ReuseTestConnector::new(1);
    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(emit.clone() as Arc<dyn PipelineEmit>)
        .spool_root(tmp.path().to_path_buf())
        .build();

    reg.register(connector).expect("register must succeed");
    reg.enable("reuse-test", make_grant())
        .await
        .expect("enable must succeed");

    // First poll with body `{"v": 1}` — must succeed.
    reg.poll_now("reuse-test")
        .await
        .expect("first poll_now must succeed");

    // Second poll with the SAME event_id and SAME body — must also succeed.
    // This simulates an at-least-once retry where the connector re-delivers an
    // event that the framework already committed.  The event-id index sees the
    // matching hash and allows the retry.
    reg.poll_now("reuse-test")
        .await
        .expect("second poll_now with identical content must succeed (idempotent retry)");

    // Both polls emitted successfully — each poll_now call drives one
    // process_event call, which calls emit on success. Total emit count = 2.
    let count = *emit.0.lock().expect("mutex unpoisoned");
    assert_eq!(
        count, 2,
        "both idempotent retries must emit a CaptureEvent (got {count}; expected 2)"
    );

    reg.shutdown().await;
}
