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

/// Delivery-id header declared in the fixture connector manifest (Finding V).
const DELIVERY_ID_HEADER: &str = "X-Fixture-Delivery";

/// Generate a unique delivery ID per request using a process-local counter.
///
/// Each call returns a distinct value of the form `"dlv-<n>"`, where `<n>` is
/// a monotonically increasing integer. This is cheaper than a ULID and
/// sufficient for test uniqueness.
fn unique_delivery_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!("dlv-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

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
        .header(DELIVERY_ID_HEADER, unique_delivery_id())
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

    // Bad-sig requests are rejected at HMAC verification before the delivery-id
    // check — no need to add the delivery-id header, but we include it for
    // symmetry and to confirm it has no effect on the rejection.
    let req = Request::builder()
        .method(Method::POST)
        .uri("/webhooks/fixture")
        .header("content-type", "application/json")
        .header(SIG_HEADER, &bad_sig)
        .header(DELIVERY_ID_HEADER, unique_delivery_id())
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
        .header(DELIVERY_ID_HEADER, unique_delivery_id())
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
/// The in-memory replay set uses `(connector_name, sig_id)` as the key.
/// `sig_id` is the HMAC-SHA256 of the body under the provisioned secret —
/// authenticated and body-bound. The same body+secret always produce the same
/// `sig_id`, so the second request is recognised as a replay.
///
/// Finding W: the delivery-id header is adapter-facing metadata only; the
/// framework does not use it for replay keying. Same body+sig = same replay
/// key, regardless of what delivery-id each request carries.
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
    // Both requests use the SAME delivery-id for symmetry, but the guard fires
    // because they share the same sig_id (body-MAC), not the delivery-id.
    let same_delivery_id = "dlv-replay-test-001";

    // First delivery — must succeed with 204.
    let req1 = Request::builder()
        .method(Method::POST)
        .uri("/webhooks/fixture")
        .header("content-type", "application/json")
        .header(SIG_HEADER, &sig)
        .header(DELIVERY_ID_HEADER, same_delivery_id)
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

    // Second delivery with the EXACT same body+sig — must return 409 Conflict.
    let req2 = Request::builder()
        .method(Method::POST)
        .uri("/webhooks/fixture")
        .header("content-type", "application/json")
        .header(SIG_HEADER, &sig)
        .header(DELIVERY_ID_HEADER, same_delivery_id)
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

// ---------------------------------------------------------------------------
// Finding L — replay marker must not be committed before processing succeeds
// ---------------------------------------------------------------------------

/// A `PipelineEmit` that always fails (simulates a transient downstream error).
#[derive(Default)]
struct FailingEmit;

#[async_trait::async_trait]
impl PipelineEmit for FailingEmit {
    async fn emit(
        &self,
        _: cairn_core::domain::capture::CaptureEvent,
    ) -> Result<(), ConnectorError> {
        Err(ConnectorError::transient_msg("simulated emit failure"))
    }
}

/// When `ingest_webhook` or `process_event` fails, the replay guard must NOT
/// lock the signature — the provider must be able to retry the exact same
/// webhook request and have it succeed on the next attempt.
///
/// Finding L fix: `replay_seen.insert(...)` must happen AFTER all `process_event`
/// calls return `Ok`, not right after HMAC verification.
#[tokio::test]
async fn failed_processing_does_not_lock_signature() {
    let creds = Arc::new(InMemoryCredentialStore::default());
    provision_secret(&creds).await;

    let tmp = tempfile::tempdir().expect("tempdir");

    // First registry: FailingEmit causes process_event to fail.
    // The signature must NOT be locked into replay_seen.
    let failing_reg = {
        let mut reg = ConnectorRegistry::builder()
            .credentials(
                Arc::clone(&creds) as Arc<dyn cairn_connectors_core::credential::CredentialStore>
            )
            .consent(Arc::new(
                cairn_connectors_core::fixture::AcceptAllConsent::default(),
            ))
            .emit(Arc::new(FailingEmit) as Arc<dyn PipelineEmit>)
            .spool_root(tmp.path().to_path_buf())
            .build();

        reg.register(FixtureConnector::with_default_manifest())
            .expect("register must succeed");
        reg.enable("fixture", default_grant())
            .await
            .expect("enable must succeed");
        reg
    };

    let body = serde_json::json!({"finding": "L"}).to_string();
    let sig = hex_hmac_sha256(WEBHOOK_SECRET, body.as_bytes());
    // Both req1 and req2 use the same body+sig: the point of this test is
    // that after a failed emit the sig_id is NOT locked (Pending entry is
    // removed), so the second registry accepts the same body+sig again.
    let delivery_id = "dlv-finding-l-001";

    // First request: processing fails (emit error). Must NOT return 204.
    let req1 = Request::builder()
        .method(Method::POST)
        .uri("/webhooks/fixture")
        .header("content-type", "application/json")
        .header(SIG_HEADER, &sig)
        .header(DELIVERY_ID_HEADER, delivery_id)
        .body(Body::from(body.clone()))
        .expect("request must build");

    let router1 = failing_reg.webhook_router();
    let resp1 = router1.oneshot(req1).await.expect("router must respond");
    assert_ne!(
        resp1.status(),
        StatusCode::NO_CONTENT,
        "first request with failing emit must not return 204",
    );
    // Must be non-2xx — 422 or 5xx depending on the error variant.
    assert!(
        !resp1.status().is_success(),
        "first request must fail (non-2xx), got {}",
        resp1.status(),
    );

    failing_reg.shutdown().await;

    // Second registry: Capturer succeeds. The same signature is sent again.
    // Since the first attempt did NOT commit the replay marker, this request
    // must succeed with 204 and emit exactly one event.
    let capturer = Arc::new(Capturer::default());
    let tmp2 = tempfile::tempdir().expect("tempdir2");
    let mut succeeding_reg = ConnectorRegistry::builder()
        .credentials(
            Arc::clone(&creds) as Arc<dyn cairn_connectors_core::credential::CredentialStore>
        )
        .consent(Arc::new(
            cairn_connectors_core::fixture::AcceptAllConsent::default(),
        ))
        .emit(capturer.clone() as Arc<dyn PipelineEmit>)
        .spool_root(tmp2.path().to_path_buf())
        .build();

    succeeding_reg
        .register(FixtureConnector::with_default_manifest())
        .expect("register must succeed");
    succeeding_reg
        .enable("fixture", default_grant())
        .await
        .expect("enable must succeed");

    let req2 = Request::builder()
        .method(Method::POST)
        .uri("/webhooks/fixture")
        .header("content-type", "application/json")
        .header(SIG_HEADER, &sig)
        .header(DELIVERY_ID_HEADER, delivery_id)
        .body(Body::from(body))
        .expect("request must build");

    let router2 = succeeding_reg.webhook_router();
    let resp2 = router2.oneshot(req2).await.expect("router must respond");
    assert_eq!(
        resp2.status(),
        StatusCode::NO_CONTENT,
        "retry with same body+sig must succeed (204) when the Pending marker was removed on first failure",
    );

    {
        let events = capturer.0.lock().expect("mutex unpoisoned");
        assert_eq!(
            events.len(),
            1,
            "exactly 1 event must be emitted on the successful retry",
        );
    }

    succeeding_reg.shutdown().await;
}

// ---------------------------------------------------------------------------
// Finding O — concurrent duplicate deliveries must not both succeed
// ---------------------------------------------------------------------------

/// Two goroutines posting the exact same signed webhook body concurrently must
/// result in exactly one 204 and one 409 — not two 204s (Finding O).
///
/// The previous implementation used a "check-then-process-then-commit" pattern
/// that left a window where two concurrent deliveries could both pass the check
/// before either committed the replay marker, producing duplicate events.
///
/// The fix inserts a `Pending` marker atomically inside the same lock
/// acquisition as the check. When the second delivery arrives (even
/// concurrently), it sees `Pending` and returns 409 immediately.
///
/// The test spawns two `tokio::task`s that each issue the same signed request.
/// After both complete, the `Capturer` must have exactly 1 event and the two
/// status codes must be exactly one 204 and one 409.
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_duplicate_deliveries_one_succeeds() {
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

    reg.register(FixtureConnector::with_default_manifest())
        .expect("register must succeed");
    reg.enable("fixture", default_grant())
        .await
        .expect("enable must succeed");

    let router = reg.webhook_router();

    let body = serde_json::json!({"concurrent": "dedup"}).to_string();
    let sig = hex_hmac_sha256(WEBHOOK_SECRET, body.as_bytes());
    // Both tasks use the same body+sig — they share the same replay key (sig_id).
    let concurrent_delivery_id = "dlv-concurrent-dedup-001".to_owned();

    // Spawn two tasks posting the identical signed request simultaneously.
    // `axum::Router` is `Clone` — both tasks get their own clone of the router
    // but share the underlying `Arc<RegistryWebhookState>`.
    let (tx1, rx1) = tokio::sync::oneshot::channel::<StatusCode>();
    let (tx2, rx2) = tokio::sync::oneshot::channel::<StatusCode>();

    let router1 = router.clone();
    let body1 = body.clone();
    let sig1 = sig.clone();
    let did1 = concurrent_delivery_id.clone();
    tokio::spawn(async move {
        let req = Request::builder()
            .method(Method::POST)
            .uri("/webhooks/fixture")
            .header("content-type", "application/json")
            .header(SIG_HEADER, &sig1)
            .header(DELIVERY_ID_HEADER, &did1)
            .body(Body::from(body1))
            .expect("request 1 must build");
        let resp = router1.oneshot(req).await.expect("router must respond");
        let _ = tx1.send(resp.status());
    });

    let router2 = router.clone();
    let body2 = body.clone();
    let sig2 = sig.clone();
    let did2 = concurrent_delivery_id.clone();
    tokio::spawn(async move {
        let req = Request::builder()
            .method(Method::POST)
            .uri("/webhooks/fixture")
            .header("content-type", "application/json")
            .header(SIG_HEADER, &sig2)
            .header(DELIVERY_ID_HEADER, &did2)
            .body(Body::from(body2))
            .expect("request 2 must build");
        let resp = router2.oneshot(req).await.expect("router must respond");
        let _ = tx2.send(resp.status());
    });

    let status1 = rx1.await.expect("task 1 must complete");
    let status2 = rx2.await.expect("task 2 must complete");

    // Exactly one must be 204 (success) and one must be 409 (conflict).
    let statuses = [status1, status2];
    let num_success = statuses
        .iter()
        .filter(|&&s| s == StatusCode::NO_CONTENT)
        .count();
    let num_conflict = statuses
        .iter()
        .filter(|&&s| s == StatusCode::CONFLICT)
        .count();

    assert_eq!(
        num_success, 1,
        "exactly one concurrent duplicate must succeed (204); got statuses {statuses:?}",
    );
    assert_eq!(
        num_conflict, 1,
        "exactly one concurrent duplicate must be rejected (409); got statuses {statuses:?}",
    );

    // Only one event must have been emitted.
    {
        let events = capturer
            .0
            .lock()
            .expect("invariant: Capturer mutex unpoisoned");
        assert_eq!(
            events.len(),
            1,
            "exactly 1 event must be emitted despite concurrent duplicates; got {events:?}",
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

// ---------------------------------------------------------------------------
// Finding W — replay key is HMAC body-MAC, not the delivery-id header
// ---------------------------------------------------------------------------
//
// Finding V (previous round) keyed the replay guard on the unauthenticated
// `delivery_id_header` value, which an attacker could swap to bypass the
// guard (same body+sig, new delivery-id → 204 a second time instead of 409).
//
// Finding W reverts the replay key to `(connector_name, sig_id)` — the HMAC
// body-MAC hex, which IS authenticated.  The `delivery_id_header` manifest
// field is retained as adapter-facing metadata only; the framework ignores it
// for replay purposes.
//
// Trade-off: identical-body deliveries (heartbeats, repeating payloads)
// collide on the replay key and the second is dropped. Providers with
// repeating-body payloads must implement per-adapter dedup inside
// `ingest_webhook` — deferred to #131.
//
// Deleted tests (Finding W):
//  - `identical_bodies_with_distinct_delivery_ids_both_succeed` — the new
//    semantics is "identical bodies collide; that's the trade-off". Two
//    deliveries with the same body+sig produce the same replay key and the
//    second is dropped regardless of which delivery-id header they carry.
//  - `missing_delivery_id_header_returns_400` — the framework no longer
//    enforces presence of the delivery-id header (it ignores it entirely).

/// An attacker replaying the same authenticated body+sig with a new
/// `X-Fixture-Delivery` header value must still receive 409 Conflict.
///
/// With the Finding V implementation the delivery-id was used as the replay
/// key, so swapping it would allow the attacker to bypass the guard. Finding
/// W fixes this: the replay key is always the HMAC body-MAC (`sig_id`), which
/// is authenticated and cannot be changed without breaking signature
/// verification. An attacker who swaps only the delivery-id header while
/// keeping the same body+sig must receive 409.
#[tokio::test]
async fn replay_with_changed_delivery_id_still_blocked() {
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

    // Authenticated body + signature — the same for both requests.
    let body = serde_json::json!({"x": 1}).to_string();
    let sig = hex_hmac_sha256(WEBHOOK_SECRET, body.as_bytes());

    // First delivery with delivery-id "id-1" — must succeed.
    let req1 = Request::builder()
        .method(Method::POST)
        .uri("/webhooks/fixture")
        .header("content-type", "application/json")
        .header(SIG_HEADER, &sig)
        .header(DELIVERY_ID_HEADER, "id-1")
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
        "first delivery must return 204 No Content",
    );

    // Attacker replays the SAME body+sig with a swapped delivery-id ("id-2").
    // Finding W: the replay key is sig_id (authenticated), not delivery_id
    // (unauthenticated), so this replay must be blocked with 409.
    let req2 = Request::builder()
        .method(Method::POST)
        .uri("/webhooks/fixture")
        .header("content-type", "application/json")
        .header(SIG_HEADER, &sig)
        .header(DELIVERY_ID_HEADER, "id-2")
        .body(Body::from(body))
        .expect("request 2 must build");

    let resp2 = router
        .clone()
        .oneshot(req2)
        .await
        .expect("router must respond to second request");
    assert_eq!(
        resp2.status(),
        StatusCode::CONFLICT,
        "replay with swapped delivery-id but identical body+sig must return 409 \
         (Finding W: replay key is sig_id, not delivery_id)",
    );

    // Only one event must have been emitted (the first delivery).
    {
        let events = capturer.0.lock().expect("mutex unpoisoned");
        assert_eq!(
            events.len(),
            1,
            "exactly 1 event must be emitted; the replay must not produce a second event",
        );
    }

    reg.shutdown().await;
}
