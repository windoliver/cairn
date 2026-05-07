//! Integration coverage for issue #61 signed verb response helpers.

use cairn_cli::verbs::signed::{
    aborted, committed, committed_retrieve, rejected_from_domain, response_error_code,
};
use cairn_core::domain::DomainError;
use cairn_core::generated::common::Ulid;
use cairn_core::generated::envelope::{
    Response, ResponseData, ResponseStatus, ResponseVerb, RetrieveData,
};
use cairn_core::generated::verbs::ingest::IngestData;
use cairn_core::generated::verbs::retrieve::DataRecord;

#[test]
fn invalid_signature_maps_to_rejected_unauthorized() {
    let resp = rejected_from_domain(ResponseVerb::Ingest, DomainError::InvalidSignature);
    assert_eq!(resp.status, ResponseStatus::Rejected);
    assert_eq!(resp.verb, ResponseVerb::Ingest);
    assert_eq!(response_error_code(&resp), Some("Unauthorized"));
    assert!(resp.data.is_none());
    assert_response_round_trips(&resp);
}

#[test]
fn infrastructure_abort_uses_real_response_verb() {
    let resp = aborted(ResponseVerb::Ingest, "store open failed");
    assert_eq!(resp.status, ResponseStatus::Aborted);
    assert_eq!(resp.verb, ResponseVerb::Ingest);
    assert_eq!(response_error_code(&resp), Some("Internal"));
    assert_response_round_trips(&resp);
}

#[test]
fn non_retrieve_committed_response_round_trips_without_target() {
    let resp = committed(
        ResponseVerb::Ingest,
        fixed_ulid(),
        ResponseData::Ingest(IngestData {
            cache_hits: None,
            cache_misses: None,
            cache_writes: None,
            files_processed: None,
            plan_ref: None,
            record_id: fixed_ulid(),
            session_id: "default".to_owned(),
        }),
        vec![],
    );
    assert_eq!(resp.status, ResponseStatus::Committed);
    assert_eq!(resp.target, None);
    assert_response_round_trips(&resp);
}

#[test]
fn retrieve_committed_response_round_trips_with_target() {
    let resp = committed_retrieve(
        fixed_ulid(),
        RetrieveData::Record(DataRecord {
            body: Some("hello".to_owned()),
            frontmatter: None,
            kind: "reference".to_owned(),
            record_id: fixed_ulid(),
        }),
        vec![],
    );
    assert_eq!(resp.status, ResponseStatus::Committed);
    assert_eq!(resp.verb, ResponseVerb::Retrieve);
    assert_eq!(
        resp.target,
        Some(cairn_core::generated::envelope::ResponseTarget::Record)
    );
    assert_response_round_trips(&resp);
}

#[test]
#[should_panic(expected = "retrieve responses require committed_retrieve")]
fn committed_rejects_retrieve_without_target() {
    let _ = committed(
        ResponseVerb::Retrieve,
        fixed_ulid(),
        ResponseData::Retrieve(RetrieveData::Record(DataRecord {
            body: None,
            frontmatter: None,
            kind: "reference".to_owned(),
            record_id: fixed_ulid(),
        })),
        vec![],
    );
}

#[test]
#[should_panic(expected = "response verb must match response data")]
fn committed_rejects_mismatched_verb_and_data() {
    let _ = committed(
        ResponseVerb::Summarize,
        fixed_ulid(),
        ResponseData::Ingest(IngestData {
            cache_hits: None,
            cache_misses: None,
            cache_writes: None,
            files_processed: None,
            plan_ref: None,
            record_id: fixed_ulid(),
            session_id: "default".to_owned(),
        }),
        vec![],
    );
}

fn assert_response_round_trips(resp: &Response) {
    let json = serde_json::to_value(resp).expect("serialize response");
    let decoded: Response = serde_json::from_value(json).expect("deserialize response");
    assert_eq!(decoded.status, resp.status);
    assert_eq!(decoded.verb, resp.verb);
}

fn fixed_ulid() -> Ulid {
    Ulid("01HQZX9F5N0000000000000000".to_owned())
}
