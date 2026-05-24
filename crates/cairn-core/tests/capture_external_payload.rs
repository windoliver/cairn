//! Coverage for the `External` source family + `CapturePayload::External`
//! variant added for the v0.3 connector framework (issue #130).

use std::collections::BTreeSet;

use cairn_core::domain::capture::{CapturePayload, SourceFamily, SourceRef};
use cairn_core::pipeline::filter::redact::{RedactionSpan, RedactionTag};

#[test]
fn external_payload_reports_external_family() {
    let payload = CapturePayload::External {
        connector: "fixture".into(),
        source_ref: SourceRef::new("issue", "gh:owner/repo#42", None),
        labels: BTreeSet::from(["note".to_string()]),
        mime: "application/json".into(),
        redacted_spans: vec![RedactionSpan {
            start: 0,
            end: 10,
            tag: RedactionTag::Email,
        }],
    };
    assert_eq!(payload.source_family(), SourceFamily::External);
}

#[test]
fn external_payload_round_trips_through_serde() {
    let payload = CapturePayload::External {
        connector: "fixture".into(),
        source_ref: SourceRef::new("issue", "gh:owner/repo#42", None),
        labels: BTreeSet::from(["note".to_string(), "comment".into()]),
        mime: "application/json".into(),
        redacted_spans: vec![],
    };
    let json = serde_json::to_string(&payload).expect("serialize");
    let back: CapturePayload = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(payload, back);
}

#[test]
fn external_source_family_serializes_as_external_string() {
    let serialized = serde_json::to_string(&SourceFamily::External).expect("serialize");
    assert_eq!(serialized, "\"external\"");
}
