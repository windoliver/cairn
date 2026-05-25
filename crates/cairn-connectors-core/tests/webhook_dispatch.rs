//! Webhook dispatcher tests — `WebhookRouter::mount` is registry-internal;
//! `webhook_router()` on the registry builds the only legal route.
//!
//! Tests:
//!
//! - `webhook_post_emits_external_event` — build registry with fixture connector
//!   enabled. Pre-provision a known secret. Construct a POST request with valid
//!   HMAC. Route through registry router. Assert 204 and 1 emitted event.
//! - `webhook_post_rejects_bad_signature` — same setup, tampered signature → 401.
//! - `webhook_post_rejects_oversize_body` — body > `max_bytes_parsed` → 413.
//! - `webhook_post_rejects_missing_credential` — no credential provisioned → 401.
//! - `registry_webhook_route_absent_for_disabled_connector` — a registered
//!   but not-yet-enabled connector does NOT get a route; a POST to its path
//!   returns 404.
//! - `webhook_router_mount_is_not_pub` — compile-time assertion: the
//!   test file cannot call `WebhookRouter::mount` because it is `pub(crate)`.
//!   This is enforced by the visibility modifier; no runtime test needed.
//!
//! Issue #130, brief §9.1, Fix 3 (Finding G).

#![allow(missing_docs)]

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use tower::ServiceExt as _;

use cairn_connectors_core::ConnectorError;
use cairn_connectors_core::CredentialStore;
use cairn_connectors_core::fixture::{AcceptAllConsent, FixtureConnector, default_grant};
use cairn_connectors_core::webhook::hex_hmac_sha256;
use cairn_connectors_core::{ConnectorRegistry, InMemoryCredentialStore, PipelineEmit};
use cairn_core::domain::capture::CaptureEvent;

// ---------------------------------------------------------------------------
// Capturer emit — records every accepted CaptureEvent.
// ---------------------------------------------------------------------------

/// Records every emitted `CaptureEvent` for post-test assertions.
#[derive(Default)]
struct Capturer(Mutex<Vec<CaptureEvent>>);

#[async_trait::async_trait]
impl PipelineEmit for Capturer {
    async fn emit(&self, ev: CaptureEvent) -> Result<(), ConnectorError> {
        self.0
            .lock()
            .expect("invariant: Capturer mutex unpoisoned")
            .push(ev);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Shared setup helpers
// ---------------------------------------------------------------------------

/// Known webhook secret for all tests that provision one.
const WEBHOOK_SECRET: &[u8] = b"shh";

/// Signature header name declared in the fixture connector manifest.
const SIG_HEADER: &str = "X-Fixture-Signature";

/// Provision the webhook secret for the `fixture` connector in `store`.
async fn provision_secret(store: &InMemoryCredentialStore) {
    store
        .put("connector/fixture/webhook_secret", WEBHOOK_SECRET.to_vec())
        .await
        .expect("invariant: in-memory store put must succeed");
}

// ---------------------------------------------------------------------------
// Tests — Finding G (real webhook handler)
// ---------------------------------------------------------------------------

/// An enabled webhook-capable connector with a valid HMAC signature and a
/// provisioned secret must return `204 No Content` and emit exactly one event.
#[tokio::test]
async fn webhook_post_emits_external_event() {
    let capturer = Arc::new(Capturer::default());
    let creds = Arc::new(InMemoryCredentialStore::default());
    provision_secret(&creds).await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let mut reg = ConnectorRegistry::builder()
        .credentials(
            Arc::clone(&creds) as Arc<dyn cairn_connectors_core::credential::CredentialStore>
        )
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(capturer.clone() as Arc<dyn PipelineEmit>)
        .spool_root(tmp.path().to_path_buf())
        .build();

    reg.register(FixtureConnector::with_default_manifest())
        .expect("register must succeed");
    reg.enable("fixture", default_grant())
        .await
        .expect("enable must succeed");

    let router = reg.webhook_router();

    let body = serde_json::json!({"k": "v"}).to_string();
    let sig = hex_hmac_sha256(WEBHOOK_SECRET, body.as_bytes());

    let req = Request::builder()
        .method(Method::POST)
        .uri("/webhooks/fixture")
        .header("content-type", "application/json")
        .header(SIG_HEADER, &sig)
        .body(Body::from(body.clone()))
        .expect("request must build");

    let resp = router.oneshot(req).await.expect("router must respond");
    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "valid webhook request must return 204 No Content"
    );

    // The fixture connector's ingest_webhook always returns one sample event.
    // Drop the lock guard before the next `.await` so we don't hold a
    // `std::sync::MutexGuard` across an await point.
    {
        let events = capturer
            .0
            .lock()
            .expect("invariant: Capturer mutex unpoisoned");
        assert_eq!(
            events.len(),
            1,
            "exactly 1 event must be emitted for a valid webhook delivery"
        );
        assert_eq!(
            events[0].source_family,
            cairn_core::domain::capture::SourceFamily::External,
            "emitted event must have source_family == External"
        );
    } // lock released here

    reg.shutdown().await;
}

/// A POST request with a tampered HMAC signature must be rejected with 401.
/// The `Capturer` must remain empty.
#[tokio::test]
async fn webhook_post_rejects_bad_signature() {
    let capturer = Arc::new(Capturer::default());
    let creds = Arc::new(InMemoryCredentialStore::default());
    provision_secret(&creds).await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let mut reg = ConnectorRegistry::builder()
        .credentials(
            Arc::clone(&creds) as Arc<dyn cairn_connectors_core::credential::CredentialStore>
        )
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(capturer.clone() as Arc<dyn PipelineEmit>)
        .spool_root(tmp.path().to_path_buf())
        .build();

    reg.register(FixtureConnector::with_default_manifest())
        .expect("register must succeed");
    reg.enable("fixture", default_grant())
        .await
        .expect("enable must succeed");

    let router = reg.webhook_router();

    let body = serde_json::json!({"k": "v"}).to_string();
    // Tampered signature — compute HMAC over the WRONG secret.
    let bad_sig = hex_hmac_sha256(b"wrong-secret", body.as_bytes());

    let req = Request::builder()
        .method(Method::POST)
        .uri("/webhooks/fixture")
        .header("content-type", "application/json")
        .header(SIG_HEADER, &bad_sig)
        .body(Body::from(body))
        .expect("request must build");

    let resp = router.oneshot(req).await.expect("router must respond");
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "bad signature must return 401 Unauthorized"
    );

