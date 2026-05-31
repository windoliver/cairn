//! Content-Type acceptance / rejection table.

use std::sync::Arc;

use cairn_connectors_core::{
    Connector, ConnectorError, CredentialHandle, WebhookContext, WebhookRequest,
};
use cairn_connectors_webclip::WebClipConnector;
use rstest::rstest;

fn ctx() -> WebhookContext {
    WebhookContext::new(Arc::new(CredentialHandle::empty()), 1000)
}

fn raw_request(content_type: &str, body: &[u8]) -> WebhookRequest {
    WebhookRequest {
        connector: "webclip".into(),
        body: body.to_vec(),
        headers: vec![("Content-Type".into(), content_type.into())],
    }
}

#[rstest]
#[case("text/html")]
#[case("application/xml")]
#[case("image/png")]
#[tokio::test]
async fn rejects_unsupported_content_type(#[case] content_type: &str) {
    let connector = WebClipConnector::new().unwrap();
    let req = raw_request(content_type, b"whatever");
    let result = connector.ingest_webhook(&req, &ctx()).await;
    assert!(
        matches!(result, Err(ConnectorError::MalformedPayload(_))),
        "expected MalformedPayload for {content_type}, got {result:?}"
    );
}

#[tokio::test]
async fn accepts_json_with_charset_parameter() {
    let connector = WebClipConnector::new().unwrap();
    let body = br#"{"url":"https://e.com/a","captured_at":1,"selection":"x"}"#;
    let req = WebhookRequest {
        connector: "webclip".into(),
        body: body.to_vec(),
        headers: vec![(
            "Content-Type".into(),
            "application/json; charset=utf-8".into(),
        )],
    };
    assert!(connector.ingest_webhook(&req, &ctx()).await.is_ok());
}
