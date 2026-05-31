//! End-to-end: signed clip -> real ConnectorRegistry webhook router -> PipelineEmit.
//!
//! Proves the adapter's event passes the substrate's full gate (HMAC verify ->
//! consent -> label -> scope -> redaction -> spool -> emit) with the real
//! per-domain scope, and that a registered-but-not-enabled connector has no route.

#![allow(missing_docs)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use tower::ServiceExt as _;

use cairn_connectors_core::fixture::AcceptAllConsent;
use cairn_connectors_core::manifest::ConnectorManifest;
use cairn_connectors_core::webhook::hex_hmac_sha256;
use cairn_connectors_core::{
    ConnectorError, ConnectorRegistry, CredentialStore, InMemoryCredentialStore, PipelineEmit,
};
use cairn_connectors_webclip::{MANIFEST_TOML, WebClipConnector};
use cairn_core::contract::connector_consent::ConsentGrant;
use cairn_core::domain::Identity;
use cairn_core::domain::capture::CaptureEvent;

const SECRET: &[u8] = b"clip-secret";

#[derive(Default)]
struct Capturer(Mutex<Vec<CaptureEvent>>);

#[async_trait::async_trait]
impl PipelineEmit for Capturer {
    async fn emit(&self, ev: CaptureEvent) -> Result<(), ConnectorError> {
        self.0.lock().expect("mutex unpoisoned").push(ev);
        Ok(())
    }
}

/// Build a grant whose manifest_hash matches the compiled-in connector.toml.
fn webclip_grant() -> ConsentGrant {
    let manifest_hash = ConnectorManifest::parse_toml(MANIFEST_TOML)
        .expect("webclip manifest valid")
        .hash();
    ConsentGrant::new(
        "webclip",
        manifest_hash,
        BTreeSet::from(["source:web".to_string(), "kind:clip".to_string()]),
        vec!["domain:*".to_string()],
        1_700_000_000,
        Identity::parse("hmn:alice").expect("valid identity"),
    )
}

#[tokio::test]
async fn signed_clip_reaches_pipeline_emit() {
    let capturer = Arc::new(Capturer::default());
    let creds = Arc::new(InMemoryCredentialStore::default());
    creds
        .put("connector/webclip/webhook_secret", SECRET.to_vec())
        .await
        .expect("secret put succeeds");

    let tmp = tempfile::tempdir().expect("tempdir");
    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::clone(&creds) as Arc<dyn CredentialStore>)
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(capturer.clone() as Arc<dyn PipelineEmit>)
        .spool_root(tmp.path().to_path_buf())
        .build();

    reg.register(WebClipConnector::new().expect("construct"))
        .expect("register succeeds");
    reg.enable("webclip", webclip_grant())
        .await
        .expect("enable succeeds");

    let router = reg.webhook_router();

    let body = include_str!("fixtures/clip_json.json");
    let sig = format!("sha256={}", hex_hmac_sha256(SECRET, body.as_bytes()));
    let req = Request::builder()
        .method(Method::POST)
        .uri("/webhooks/webclip")
        .header("content-type", "application/json")
        .header("X-Cairn-Signature-256", &sig)
        .body(Body::from(body))
        .expect("request builds");

    let resp = router.oneshot(req).await.expect("router responds");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT, "valid clip -> 204");

    {
        let events = capturer.0.lock().expect("mutex unpoisoned");
        assert_eq!(events.len(), 1, "exactly one CaptureEvent emitted");
        assert_eq!(
            events[0].source_family,
            cairn_core::domain::capture::SourceFamily::External,
            "clip event must be source_family External",
        );
    }

    reg.shutdown().await;
}

#[tokio::test]
async fn not_enabled_connector_has_no_route() {
    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(Arc::new(Capturer::default()) as Arc<dyn PipelineEmit>)
        .build();

    // Register but do NOT enable.
    reg.register(WebClipConnector::new().unwrap())
        .expect("register succeeds");

    let router = reg.webhook_router();
    let req = Request::builder()
        .method(Method::POST)
        .uri("/webhooks/webclip")
        .body(Body::from(b"{}".as_slice()))
        .expect("request builds");

    let resp = router.oneshot(req).await.expect("router responds");
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "disabled connector must not have a webhook route (fail-closed)"
    );

    reg.shutdown().await;
}