    {
        let events = capturer
            .0
            .lock()
            .expect("invariant: Capturer mutex unpoisoned");
        assert_eq!(
            events.len(),
            0,
            "no events must be emitted when the signature is rejected"
        );
    } // lock released before shutdown await

    reg.shutdown().await;
}

/// A POST request whose body exceeds `manifest.payload.max_bytes_parsed` must
/// be rejected with 413 Payload Too Large.
#[tokio::test]
async fn webhook_post_rejects_oversize_body() {
    let creds = Arc::new(InMemoryCredentialStore::default());
    provision_secret(&creds).await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let mut reg = ConnectorRegistry::builder()
        .credentials(
            Arc::clone(&creds) as Arc<dyn cairn_connectors_core::credential::CredentialStore>
        )
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(Arc::new(Capturer::default()) as Arc<dyn PipelineEmit>)
        .spool_root(tmp.path().to_path_buf())
        .build();

    reg.register(FixtureConnector::with_default_manifest())
        .expect("register must succeed");
    reg.enable("fixture", default_grant())
        .await
        .expect("enable must succeed");

    // max_bytes_parsed for the fixture connector is 256 KiB = 262144 bytes.
    // Send a body that is 1 byte over the limit.
    let oversize_body = vec![b'x'; 262_145];
    // The limit check happens before signature verification, so we don't need
    // a valid HMAC here.
    let req = Request::builder()
        .method(Method::POST)
        .uri("/webhooks/fixture")
        .header("content-type", "application/octet-stream")
        .body(Body::from(oversize_body))
        .expect("request must build");

    let router = reg.webhook_router();
    let resp = router.oneshot(req).await.expect("router must respond");
    assert_eq!(
        resp.status(),
        StatusCode::PAYLOAD_TOO_LARGE,
        "body exceeding max_bytes_parsed must return 413 Payload Too Large"
    );

    reg.shutdown().await;
}

