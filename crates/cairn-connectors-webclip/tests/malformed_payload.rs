//! Adapter-side rejections all surface as `ConnectorError::MalformedPayload`.

use std::sync::Arc;

use cairn_connectors_core::{
    Connector, ConnectorError, CredentialHandle, WebhookContext, WebhookRequest,
};
use cairn_connectors_webclip::WebClipConnector;

fn ctx() -> WebhookContext {
    WebhookContext::new(Arc::new(CredentialHandle::empty()), 1000)
}

async fn assert_malformed(req: WebhookRequest) {
    let connector = WebClipConnector::new().unwrap();
    let result = connector.ingest_webhook(&req, &ctx()).await;
    assert!(
        matches!(result, Err(ConnectorError::MalformedPayload(_))),
        "expected MalformedPayload, got {result:?}"
    );
}

fn json_req(body: &[u8]) -> WebhookRequest {
    WebhookRequest {
        connector: "webclip".into(),
        body: body.to_vec(),
        headers: vec![("Content-Type".into(), "application/json".into())],
    }
}

#[tokio::test]
async fn json_missing_url_is_malformed() {
    assert_malformed(json_req(br#"{"captured_at":1,"markdown":"x"}"#)).await;
}

#[tokio::test]
async fn json_missing_captured_at_is_malformed() {
    assert_malformed(json_req(br#"{"url":"https://e.com/a","markdown":"x"}"#)).await;
}

#[tokio::test]
async fn json_no_body_field_is_malformed() {
    assert_malformed(json_req(br#"{"url":"https://e.com/a","captured_at":1}"#)).await;
}

#[tokio::test]
async fn hostless_url_is_malformed() {
    assert_malformed(json_req(
        br#"{"url":"file:///x","captured_at":1,"markdown":"x"}"#,
    ))
    .await;
}

#[tokio::test]
async fn missing_content_type_is_malformed() {
    assert_malformed(WebhookRequest {
        connector: "webclip".into(),
        body: b"{}".to_vec(),
        headers: vec![],
    })
    .await;
}

#[tokio::test]
async fn text_mode_missing_url_header_is_malformed() {
    assert_malformed(WebhookRequest {
        connector: "webclip".into(),
        body: b"body".to_vec(),
        headers: vec![
            ("Content-Type".into(), "text/plain".into()),
            ("X-Cairn-Clip-Captured-At".into(), "1".into()),
        ],
    })
    .await;
}
