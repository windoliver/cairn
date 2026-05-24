//! Round-trip tests for the propagation job payloads.
//!
//! Verifies that:
//! - The job-kind string constants are stable and match the values T7/T9
//!   actually enqueue.
//! - [`parse_outbound_share`] can deserialize the JSON bytes that T7 would
//!   write (using `propose_envelope.json` as a real fixture).
//! - [`parse_outbound_revoke`] can deserialize the JSON bytes that T9 would
//!   write (using `revoke_envelope.json` as a real fixture).

use cairn_workflows::propagation::payload::{
    ManifestEntry, OUTBOUND_REVOKE_KIND, OUTBOUND_SHARE_KIND, OutboundSharePayload,
    parse_outbound_revoke, parse_outbound_share,
};

#[test]
fn kind_strings_match_published_constants() {
    assert_eq!(OUTBOUND_SHARE_KIND, "federation.propagate.outbound_share");
    assert_eq!(OUTBOUND_REVOKE_KIND, "federation.propagate.outbound_revoke");
}

#[test]
fn outbound_share_payload_roundtrips() {
    // The fixture is a FederationEnvelope (generated wire type). The `link`
    // field holds a `generated::common::SignedShareLink`. We build an
    // OutboundSharePayload wrapping the domain-typed link plus a manifest
    // entry, serialize to JSON bytes, then deserialize via
    // `parse_outbound_share` and verify the round-trip.
    let envelope: cairn_core::domain::federation::FederationEnvelope = serde_json::from_str(
        include_str!("../../cairn-core/tests/fixtures/federation/propose_envelope.json"),
    )
    .expect("parse propose_envelope.json");

    let wire_link = envelope.link.expect("propose envelope must have a link");
    let domain_link =
        cairn_core::domain::federation::signed_share_link_from_wire(&wire_link).expect("from_wire");

    let manifest = vec![ManifestEntry {
        record_id: "01HQZX9F5N0000000000000099".to_owned(),
        kind: "user".to_owned(),
        body: "test body".to_owned(),
        body_hash: format!("sha256:{}", "b".repeat(64)),
        visibility: "team".to_owned(),
        scope_wire: "project=test-project".to_owned(),
        tags: vec!["tag-a".to_owned()],
    }];
    let payload = OutboundSharePayload {
        link: domain_link,
        manifest,
        peer: None,
    };

    let bytes = serde_json::to_vec(&payload).expect("serialize OutboundSharePayload");
    let parsed = parse_outbound_share(&bytes).expect("parse_outbound_share must succeed");

    // Verify the stable link_id field round-trips correctly.
    assert_eq!(parsed.link.link_id, wire_link.link_id);
    assert_eq!(parsed.manifest.len(), 1);
    assert_eq!(parsed.manifest[0].record_id, "01HQZX9F5N0000000000000099");
    assert_eq!(parsed.manifest[0].body, "test body");
}

#[test]
fn outbound_revoke_payload_roundtrips() {
    // The fixture is a FederationEnvelope. The `revocation` field holds a
    // `generated::common::SignedRevocation` which is the same type the
    // `domain::federation::SignedRevocation` re-exports.
    let envelope: cairn_core::domain::federation::FederationEnvelope = serde_json::from_str(
        include_str!("../../cairn-core/tests/fixtures/federation/revoke_envelope.json"),
    )
    .expect("parse revoke_envelope.json");

    let revocation = envelope
        .revocation
        .expect("revoke envelope must have a revocation");

    let bytes = serde_json::to_vec(&revocation).expect("serialize wire SignedRevocation");

    let parsed = parse_outbound_revoke(&bytes).expect("parse_outbound_revoke must succeed");

    // `SignedRevocation.link_id` is a plain `String` in both the wire and
    // domain types (the domain re-exports the generated type).
    assert_eq!(parsed.link_id, revocation.link_id);
    assert_eq!(parsed.issuer.0, revocation.issuer.0);
}