/// A POST request where no webhook secret has been provisioned for the
/// connector must be rejected with 401 Unauthorized.
#[tokio::test]
async fn webhook_post_rejects_missing_credential() {
    let capturer = Arc::new(Capturer::default());
    // Intentionally do NOT provision any credential.
    let creds = Arc::new(InMemoryCredentialStore::default());

    let tmp = tempfile::tempdir().expect("tempdir");
    let mut reg = ConnectorRegistry::builder()
        .credentials(
            Arc::clone(&creds) as Arc<dyn cairn_connectors_core::credential::CredentialStore>
        )
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(capturer.clone() as Arc<dyn PipelineEmit>)
        .spool_root(tmp.path().to_path_buf())
        .build();

    reg.register(FixtureConnector::with_default_manifest())
        .expect("register must succeed");
    reg.enable("fixture", default_grant())
        .await
        .expect("enable must succeed");

    let router = reg.webhook_router();

    let body = b"{}";
    let sig = hex_hmac_sha256(WEBHOOK_SECRET, body);

    let req = Request::builder()
        .method(Method::POST)
        .uri("/webhooks/fixture")
        .header("content-type", "application/json")
        .header(SIG_HEADER, &sig)
        .body(Body::from(body.as_slice()))
        .expect("request must build");

    let resp = router.oneshot(req).await.expect("router must respond");
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "missing webhook secret must return 401 Unauthorized"
    );

    {
        let events = capturer
            .0
            .lock()
            .expect("invariant: Capturer mutex unpoisoned");
        assert_eq!(
            events.len(),
            0,
            "no events must be emitted when the credential is missing"
        );
    } // lock released before shutdown await

    reg.shutdown().await;
}

/// Sending the exact same signed webhook request twice must return 204 on the
/// first delivery and 409 Conflict on the second (replay guard, Finding I).
///
/// The in-memory replay set uses `(connector_name, signature_id)` as the key.
/// Since the signature is HMAC-SHA256 of a fixed body+secret, the same request
/// will always produce the same `signature_id` and therefore be recognised as a
/// replay.
#[tokio::test]
async fn replay_returns_conflict() {
    let capturer = Arc::new(Capturer::default());
    let creds = Arc::new(InMemoryCredentialStore::default());
    provision_secret(&creds).await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let mut reg = ConnectorRegistry::builder()
        .credentials(
            Arc::clone(&creds) as Arc<dyn cairn_connectors_core::credential::CredentialStore>
        )
        .consent(Arc::new(
            cairn_connectors_core::fixture::AcceptAllConsent::default(),
        ))
        .emit(capturer.clone() as Arc<dyn PipelineEmit>)
        .spool_root(tmp.path().to_path_buf())
        .build();

    reg.register(cairn_connectors_core::fixture::FixtureConnector::with_default_manifest())
        .expect("register must succeed");
    reg.enable("fixture", cairn_connectors_core::fixture::default_grant())
        .await
        .expect("enable must succeed");

    let router = reg.webhook_router();

    let body = serde_json::json!({"replay": "test"}).to_string();
    let sig = hex_hmac_sha256(WEBHOOK_SECRET, body.as_bytes());

    // First delivery — must succeed with 204.
    let req1 = Request::builder()
        .method(Method::POST)
        .uri("/webhooks/fixture")
        .header("content-type", "application/json")
        .header(SIG_HEADER, &sig)
        .body(Body::from(body.clone()))
        .expect("request 1 must build");

    let resp1 = router
        .clone()
        .oneshot(req1)
        .await
        .expect("router must respond to first request");
    assert_eq!(
        resp1.status(),
        StatusCode::NO_CONTENT,
        "first delivery of a valid webhook must return 204",
    );

    // Second delivery with the EXACT same signature — must return 409 Conflict.
    let req2 = Request::builder()
        .method(Method::POST)
        .uri("/webhooks/fixture")
        .header("content-type", "application/json")
        .header(SIG_HEADER, &sig)
        .body(Body::from(body.clone()))
        .expect("request 2 must build");

    let resp2 = router
        .clone()
        .oneshot(req2)
        .await
        .expect("router must respond to second request");
    assert_eq!(
        resp2.status(),
        StatusCode::CONFLICT,
        "replayed webhook (same signature) must return 409 Conflict",
    );

    // Only one event must have been emitted (the first delivery).
    {
        let events = capturer
            .0
            .lock()
            .expect("invariant: Capturer mutex unpoisoned");
        assert_eq!(
            events.len(),
            1,
            "exactly 1 event must be emitted; the replay must not produce a second event",
        );
    }

    reg.shutdown().await;
}

/// A registered connector that has NOT been enabled must not get a webhook
/// route. Requests to its path must return 404.
#[tokio::test]
async fn registry_webhook_route_absent_for_disabled_connector() {
    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(Arc::new(Capturer::default()) as Arc<dyn PipelineEmit>)
        .build();

    // Register but do NOT enable.
    reg.register(FixtureConnector::with_default_manifest())
        .expect("register must succeed");

    let router = reg.webhook_router();

    let req = Request::builder()
        .method(Method::POST)
        .uri("/webhooks/fixture")
        .body(Body::from(b"{}".as_slice()))
        .expect("request must build");

    let resp = router.oneshot(req).await.expect("router must respond");

    // No route registered → axum returns 404.
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "disabled connector must not have a webhook route (expected 404)"
    );

    reg.shutdown().await;
}
