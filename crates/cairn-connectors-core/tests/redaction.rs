//! T20e — Redaction span coverage.
//!
//! Build a `ConnectorEvent` whose JSON payload has an email address in a
//! string leaf. After passing through `RedactionPipeline`:
//!   - `r.spans` must be non-empty (at least one redaction fired).
//!   - The serialised event must not contain the raw email.
//!
//! Issue #130, brief §5.2 + §14 pre-capture redaction.

#![allow(missing_docs)]

use std::collections::BTreeSet;

use cairn_connectors_core::event::{
    ConnectorEvent, ConnectorEventId, ConnectorPayload, ConnectorScope, DeliveryMode, SourceRef,
};
use cairn_connectors_core::redact::RedactionPipeline;

#[test]
fn email_in_json_leaf_records_span_and_masks_body() {
    let event = ConnectorEvent::new(
        ConnectorEventId::new("01HX0000000000000000000000"),
        "fixture",
        SourceRef::new("issue", "x", None),
        0,
        BTreeSet::new(),
        ConnectorScope::project("p"),
        ConnectorPayload::Json {
            mime: "application/json".into(),
            body: serde_json::json!({"author": "alice@example.com"}),
        },
        DeliveryMode::Poll { cursor: None },
    );

    let r = RedactionPipeline::new()
        .redact(event)
        .expect("redact must succeed");

    assert!(
        !r.spans.is_empty(),
        "expected at least one redaction span for the email address",
    );

    let json = serde_json::to_string(&r.event).expect("serialise redacted event");
    assert!(
        !json.contains("alice@example.com"),
        "serialised event must not contain the raw email address after redaction",
    );
}
