//! Re-delivery idempotency + content-sensitivity of the event id.

use std::sync::Arc;

use cairn_connectors_core::{Connector, CredentialHandle, WebhookContext};
use cairn_connectors_webclip::{WebClipConnector, testkit};

fn ctx() -> WebhookContext {
    WebhookContext::new(Arc::new(CredentialHandle::empty()), 1000)
}

#[tokio::test]
async fn same_clip_twice_yields_same_event_id() {
    let c = WebClipConnector::new().unwrap();
    let body = include_str!("fixtures/clip_json.json");
    let e1 = c
        .ingest_webhook(&testkit::json_clip_request(b"s", body), &ctx())
        .await
        .unwrap();
    let e2 = c
        .ingest_webhook(&testkit::json_clip_request(b"s", body), &ctx())
        .await
        .unwrap();
    assert_eq!(e1[0].event_id.as_str(), e2[0].event_id.as_str());
}

#[tokio::test]
async fn same_url_same_second_different_body_differs() {
    let c = WebClipConnector::new().unwrap();
    let b1 = r#"{"url":"https://e.com/a","captured_at":100,"markdown":"one"}"#;
    let b2 = r#"{"url":"https://e.com/a","captured_at":100,"markdown":"two"}"#;
    let e1 = c
        .ingest_webhook(&testkit::json_clip_request(b"s", b1), &ctx())
        .await
        .unwrap();
    let e2 = c
        .ingest_webhook(&testkit::json_clip_request(b"s", b2), &ctx())
        .await
        .unwrap();
    assert_ne!(e1[0].event_id.as_str(), e2[0].event_id.as_str());
}
