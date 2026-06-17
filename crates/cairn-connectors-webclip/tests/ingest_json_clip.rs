//! JSON clip -> one per-domain-scoped `ConnectorEvent`.

use std::sync::Arc;

use cairn_connectors_core::{Connector, ConnectorPayload, CredentialHandle, WebhookContext};
use cairn_connectors_webclip::{WebClipConnector, testkit};

fn ctx() -> WebhookContext {
    WebhookContext::new(Arc::new(CredentialHandle::empty()), 1000)
}

#[tokio::test]
async fn json_clip_produces_one_scoped_event() {
    let connector = WebClipConnector::new().expect("construct");
    let body = include_str!("fixtures/clip_json.json");
    let req = testkit::json_clip_request(b"secret", body);

    let events = connector
        .ingest_webhook(&req, &ctx())
        .await
        .expect("ingest");
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert_eq!(e.connector, "webclip");
    assert_eq!(e.source_ref.kind, "clip");
    assert_eq!(e.scope.kind, "domain");
    assert_eq!(e.scope.value, "en.wikipedia.org");
    assert_eq!(e.occurred_at, 1_748_563_200);
    assert!(e.labels.contains("source:web"));
    assert!(e.labels.contains("kind:clip"));
    assert!(matches!(e.payload, ConnectorPayload::Json { .. }));
}
