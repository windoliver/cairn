//! Integration coverage for issue #61 signed verb response helpers.

use std::process::Command;

use cairn_cli::vault::{BootstrapOpts, bootstrap};
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
use rusqlite::Connection;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

#[test]
fn ingest_body_commits_record_and_policy_trace() {
    let vault = tempfile::tempdir().expect("vault");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap");
    let out = cli()
        .current_dir(vault.path())
        .args([
            "ingest",
            "--kind",
            "reference",
            "--body",
            "remember alice@example.com as project contact",
            "--json",
        ])
        .output()
        .expect("run ingest");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(json["status"], "committed");
    assert!(json["data"]["record_id"].as_str().is_some());
    assert!(json["policy_trace"].as_array().expect("trace").len() >= 4);
}

#[test]
fn rejected_secret_ingest_does_not_auto_provision_default_issuer() {
    let vault = tempfile::tempdir().expect("vault");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap");
    let out = cli()
        .current_dir(vault.path())
        .args([
            "ingest",
            "--kind",
            "reference",
            "--body",
            "api_key = sk-test-12345678901234567890",
            "--json",
        ])
        .output()
        .expect("run ingest");
    assert_eq!(
        out.status.code(),
        Some(65),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(json["status"], "rejected");

    let conn = Connection::open(vault.path().join(".cairn/cairn.db")).expect("open db");
    let identities: i64 = conn
        .query_row("SELECT COUNT(*) FROM identities", [], |r| r.get(0))
        .expect("identity count");
    let vault_meta: i64 = conn
        .query_row("SELECT COUNT(*) FROM vault_meta", [], |r| r.get(0))
        .expect("vault_meta count");
    let records: i64 = conn
        .query_row("SELECT COUNT(*) FROM records", [], |r| r.get(0))
        .expect("records count");
    let wal_ops: i64 = conn
        .query_row("SELECT COUNT(*) FROM wal_ops", [], |r| r.get(0))
        .expect("wal_ops count");
    assert_eq!(identities, 0);
    assert_eq!(vault_meta, 0);
    assert_eq!(records, 0);
    assert_eq!(wal_ops, 0);
}

#[test]
fn session_scoped_body_ingest_rejects_before_opening_store() {
    let vault = tempfile::tempdir().expect("vault");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap");
    let db_path = vault.path().join(".cairn/cairn.db");
    assert!(!db_path.exists(), "bootstrap should not create store DB");

    let out = cli()
        .current_dir(vault.path())
        .args([
            "ingest",
            "--kind",
            "reference",
            "--body",
            "remember this for a session",
            "--session",
            "s1",
            "--json",
        ])
        .output()
        .expect("run ingest");
    assert_eq!(
        out.status.code(),
        Some(64),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(json["status"], "rejected");
    assert_eq!(json["error"]["code"], "InvalidArgs");
    assert!(
        !db_path.exists(),
        "session reject must happen before DB open"
    );
}

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
