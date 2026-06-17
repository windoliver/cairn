//! text/markdown clip body + X-Cairn-Clip-* headers -> Text-payload event.

use std::sync::Arc;

use cairn_connectors_core::{Connector, ConnectorPayload, CredentialHandle, WebhookContext};
use cairn_connectors_webclip::{WebClipConnector, testkit};

fn ctx() -> WebhookContext {
    WebhookContext::new(Arc::new(CredentialHandle::empty()), 1000)
}

#[tokio::test]
async fn markdown_clip_produces_text_payload_event() {
    let connector = WebClipConnector::new().expect("construct");
    let body = include_str!("fixtures/clip_markdown.md");
    let req = testkit::text_clip_request(
        b"secret",
        "text/markdown",
        "https://example.com/post/42",
        1_748_563_200,
        body,
    );

    let events = connector
        .ingest_webhook(&req, &ctx())
        .await
        .expect("ingest");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].scope.value, "example.com");
    assert!(matches!(events[0].payload, ConnectorPayload::Text { .. }));
}
