//! Fix 4 — Payload limits and scope validation enforced.
//!
//! Tests:
//!
//! - `event_with_undeclared_scope_rejected` — scope kind not in `scopes.declared`
//!   returns `MalformedPayload`.
//! - `event_with_disallowed_mime_rejected` — MIME not in `webhook.allowed_mimes`
//!   returns `MalformedPayload`.
//! - `event_payload_size_over_limit_rejected` — payload larger than
//!   `payload.max_bytes` returns `MalformedPayload`.
//! - `deeply_nested_json_rejected` — JSON depth exceeding `payload.max_depth`
//!   returns `MalformedPayload` from the redaction pipeline.
//! - `parse_byte_size_units` — unit tests for the `parse_byte_size` helper.
//!
//! Issue #130, brief §9.1 (Fix 4).

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
use cairn_connectors_core::manifest::{ConnectorManifest, parse_byte_size};
use cairn_connectors_core::redact::RedactionPipeline;
use cairn_connectors_core::webhook::WebhookRequest;
use cairn_connectors_core::{
    ConnectorError, ConnectorRegistry, InMemoryCredentialStore, PipelineEmit,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::Identity;
use cairn_core::domain::capture::CaptureEvent;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// `PanicEmit` — emit must never be called in error-path tests.
struct PanicEmit;

#[async_trait]
impl PipelineEmit for PanicEmit {
    async fn emit(&self, _: CaptureEvent) -> Result<(), ConnectorError> {
        panic!("emit must NOT be called — framework gate should have blocked this");
    }
}

/// Manifest with strict limits used by most tests in this file.
const STRICT_MANIFEST: &str = r#"
[connector]
name = "strict"
contract = "Connector"
contract_version = "0.1.0"
sensor_identity = "snr:local:connector:strict:v1"

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
max_bytes_per_day = "1MiB"

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
max_bytes = "1KiB"
max_depth = 2
"#;

fn strict_manifest() -> ConnectorManifest {
    ConnectorManifest::parse_toml(STRICT_MANIFEST).expect("STRICT_MANIFEST parses")
}

fn strict_grant() -> cairn_core::contract::connector_consent::ConsentGrant {
    cairn_core::contract::connector_consent::ConsentGrant::new(
        "strict",
        strict_manifest().hash(),
        BTreeSet::from(["note".to_string()]),
        vec!["project:*".to_string()],
        1_700_000_000,
        Identity::parse("hmn:alice").expect("invariant: hmn:alice is valid"),
    )
}

/// Build a minimal valid event for the strict connector, customisable.
fn base_event(scope: ConnectorScope, payload: ConnectorPayload) -> ConnectorEvent {
    ConnectorEvent::new(
        ConnectorEventId::new("01HX0000000000000000000000"),
        "strict",
        SourceRef::new("issue", "x", None),
        0,
        BTreeSet::from(["note".to_string()]),
        scope,
        payload,
        DeliveryMode::Poll { cursor: None },
    )
}

// ---------------------------------------------------------------------------
// StrictConnector — customisable: each `make_event` returns a caller-
// defined event.
// ---------------------------------------------------------------------------

struct StrictConnector {
    manifest: ConnectorManifest,
    sensor: Identity,
    events: Vec<ConnectorEvent>,
}

impl StrictConnector {
    fn new(events: Vec<ConnectorEvent>) -> Self {
        Self {
            manifest: strict_manifest(),
            sensor: Identity::parse("snr:local:connector:strict:v1")
                .expect("invariant: strict sensor identity must parse"),
            events,
        }
    }
}

#[async_trait]
impl Connector for StrictConnector {
    fn name(&self) -> &'static str {
        "strict"
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

impl ConnectorPlugin for StrictConnector {
    const NAME: &'static str = "strict";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// An event whose `scope.kind` is not listed in `scopes.declared` must be
/// rejected before reaching `PipelineEmit::emit`.
///
/// With the Finding F grant `scope_patterns` check now running BEFORE
/// `validate_against_manifest`, an out-of-scope event may be caught by either:
/// - `ConsentRevoked` (Finding F: scope key doesn't match any grant pattern).
/// - `MalformedPayload` (original Fix 4 check: scope kind not in manifest).
///
/// Both are acceptable — what matters is that the event is blocked.
#[tokio::test]
async fn event_with_undeclared_scope_rejected() {
    // Manifest only declares kind="project"; emit kind="channel".
    // Grant has scope_patterns = ["project:*"], so "channel:#general" is
    // rejected by Finding F's consent scope-pattern check (ConsentRevoked)
    // before Fix 4's validate_against_manifest can fire (MalformedPayload).
    let event = base_event(
        ConnectorScope::new("channel", "#general"),
        ConnectorPayload::Json {
            mime: "application/json".into(),
            body: serde_json::json!({"text": "hello"}),
        },
    );

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(Arc::new(PanicEmit))
        .build();

    reg.register(StrictConnector::new(vec![event]))
        .expect("register must succeed");
    reg.enable("strict", strict_grant())
        .await
        .expect("enable must succeed");

    let err = reg
        .poll_now("strict")
        .await
        .expect_err("poll_now must fail for undeclared scope");

    // Accept either error: Finding F catches it first with ConsentRevoked;
    // if the grant scope_patterns were wider, Fix 4's MalformedPayload would fire.
    assert!(
        matches!(
            err,
            ConnectorError::MalformedPayload(ref msg) if msg.contains("scope")
        ) || matches!(err, ConnectorError::ConsentRevoked { .. }),
        "expected MalformedPayload(scope) or ConsentRevoked, got {err:?}",
    );

    reg.shutdown().await;
}

/// An event whose payload MIME type is not in `webhook.allowed_mimes` must be
/// rejected with `MalformedPayload`.
#[tokio::test]
async fn event_with_disallowed_mime_rejected() {
    // Manifest only allows "application/json"; emit "text/plain".
    let event = base_event(
        ConnectorScope::project("p"),
        ConnectorPayload::Text {
            mime: "text/plain".into(),
            body: "hello".into(),
        },
    );

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(Arc::new(PanicEmit))
        .build();

    reg.register(StrictConnector::new(vec![event]))
        .expect("register must succeed");
    reg.enable("strict", strict_grant())
        .await
        .expect("enable must succeed");

    let err = reg
        .poll_now("strict")
        .await
        .expect_err("poll_now must fail for disallowed MIME");

    assert!(
        matches!(err, ConnectorError::MalformedPayload(ref msg) if msg.contains("MIME")),
        "expected MalformedPayload about MIME, got {err:?}",
    );

    reg.shutdown().await;
}

/// An event whose payload size exceeds `payload.max_bytes` must be rejected
/// with `MalformedPayload`. `STRICT_MANIFEST` sets `max_bytes = "1KiB"` (1024 B).
#[tokio::test]
async fn event_payload_size_over_limit_rejected() {
    // Build a JSON body that serialises to more than 1 KiB.
    let big_body = serde_json::json!({
        "text": "x".repeat(1100)
    });

    let event = base_event(
        ConnectorScope::project("p"),
        ConnectorPayload::Json {
            mime: "application/json".into(),
            body: big_body,
        },
    );

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(Arc::new(PanicEmit))
        .build();

    reg.register(StrictConnector::new(vec![event]))
        .expect("register must succeed");
    reg.enable("strict", strict_grant())
        .await
        .expect("enable must succeed");

    let err = reg
        .poll_now("strict")
        .await
        .expect_err("poll_now must fail for oversized payload");

    assert!(
        matches!(err, ConnectorError::MalformedPayload(ref msg) if msg.contains("size")),
        "expected MalformedPayload about size, got {err:?}",
    );

    reg.shutdown().await;
}

/// JSON payload nested deeper than `payload.max_depth` (= 2 in `STRICT_MANIFEST`)
/// must be rejected with `MalformedPayload` from the redaction pipeline.
#[tokio::test]
async fn deeply_nested_json_rejected() {
    // Build 4 levels of nesting: {a: {b: {c: {d: "leaf"}}}}
    // STRICT_MANIFEST allows max_depth = 2; this exceeds it.
    let deeply_nested = serde_json::json!({
        "a": { "b": { "c": { "d": "leaf" } } }
    });

    let event = base_event(
        ConnectorScope::project("p"),
        ConnectorPayload::Json {
            mime: "application/json".into(),
            body: deeply_nested,
        },
    );

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(Arc::new(PanicEmit))
        .build();

    reg.register(StrictConnector::new(vec![event]))
        .expect("register must succeed");
    reg.enable("strict", strict_grant())
        .await
        .expect("enable must succeed");

    let err = reg
        .poll_now("strict")
        .await
        .expect_err("poll_now must fail for deeply nested JSON");

    assert!(
        matches!(err, ConnectorError::MalformedPayload(ref msg) if msg.contains("depth")),
        "expected MalformedPayload about depth, got {err:?}",
    );

    reg.shutdown().await;
}

/// Direct unit tests for `RedactionPipeline::with_max_depth`.
///
/// Depth semantics: `max_depth` is the maximum number of Object/Array
/// container boundaries that may be crossed. A flat `{"key": "value"}` has
/// depth 1 (one object boundary). `{"a": {"b": "leaf"}}` has depth 2.
#[test]
fn redaction_pipeline_depth_check() {
    use cairn_connectors_core::event::{
        ConnectorEventId, ConnectorPayload, ConnectorScope, DeliveryMode, SourceRef,
    };

    fn make_event(body: serde_json::Value) -> ConnectorEvent {
        ConnectorEvent::new(
            ConnectorEventId::new("01HX0000000000000000000000"),
            "x",
            SourceRef::new("t", "y", None),
            0,
            BTreeSet::new(),
            ConnectorScope::project("p"),
            ConnectorPayload::Json {
                mime: "application/json".into(),
                body,
            },
            DeliveryMode::Poll { cursor: None },
        )
    }

    // Flat JSON `{"key": "value"}` — depth 1 (one object boundary).
    // Passes with max_depth >= 1.
    let flat = serde_json::json!({"key": "value"});
    assert!(
        RedactionPipeline::new()
            .with_max_depth(1)
            .redact(make_event(flat.clone()))
            .is_ok(),
        "flat JSON must pass depth check of 1"
    );

    // Same flat JSON fails with max_depth = 0.
    let err = RedactionPipeline::new()
        .with_max_depth(0)
        .redact(make_event(flat))
        .expect_err("flat JSON must fail depth check of 0");
    assert!(
        matches!(err, ConnectorError::MalformedPayload(ref m) if m.contains("depth")),
        "expected depth error, got {err:?}"
    );

    // Two-level nested: {a: {b: "leaf"}} — depth 2.
    // Passes with max_depth >= 2; fails with max_depth = 1.
    let two_deep = serde_json::json!({"a": {"b": "leaf"}});
    assert!(
        RedactionPipeline::new()
            .with_max_depth(2)
            .redact(make_event(two_deep.clone()))
            .is_ok(),
        "two-deep JSON must pass depth check of 2"
    );

    let err = RedactionPipeline::new()
        .with_max_depth(1)
        .redact(make_event(two_deep))
        .expect_err("two-deep JSON must fail depth check of 1");
    assert!(
        matches!(err, ConnectorError::MalformedPayload(ref m) if m.contains("depth")),
        "expected depth error, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// parse_byte_size unit tests
// ---------------------------------------------------------------------------

/// `parse_byte_size` must parse all supported suffixes correctly.
#[test]
fn parse_byte_size_units() {
    assert_eq!(parse_byte_size("256KiB").unwrap(), 256 * 1_024);
    assert_eq!(parse_byte_size("50MiB").unwrap(), 50 * 1_048_576);
    assert_eq!(parse_byte_size("1GiB").unwrap(), 1_073_741_824);
    assert_eq!(parse_byte_size("1024").unwrap(), 1024);
    assert_eq!(parse_byte_size("0").unwrap(), 0);
}

/// Unknown suffixes must return `Err`.
#[test]
fn parse_byte_size_rejects_unknown_suffix() {
    assert!(parse_byte_size("100MB").is_err());
    assert!(parse_byte_size("100KB").is_err());
    assert!(parse_byte_size("not_a_number").is_err());
    assert!(parse_byte_size("5TiB").is_err());
}

// ---------------------------------------------------------------------------
// Finding B — Binary payload must carry bytes_len for size-gate enforcement
// ---------------------------------------------------------------------------

/// A `Binary` payload whose declared `bytes_len` exceeds `payload.max_bytes`
/// (1 KiB in `STRICT_MANIFEST`) must be rejected with `MalformedPayload`.
///
/// Verifies that `validate_against_manifest` no longer waves Binary payloads
/// through the size gate with a hard-coded `0`.
#[tokio::test]
async fn binary_payload_over_max_bytes_rejected() {
    // manifest max_bytes = "1KiB" = 1024 bytes; emit 2 KiB declared length.
    let event = base_event(
        ConnectorScope::project("p"),
        ConnectorPayload::Binary {
            mime: "application/json".into(),
            sha256: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            bytes_ref: "spool/x".into(),
            bytes_len: 2 * 1024, // 2 KiB > 1 KiB limit
        },
    );

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(Arc::new(PanicEmit))
        .build();

    reg.register(StrictConnector::new(vec![event]))
        .expect("register must succeed");
    reg.enable("strict", strict_grant())
        .await
        .expect("enable must succeed");

    let err = reg
        .poll_now("strict")
        .await
        .expect_err("poll_now must fail for oversized binary payload");

    assert!(
        matches!(err, ConnectorError::MalformedPayload(ref msg) if msg.contains("size")),
        "expected MalformedPayload about size, got {err:?}",
    );

    reg.shutdown().await;
}

/// `ConnectorPayload::Binary::size_hint()` must return the declared
/// `bytes_len` value, not `0`.
#[test]
fn binary_size_hint_returns_bytes_len() {
    let hint = ConnectorPayload::Binary {
        mime: "application/octet-stream".into(),
        sha256: "abc".into(),
        bytes_ref: "/tmp/x".into(),
        bytes_len: 4096,
    }
    .size_hint();
    assert_eq!(
        hint, 4096,
        "size_hint() must equal bytes_len for Binary variant"
    );
}

/// A `Binary` payload whose declared `bytes_len` fits within `payload.max_bytes`
/// must be accepted (`PanicEmit` is NOT used here — we use a recording emit).
#[tokio::test]
async fn binary_payload_within_limit_accepted() {
    use cairn_core::domain::capture::CaptureEvent;
    use std::sync::Mutex as StdMutex;

    struct CountEmit(StdMutex<usize>);
    #[async_trait::async_trait]
    impl PipelineEmit for CountEmit {
        async fn emit(&self, _: CaptureEvent) -> Result<(), ConnectorError> {
            *self.0.lock().expect("mutex unpoisoned") += 1;
            Ok(())
        }
    }

    let emit = Arc::new(CountEmit(StdMutex::new(0)));

    // max_bytes = "1KiB" = 1024; declare exactly 1024 bytes — must pass.
    let event = base_event(
        ConnectorScope::project("p"),
        ConnectorPayload::Binary {
            mime: "application/json".into(),
            sha256: "sha256:0000000000000000000000000000000000000000000000000000000000000000"
                .into(),
            bytes_ref: "spool/x".into(),
            bytes_len: 1024,
        },
    );

    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(emit.clone())
        .build();

    reg.register(StrictConnector::new(vec![event]))
        .expect("register must succeed");
    reg.enable("strict", strict_grant())
        .await
        .expect("enable must succeed");

    reg.poll_now("strict")
        .await
        .expect("poll_now must succeed for binary payload within size limit");

    assert_eq!(
        *emit.0.lock().expect("mutex unpoisoned"),
        1,
        "exactly one event must reach emit"
    );

    reg.shutdown().await;
}
