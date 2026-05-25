//! Fix 3 — `WebhookRouter::mount` is registry-internal; `webhook_router()`
//! on the registry builds the only legal route.
//!
//! Tests:
//!
//! - `registry_webhook_route_returns_route_for_enabled_connector` — an
//!   enabled webhook-capable connector gets a route under `/webhooks/<name>`.
//!   Sending a POST returns 501 (P0 stub — full handler is wired in #131).
//! - `registry_webhook_route_absent_for_disabled_connector` — a registered
//!   but not-yet-enabled connector does NOT get a route; a POST to its path
//!   returns 404.
//! - `webhook_router_mount_is_not_pub` — compile-time assertion: the
//!   test file cannot call `WebhookRouter::mount` because it is `pub(crate)`.
//!   This is enforced by the visibility modifier; no runtime test needed.
//!
//! Issue #130, brief §9.1, Fix 3.

#![allow(missing_docs)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use tower::ServiceExt as _;

use cairn_connectors_core::ConnectorError;
use cairn_connectors_core::fixture::{AcceptAllConsent, FixtureConnector, default_grant};
use cairn_connectors_core::{ConnectorRegistry, InMemoryCredentialStore, PipelineEmit};
use cairn_core::domain::capture::CaptureEvent;

// ---------------------------------------------------------------------------
// Recording emit (tests need to receive events; not used here but required
// by the registry builder).
// ---------------------------------------------------------------------------

#[derive(Default)]
struct NoopEmit;

#[async_trait::async_trait]
impl PipelineEmit for NoopEmit {
    async fn emit(&self, _: CaptureEvent) -> Result<(), ConnectorError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// An enabled, webhook-capable connector gets a route under `/webhooks/<name>`.
/// The P0 stub returns 501 Not Implemented (full handler lands in #131).
#[tokio::test]
async fn registry_webhook_route_returns_route_for_enabled_connector() {
    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(Arc::new(NoopEmit))
        .build();

    reg.register(FixtureConnector::with_default_manifest())
        .expect("register must succeed");
    reg.enable("fixture", default_grant())
        .await
        .expect("enable must succeed");

    let router = reg.webhook_router();

    // POST to the fixture connector's webhook path.
    let req = Request::builder()
        .method(Method::POST)
        .uri("/webhooks/fixture")
        .header("content-type", "application/json")
        .body(Body::from(b"{}".as_slice()))
        .expect("request must build");

    let resp = router.oneshot(req).await.expect("router must respond");

    // P0 stub returns 501; this will become 200 when #131 wires the handler.
    assert_eq!(
        resp.status(),
        StatusCode::NOT_IMPLEMENTED,
        "P0 stub must return 501 for enabled webhook connector"
    );

    reg.shutdown().await;
}

/// A registered connector that has NOT been enabled must not get a webhook
/// route. Requests to its path must return 404.
#[tokio::test]
async fn registry_webhook_route_absent_for_disabled_connector() {
    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(Arc::new(NoopEmit))
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
