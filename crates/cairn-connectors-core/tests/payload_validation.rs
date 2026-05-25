//! Proptest: no raw byte from a webhook body can reach the emitted
//! `CaptureEvent` unredacted if the cairn-core redactor would have caught it.

use cairn_connectors_core::event::{
    ConnectorEvent, ConnectorEventId, ConnectorPayload, ConnectorScope, DeliveryMode, SourceRef,
};
use cairn_connectors_core::redact::RedactionPipeline;
use cairn_core::pipeline::filter::redact::{RedactionTag, redact as core_redact};
use proptest::prelude::*;
use std::collections::BTreeSet;

proptest! {
    #[test]
    fn redaction_removes_all_pii_the_core_detector_finds(
        body in r"[a-z0-9 ]{0,40}(alice@example\.com)?[a-z0-9 ]{0,40}",
    ) {
        let event = ConnectorEvent::new(
            ConnectorEventId::new("01HX0000000000000000000000"),
            "fixture",
            SourceRef::new("issue", "x", None),
            0,
            BTreeSet::new(),
            ConnectorScope::project("p"),
            ConnectorPayload::Text { mime: "text/plain".into(), body: body.clone() },
            DeliveryMode::Poll { cursor: None },
        );
        let core_result = core_redact(&body);
        let expected_spans = core_result.spans.len();
        let r = RedactionPipeline::new().redact(event).unwrap();
        // Invariant 1: span count matches core redactor exactly.
        prop_assert_eq!(r.spans.len(), expected_spans);
        // Invariant 2: if the core redactor specifically detected an email span
        // covering alice@example.com, the pipeline must have removed it from the
        // output. We narrow to Email-tagged spans because other detectors (e.g.
        // Phone) may fire on adjacent digit sequences without covering the email
        // address itself — in those cases alice@example.com legitimately remains
        // as non-redacted text (the \b word boundary before the local part was
        // not satisfied) and the invariant does not apply.
        let email_detected = core_result.spans.iter().any(|s| {
            s.tag == RedactionTag::Email
                && body
                    .get(s.start..s.end)
                    .is_some_and(|slice| slice.contains("alice@example.com"))
        });
        if email_detected {
            if let ConnectorPayload::Text { body: out, .. } = &r.event.payload {
                prop_assert!(!out.contains("alice@example.com"));
            } else {
                unreachable!();
            }
        }
    }
}
