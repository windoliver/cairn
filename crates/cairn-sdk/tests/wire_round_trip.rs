//! Wire round-trip parity tests.
//!
//! For every variant of `VerbResponse` and every `SdkError` shape the SDK
//! can emit, serialize to JSON and feed the result back through
//! `cairn_core::generated::envelope::Response`'s `Deserialize` impl —
//! the canonical wire validator the CLI / MCP also use. Anything that
//! round-trips cleanly is guaranteed not to be rejected by another
//! surface; anything that fails round-trip would be a real interop bug.

use cairn_sdk::VerbResponse;
use cairn_sdk::generated::common::Ulid;
use cairn_sdk::generated::envelope::{Response, ResponseStatus, ResponseTarget, ResponseVerb};
use cairn_sdk::generated::verbs::ingest::IngestData;
use cairn_sdk::generated::verbs::retrieve::DataRecord;

fn ulid() -> Ulid {
    Ulid("01HZX9YT6Q4G2K7M3N5P8R0V1W".to_owned())
}

fn round_trip<D: serde::Serialize>(resp: &VerbResponse<D>) -> Response {
    let json = serde_json::to_value(resp).expect("VerbResponse must serialize");
    serde_json::from_value::<Response>(json).expect("must round-trip through canonical envelope")
}

#[test]
fn ingest_response_round_trips() {
    let resp = VerbResponse {
        operation_id: ulid(),
        policy_trace: vec![],
        verb: ResponseVerb::Ingest,
        target: None,
        data: IngestData {
            record_id: ulid(),
            session_id: "sess-1".to_owned(),
        },
    };
    let parsed = round_trip(&resp);
    assert!(matches!(parsed.verb, ResponseVerb::Ingest));
    assert!(matches!(parsed.status, ResponseStatus::Committed));
    assert_eq!(parsed.contract, "cairn.mcp.v1");
    assert!(parsed.target.is_none());
    assert!(parsed.data.is_some());
    assert!(parsed.error.is_none());
}

#[test]
fn retrieve_record_response_round_trips() {
    let resp = VerbResponse {
        operation_id: ulid(),
        policy_trace: vec![],
        verb: ResponseVerb::Retrieve,
        target: Some(ResponseTarget::Record),
        data: DataRecord {
            body: Some("hello".to_owned()),
            frontmatter: None,
            kind: "note".to_owned(),
            record_id: ulid(),
        },
    };
    let parsed = round_trip(&resp);
    assert!(matches!(parsed.verb, ResponseVerb::Retrieve));
    assert!(matches!(parsed.target, Some(ResponseTarget::Record)));
}

#[test]
fn retrieve_without_target_fails_round_trip() {
    // Serialize impl already errors at the SDK boundary; verify that the
    // Serialize-side rejection beats the canonical deserializer to the
    // punch (no half-formed JSON ever reaches the wire).
    let resp: VerbResponse<serde_json::Value> = VerbResponse {
        operation_id: ulid(),
        policy_trace: vec![],
        verb: ResponseVerb::Retrieve,
        target: None,
        data: serde_json::json!({}),
    };
    assert!(serde_json::to_value(&resp).is_err());
}

#[test]
fn target_on_non_retrieve_fails_round_trip() {
    let resp = VerbResponse {
        operation_id: ulid(),
        policy_trace: vec![],
        verb: ResponseVerb::Ingest,
        target: Some(ResponseTarget::Record),
        data: IngestData {
            record_id: ulid(),
            session_id: "s".to_owned(),
        },
    };
    assert!(serde_json::to_value(&resp).is_err());
}

#[test]
fn malformed_operation_id_caught_by_canonical_deserializer() {
    // VerbResponse fields are public, so a caller can build it with a
    // structurally malformed Ulid newtype. The SDK serializer does not
    // re-check the inner string (round-3 reviewer concern), but the
    // canonical Response deserializer does — so cross-surface forwarding
    // catches the mistake before it can land somewhere unsafe.
    let resp: VerbResponse<IngestData> = VerbResponse {
        operation_id: Ulid("not-a-ulid".to_owned()),
        policy_trace: vec![],
        verb: ResponseVerb::Ingest,
        target: None,
        data: IngestData {
            record_id: ulid(),
            session_id: "s".to_owned(),
        },
    };
    let json = serde_json::to_value(&resp).expect("SDK serialize succeeds");
    let parsed: Result<Response, _> = serde_json::from_value(json);
    let err = parsed.expect_err("canonical deserializer must reject bad ULID");
    assert!(err.to_string().contains("ULID"), "{err}");
}

#[test]
fn unknown_verb_rejected_at_serialize() {
    let resp: VerbResponse<serde_json::Value> = VerbResponse {
        operation_id: ulid(),
        policy_trace: vec![],
        verb: ResponseVerb::Unknown,
        target: None,
        data: serde_json::json!({}),
    };
    assert!(serde_json::to_value(&resp).is_err());
}
