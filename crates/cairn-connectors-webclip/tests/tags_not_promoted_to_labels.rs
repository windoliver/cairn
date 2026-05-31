//! User tags stay inside the payload; emitted labels are fixed.

use std::sync::Arc;

use cairn_connectors_core::{Connector, ConnectorPayload, CredentialHandle, WebhookContext};
use cairn_connectors_webclip::{WebClipConnector, testkit};

fn ctx() -> WebhookContext {
    WebhookContext::new(Arc::new(CredentialHandle::empty()), 1000)
}

#[tokio::test]
async fn tags_stay_in_payload_not_labels() {
    let c = WebClipConnector::new().unwrap();
    let body = include_str!("fixtures/clip_json.json"); // contains "tags"
    let events = c
        .ingest_webhook(&testkit::json_clip_request(b"s", body), &ctx())
        .await
        .unwrap();
    let e = &events[0];

    // Exactly the two manifest-declared labels — tags are NOT promoted.
    assert_eq!(e.labels.len(), 2);
    assert!(e.labels.contains("source:web") && e.labels.contains("kind:clip"));

    // Tags survive inside the JSON payload as data.
    match &e.payload {
        ConnectorPayload::Json { body, .. } => {
            assert!(
                body.get("tags").is_some(),
                "tags must be preserved in payload"
            );
        }
        other => panic!("expected Json payload, got {other:?}"),
    }
}
