//! Integration coverage for issue #61 signed verb response helpers.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use cairn_cli::vault::{BootstrapOpts, bootstrap};
use cairn_cli::verbs::signed::{
    aborted, committed, committed_retrieve, rejected_from_domain, response_error_code,
};
use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::domain::{
    ActorChainEntry, CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, CaptureRefs,
    ChainRole, DomainError, Identity, MemoryVisibility, PayloadHash, Rfc3339Timestamp,
    SourceFamily,
};
use cairn_core::generated::common::Ulid;
use cairn_core::generated::envelope::{
    Response, ResponseData, ResponsePolicyTraceResult, ResponseStatus, ResponseTarget,
    ResponseVerb, RetrieveData,
};
use cairn_core::generated::verbs::ingest::IngestData;
use cairn_core::generated::verbs::retrieve::{DataRecord, DataToolCall, DataTurn, TurnItemRole};
use rusqlite::Connection;
use sha2::{Digest as _, Sha256};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn enable_hook_sensor(vault: &Path) {
    let out = cli()
        .current_dir(vault)
        .args([
            "sensor",
            "enable",
            "hook",
            "--reason",
            "operator_on",
            "--json",
        ])
        .output()
        .expect("enable hook sensor");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn retrieve_turn_json(
    vault: &Path,
    session_id: &str,
    turn_id: &str,
    include_tool_calls: bool,
) -> DataTurn {
    let mut cmd = cli();
    cmd.current_dir(vault)
        .args(["retrieve", "--session", session_id, "--turn", turn_id]);
    if include_tool_calls {
        cmd.args(["--include", "tool_calls"]);
    }
    let out = cmd.arg("--json").output().expect("run retrieve turn");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let resp: Response = serde_json::from_slice(&out.stdout).expect("response");
    let Some(ResponseData::Retrieve(RetrieveData::Turn(data))) = resp.data else {
        panic!("retrieve turn must return turn data");
    };
    data
}

fn retrieve_tool_call_json(vault: &Path, session_id: &str, turn_id: &str) -> DataToolCall {
    let out = cli()
        .current_dir(vault)
        .args([
            "retrieve",
            "--tool-call",
            "call-1",
            "--session",
            session_id,
            "--turn",
            turn_id,
            "--json",
        ])
        .output()
        .expect("run retrieve tool-call");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let resp: Response = serde_json::from_slice(&out.stdout).expect("response");
    assert_eq!(resp.target, Some(ResponseTarget::ToolCall));
    let Some(ResponseData::Retrieve(RetrieveData::ToolCall(data))) = resp.data else {
        panic!("retrieve tool-call must return tool-call data");
    };
    data
}

fn assert_policy_trace_body_free(value: &serde_json::Value) {
    let trace = value["policy_trace"]
        .as_array()
        .expect("policy_trace array");
    for entry in trace {
        let text = serde_json::to_string(entry).expect("trace entry json");
        assert!(!text.contains("alice@example.com"));
        assert!(!text.contains("sk-test"));
        assert!(!text.contains("secret"));
    }
}

fn normalize_ulids_for_snapshot(value: &mut serde_json::Value) {
    const ULID_LEN: usize = 26;
    const PLACEHOLDER: &str = "01XXXXXXXXXXXXXXXXXXXXXXXX";

    match value {
        serde_json::Value::String(s) => {
            if s.len() == ULID_LEN
                && s.chars()
                    .all(|c| matches!(c, '0'..='9' | 'A'..='H' | 'J'..='K' | 'M'..='N' | 'P'..='T' | 'V'..='Z'))
            {
                PLACEHOLDER.clone_into(s);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                normalize_ulids_for_snapshot(item);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values_mut() {
                normalize_ulids_for_snapshot(item);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

#[test]
fn live_ingest_policy_trace_is_body_free() {
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
            "project",
            "--body",
            "alice@example.com has secret sk-test-12345678901234567890",
            "--json",
        ])
        .output()
        .expect("run ingest");
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_policy_trace_body_free(&json);
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
fn retrieve_record_json_commits_typed_record_target() {
    let vault = tempfile::tempdir().expect("vault");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap");

    let ingest = cli()
        .current_dir(vault.path())
        .args([
            "ingest",
            "--kind",
            "reference",
            "--body",
            "retrievable issue 61 cli body",
            "--json",
        ])
        .output()
        .expect("run ingest");
    assert_eq!(
        ingest.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&ingest.stderr)
    );
    let ingest_json: serde_json::Value = serde_json::from_slice(&ingest.stdout).expect("json");
    let record_id = ingest_json["data"]["record_id"]
        .as_str()
        .expect("record_id")
        .to_owned();

    let retrieve = cli()
        .current_dir(vault.path())
        .args(["retrieve", &record_id, "--json"])
        .output()
        .expect("run retrieve");
    assert_eq!(
        retrieve.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&retrieve.stderr)
    );
    let resp: Response = serde_json::from_slice(&retrieve.stdout).expect("response");
    assert_eq!(resp.status, ResponseStatus::Committed);
    assert_eq!(resp.verb, ResponseVerb::Retrieve);
    assert_eq!(resp.target, Some(ResponseTarget::Record));
    assert!(resp.policy_trace.iter().any(|entry| {
        entry.gate == "read.visibility" && entry.result == ResponsePolicyTraceResult::Pass
    }));
    let Some(ResponseData::Retrieve(RetrieveData::Record(data))) = resp.data else {
        panic!("retrieve record must return typed record data");
    };
    assert_eq!(data.record_id.0, record_id);
    assert_eq!(data.kind, "reference");
    assert_eq!(data.body.as_deref(), Some("retrievable issue 61 cli body"));
}

#[test]
fn summarize_json_commits_authorized_rollup() {
    let vault = tempfile::tempdir().expect("vault");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap");
    let alpha = ingest_reference(vault.path(), "Alpha detail for the project");
    let beta = ingest_reference(vault.path(), "Beta detail for the project");

    let summarize = cli()
        .current_dir(vault.path())
        .args(["summarize", &beta, &alpha, "--citations", "off", "--json"])
        .output()
        .expect("run summarize");
    assert_eq!(
        summarize.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&summarize.stderr)
    );
    let mut snapshot_json: serde_json::Value =
        serde_json::from_slice(&summarize.stdout).expect("snapshot json");
    normalize_ulids_for_snapshot(&mut snapshot_json);
    insta::assert_snapshot!(
        "summarize_triple_form_json",
        serde_json::to_string_pretty(&snapshot_json).expect("snapshot string")
    );

    let resp: Response = serde_json::from_slice(&summarize.stdout).expect("response");
    assert_eq!(resp.status, ResponseStatus::Committed);
    assert_eq!(resp.verb, ResponseVerb::Summarize);
    assert!(resp.policy_trace.iter().any(|entry| {
        entry.gate == "read.visibility" && entry.result == ResponsePolicyTraceResult::Pass
    }));
    let Some(ResponseData::Summarize(data)) = resp.data else {
        panic!("summarize must return summary data");
    };
    assert!(data.digest.contains("Alpha detail for the project"));
    assert!(data.digest.contains("Beta detail for the project"));
    assert_eq!(data.narrative, "");
    assert!(data.facts.iter().any(|fact| {
        fact.object == "Alpha detail for the project"
            && fact.confidence == cairn_core::generated::verbs::summarize::ConfidenceTag::Extracted
            && fact.source_record_ids.iter().any(|id| id.0 == alpha)
    }));
    assert!(data.concepts.iter().any(|concept| {
        concept.name == "project"
            && concept.kind == cairn_core::generated::verbs::summarize::ConceptKind::Topic
    }));
    assert!(
        !data.digest.contains(&alpha),
        "citations=off should omit source record ids"
    );
}

#[test]
fn summarize_persist_writes_summary_record() {
    let vault = tempfile::tempdir().expect("vault");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap");
    let alpha = ingest_reference(vault.path(), "Alpha persisted summary detail");
    let beta = ingest_reference(vault.path(), "Beta persisted summary detail");

    let summarize = cli()
        .current_dir(vault.path())
        .args(["summarize", &alpha, &beta, "--persist", "--json"])
        .output()
        .expect("run summarize");
    assert_eq!(
        summarize.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&summarize.stderr)
    );
    let resp: Response = serde_json::from_slice(&summarize.stdout).expect("response");
    assert_eq!(resp.status, ResponseStatus::Committed);
    assert!(resp.policy_trace.iter().any(|entry| {
        entry.gate == "write.wal" && entry.result == ResponsePolicyTraceResult::Pass
    }));
    assert!(resp.policy_trace.iter().any(|entry| {
        entry.gate == "write.consent" && entry.result == ResponsePolicyTraceResult::Pass
    }));
    let Some(ResponseData::Summarize(data)) = resp.data else {
        panic!("summarize must return summary data");
    };
    let persisted = data
        .persisted_record_id
        .as_ref()
        .expect("persisted record id")
        .0
        .clone();

    let retrieve = cli()
        .current_dir(vault.path())
        .args(["retrieve", &persisted, "--json"])
        .output()
        .expect("retrieve persisted summary");
    assert_eq!(
        retrieve.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&retrieve.stderr)
    );
    let resp: Response = serde_json::from_slice(&retrieve.stdout).expect("response");
    let Some(ResponseData::Retrieve(RetrieveData::Record(data))) = resp.data else {
        panic!("retrieve must return persisted summary record");
    };
    let body = data.body.expect("summary body");
    assert!(body.contains("Alpha persisted summary detail"));
    assert!(body.contains("Beta persisted summary detail"));
    let source_ids = data
        .frontmatter
        .as_ref()
        .and_then(|frontmatter| frontmatter.get("summary_sources"))
        .and_then(serde_json::Value::as_array)
        .expect("summary source ids");
    assert!(source_ids.iter().any(|id| id.as_str() == Some(&alpha)));
    assert!(source_ids.iter().any(|id| id.as_str() == Some(&beta)));
}

#[test]
fn summarize_rejects_unknown_issuer_before_returning_data() {
    let vault = tempfile::tempdir().expect("vault");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap");
    let record_id = ingest_reference(vault.path(), "issuer-gated summarize body");

    let summarize = cli()
        .current_dir(vault.path())
        .env("CAIRN_ISSUER", "agt:cairn-cli:missing:reader:v1")
        .args(["summarize", &record_id, "--json"])
        .output()
        .expect("run summarize");
    assert_eq!(
        summarize.status.code(),
        Some(64),
        "stderr={}",
        String::from_utf8_lossy(&summarize.stderr)
    );
    let resp: Response = serde_json::from_slice(&summarize.stdout).expect("response");
    assert_eq!(resp.status, ResponseStatus::Rejected);
    assert_eq!(resp.verb, ResponseVerb::Summarize);
    assert!(resp.data.is_none());
    assert_eq!(response_error_code(&resp), Some("Unauthorized"));
}

#[test]
fn summarize_rejects_private_record_from_other_active_issuer() {
    let vault = tempfile::tempdir().expect("vault");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap");
    let record_id = ingest_reference(vault.path(), "private summarize source body");
    let other_issuer = provision_agent(vault.path(), "cairn-cli:other:reader");

    let summarize = cli()
        .current_dir(vault.path())
        .env("CAIRN_ISSUER", other_issuer)
        .args(["summarize", &record_id, "--json"])
        .output()
        .expect("run summarize");
    assert_eq!(
        summarize.status.code(),
        Some(64),
        "stderr={}",
        String::from_utf8_lossy(&summarize.stderr)
    );
    let resp: Response = serde_json::from_slice(&summarize.stdout).expect("response");
    assert_eq!(resp.status, ResponseStatus::Rejected);
    assert_eq!(resp.verb, ResponseVerb::Summarize);
    assert!(resp.data.is_none());
    assert_eq!(response_error_code(&resp), Some("InvalidArgs"));
    assert!(resp.policy_trace.iter().any(|entry| {
        entry.gate == "read.scope" && entry.result == ResponsePolicyTraceResult::Pass
    }));
}

#[test]
fn summarize_rejects_session_record_without_session_authorization() {
    let vault = tempfile::tempdir().expect("vault");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap");
    let _ = ingest_reference(vault.path(), "seed default issuer");
    let record_id = insert_session_record(vault.path(), "session-only summarize source body");

    let summarize = cli()
        .current_dir(vault.path())
        .args(["summarize", &record_id, "--json"])
        .output()
        .expect("run summarize");
    assert_eq!(
        summarize.status.code(),
        Some(64),
        "stderr={}",
        String::from_utf8_lossy(&summarize.stderr)
    );
    let resp: Response = serde_json::from_slice(&summarize.stdout).expect("response");
    assert_eq!(resp.status, ResponseStatus::Rejected);
    assert_eq!(resp.verb, ResponseVerb::Summarize);
    assert!(resp.data.is_none());
    assert_eq!(response_error_code(&resp), Some("InvalidArgs"));
    assert!(resp.policy_trace.iter().any(|entry| {
        entry.gate == "read.scope" && entry.result == ResponsePolicyTraceResult::Pass
    }));
}

#[test]
fn summarize_rejects_missing_authorized_source_record() {
    let vault = tempfile::tempdir().expect("vault");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap");
    let record_id = ingest_reference(vault.path(), "summarize source body");
    let missing = "01HQZX9F5N0000000000000000";

    let summarize = cli()
        .current_dir(vault.path())
        .args(["summarize", &record_id, missing, "--json"])
        .output()
        .expect("run summarize");
    assert_eq!(
        summarize.status.code(),
        Some(64),
        "stderr={}",
        String::from_utf8_lossy(&summarize.stderr)
    );
    let resp: Response = serde_json::from_slice(&summarize.stdout).expect("response");
    assert_eq!(resp.status, ResponseStatus::Rejected);
    assert_eq!(resp.verb, ResponseVerb::Summarize);
    assert!(resp.data.is_none());
    assert_eq!(response_error_code(&resp), Some("InvalidArgs"));
    assert!(resp.policy_trace.iter().any(|entry| {
        entry.gate == "read.scope" && entry.result == ResponsePolicyTraceResult::Pass
    }));
}

#[test]
fn summarize_persist_reject_keeps_source_read_trace() {
    let vault = tempfile::tempdir().expect("vault");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap");
    let record_id = ingest_reference(vault.path(), "summarize invalid-kind source body");

    let summarize = cli()
        .current_dir(vault.path())
        .args([
            "summarize",
            &record_id,
            "--persist",
            "--kind",
            "not_a_kind",
            "--json",
        ])
        .output()
        .expect("run summarize");
    assert_eq!(
        summarize.status.code(),
        Some(64),
        "stderr={}",
        String::from_utf8_lossy(&summarize.stderr)
    );
    let resp: Response = serde_json::from_slice(&summarize.stdout).expect("response");
    assert_eq!(resp.status, ResponseStatus::Rejected);
    assert!(resp.policy_trace.iter().any(|entry| {
        entry.gate == "read.scope" && entry.result == ResponsePolicyTraceResult::Pass
    }));
}

#[test]
fn retrieve_record_rejects_incompatible_include_before_opening_store() {
    let vault = tempfile::tempdir().expect("vault");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap");
    let db_path = vault.path().join(".cairn/cairn.db");
    assert!(!db_path.exists(), "bootstrap should not create store DB");

    let retrieve = cli()
        .current_dir(vault.path())
        .args([
            "retrieve",
            "01HQZX9F5N0000000000000000",
            "--include",
            "reasoning",
            "--json",
        ])
        .output()
        .expect("run retrieve");
    assert_eq!(
        retrieve.status.code(),
        Some(64),
        "stderr={}",
        String::from_utf8_lossy(&retrieve.stderr)
    );
    let resp: Response = serde_json::from_slice(&retrieve.stdout).expect("response");
    assert_eq!(resp.status, ResponseStatus::Rejected);
    assert_eq!(response_error_code(&resp), Some("InvalidArgs"));
    assert!(!db_path.exists(), "arg reject must happen before DB open");
}

#[test]
fn retrieve_record_rejects_unknown_issuer_before_returning_data() {
    let vault = tempfile::tempdir().expect("vault");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap");
    let record_id = ingest_reference(vault.path(), "issuer-gated retrieve body");

    let retrieve = cli()
        .current_dir(vault.path())
        .env("CAIRN_ISSUER", "agt:cairn-cli:missing:reader:v1")
        .args(["retrieve", &record_id, "--json"])
        .output()
        .expect("run retrieve");
    assert_eq!(
        retrieve.status.code(),
        Some(64),
        "stderr={}",
        String::from_utf8_lossy(&retrieve.stderr)
    );
    let resp: Response = serde_json::from_slice(&retrieve.stdout).expect("response");
    assert_eq!(resp.status, ResponseStatus::Rejected);
    assert_eq!(resp.verb, ResponseVerb::Retrieve);
    assert!(resp.data.is_none());
    assert_eq!(response_error_code(&resp), Some("Unauthorized"));
}

#[test]
fn retrieve_scope_excludes_rows_outside_requested_scope() {
    let vault = tempfile::tempdir().expect("vault");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap");
    let record_id = ingest_reference(vault.path(), "scoped retrieve body");

    let in_scope = cli()
        .current_dir(vault.path())
        .args(["retrieve", "--scope", r#"{"entity":"ingest"}"#, "--json"])
        .output()
        .expect("run retrieve scope");
    assert_eq!(
        in_scope.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&in_scope.stderr)
    );
    let resp: Response = serde_json::from_slice(&in_scope.stdout).expect("response");
    assert_eq!(resp.status, ResponseStatus::Committed);
    assert_eq!(resp.target, Some(ResponseTarget::Scope));
    let Some(ResponseData::Retrieve(RetrieveData::Scope(data))) = resp.data else {
        panic!("retrieve scope must return scope data");
    };
    assert!(data.items.iter().any(|item| item.record_id.0 == record_id));

    let out_of_scope = cli()
        .current_dir(vault.path())
        .args(["retrieve", "--scope", r#"{"entity":"other"}"#, "--json"])
        .output()
        .expect("run retrieve scope");
    assert_eq!(
        out_of_scope.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out_of_scope.stderr)
    );
    let resp: Response = serde_json::from_slice(&out_of_scope.stdout).expect("response");
    let Some(ResponseData::Retrieve(RetrieveData::Scope(data))) = resp.data else {
        panic!("retrieve scope must return scope data");
    };
    assert!(data.items.is_empty());
}

#[test]
fn retrieve_session_after_capture_trace_honors_scope_and_limit() {
    const SESSION_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let vault = tempfile::tempdir().expect("vault");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap");
    let _ = ingest_reference(vault.path(), "seed default issuer before retrieve");
    enable_hook_sensor(vault.path());
    let trace_path = write_issue_61_trace_fixture(vault.path(), SESSION_ID);

    let capture = cli()
        .current_dir(vault.path())
        .args([
            "capture_trace",
            "--from",
            trace_path.to_str().expect("utf-8 trace path"),
            "--json",
        ])
        .output()
        .expect("run capture_trace");
    assert_eq!(
        capture.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&capture.stderr)
    );

    let retrieve = cli()
        .current_dir(vault.path())
        .args([
            "retrieve",
            "--session",
            SESSION_ID,
            "--limit",
            "2",
            "--order",
            "asc",
            "--json",
        ])
        .output()
        .expect("run retrieve session");
    assert_eq!(
        retrieve.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&retrieve.stderr)
    );
    let resp: Response = serde_json::from_slice(&retrieve.stdout).expect("response");
    let Some(ResponseData::Retrieve(RetrieveData::Session(data))) = resp.data else {
        panic!("retrieve session must return session data");
    };
    assert_eq!(data.items.len(), 3);
    assert_eq!(
        data.items[0].content.as_deref(),
        Some("first trace message")
    );
    assert_eq!(
        data.items[1].content.as_deref(),
        Some("second trace message")
    );
    assert_eq!(
        data.items[2].content.as_deref(),
        Some("third trace message")
    );
}

#[test]
fn retrieve_session_windows_by_turn_with_cursor() {
    const SESSION_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let vault = tempfile::tempdir().expect("vault");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap");
    let _ = ingest_reference(vault.path(), "seed default issuer before retrieve");
    let trace_path = write_issue_78_trace_fixture(vault.path(), SESSION_ID);
    run_capture_trace(vault.path(), &trace_path);

    let first = cli()
        .current_dir(vault.path())
        .args([
            "retrieve",
            "--session",
            SESSION_ID,
            "--limit",
            "2",
            "--order",
            "desc",
            "--json",
        ])
        .output()
        .expect("run retrieve session");
    assert_eq!(
        first.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&first.stderr)
    );
    let resp: Response = serde_json::from_slice(&first.stdout).expect("response");
    let Some(ResponseData::Retrieve(RetrieveData::Session(data))) = resp.data else {
        panic!("retrieve session must return session data");
    };
    assert!(data.items.iter().any(|item| item.turn_id == "turn-3"));
    assert!(data.items.iter().any(|item| item.turn_id == "turn-2"));
    assert!(!data.items.iter().any(|item| item.turn_id == "turn-1"));
    let cursor = data.next_cursor.expect("next cursor").0;

    let second = cli()
        .current_dir(vault.path())
        .args([
            "retrieve",
            "--session",
            SESSION_ID,
            "--limit",
            "2",
            "--order",
            "desc",
            "--cursor",
            &cursor,
            "--json",
        ])
        .output()
        .expect("run retrieve session cursor");
    assert_eq!(
        second.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&second.stderr)
    );
    let resp: Response = serde_json::from_slice(&second.stdout).expect("response");
    let Some(ResponseData::Retrieve(RetrieveData::Session(data))) = resp.data else {
        panic!("retrieve session must return session data");
    };
    assert!(data.items.iter().all(|item| item.turn_id == "turn-1"));
    assert!(data.next_cursor.is_none());
}

#[test]
fn retrieve_turn_and_tool_call_return_linkage() {
    const SESSION_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let vault = tempfile::tempdir().expect("vault");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap");
    let _ = ingest_reference(vault.path(), "seed default issuer before retrieve");
    let trace_path = write_issue_78_trace_fixture(vault.path(), SESSION_ID);
    run_capture_trace(vault.path(), &trace_path);

    let data = retrieve_turn_json(vault.path(), SESSION_ID, "turn-1", false);
    assert!(
        data.turn
            .iter()
            .any(|item| item.role == TurnItemRole::Tool && item.content.is_none())
    );

    let data = retrieve_turn_json(vault.path(), SESSION_ID, "turn-1", true);
    assert!(
        data.turn
            .iter()
            .any(|item| item.content.as_deref() == Some("run cargo test"))
    );
    assert!(data.turn.iter().any(|item| {
        item.linkage
            .as_ref()
            .and_then(|linkage| linkage.tool_call_id.as_deref())
            == Some("call-1")
    }));

    let data = retrieve_tool_call_json(vault.path(), SESSION_ID, "turn-1");
    assert_eq!(data.tool_call_id, "call-1");
    assert_eq!(data.items.len(), 2);
    assert!(data.items.iter().all(|item| {
        item.linkage
            .as_ref()
            .and_then(|linkage| linkage.tool_call_id.as_deref())
            == Some("call-1")
    }));
}

#[test]
fn retrieve_session_budget_trace_is_deterministic_and_body_free() {
    const SESSION_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let vault = tempfile::tempdir().expect("vault");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap");
    set_retrieve_budget(vault.path(), 20);
    let _ = ingest_reference(vault.path(), "seed default issuer before retrieve");
    let trace_path = write_issue_78_trace_fixture(vault.path(), SESSION_ID);
    run_capture_trace(vault.path(), &trace_path);

    let retrieve = cli()
        .current_dir(vault.path())
        .args([
            "retrieve",
            "--session",
            SESSION_ID,
            "--order",
            "asc",
            "--json",
        ])
        .output()
        .expect("run retrieve session");
    assert_eq!(
        retrieve.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&retrieve.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&retrieve.stdout).expect("json");
    let trace = value["policy_trace"].as_array().expect("policy_trace");
    let budget = trace
        .iter()
        .find(|entry| entry["gate"] == "read.budget")
        .expect("read.budget");
    let detail = budget["detail"].as_str().expect("detail");
    assert!(detail.contains("chars=20"), "detail: {detail}");
    assert!(detail.contains("trimmed=true"), "detail: {detail}");
    assert!(
        !detail.contains("turn one user"),
        "budget trace must be body-free: {detail}"
    );
}

#[test]
fn retrieve_session_rehydrate_adds_body_free_trace() {
    const SESSION_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let vault = tempfile::tempdir().expect("vault");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap");
    let _ = ingest_reference(vault.path(), "seed default issuer before retrieve");
    let trace_path = write_issue_78_trace_fixture(vault.path(), SESSION_ID);
    run_capture_trace(vault.path(), &trace_path);

    let retrieve = cli()
        .current_dir(vault.path())
        .args([
            "retrieve",
            "--session",
            SESSION_ID,
            "--rehydrate",
            "--order",
            "asc",
            "--json",
        ])
        .output()
        .expect("run retrieve session rehydrate");
    assert_eq!(
        retrieve.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&retrieve.stderr)
    );
    let output = String::from_utf8(retrieve.stdout).expect("utf-8 stdout");
    assert!(
        !output.contains("alice@example.com"),
        "rehydrate output must not leak raw email text: {output}"
    );
    let value: serde_json::Value = serde_json::from_str(&output).expect("json");
    let trace = value["policy_trace"].as_array().expect("policy_trace");
    let rehydrate = trace
        .iter()
        .find(|entry| entry["gate"] == "read.rehydrate")
        .expect("read.rehydrate");
    assert_eq!(rehydrate["result"], "pass");
    let detail = rehydrate["detail"].as_str().expect("detail");
    assert!(
        detail.contains("requested=true source_tier=hot_or_warm"),
        "detail: {detail}"
    );
    assert!(detail.contains("budget_chars="), "detail: {detail}");
    assert!(detail.contains("elapsed_ms="), "detail: {detail}");
    assert!(
        !detail.contains("turn one user"),
        "rehydrate trace must be body-free: {detail}"
    );
}

#[test]
fn retrieve_session_default_path_omits_rehydrate_trace() {
    const SESSION_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let vault = tempfile::tempdir().expect("vault");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap");
    let _ = ingest_reference(vault.path(), "seed default issuer before retrieve");
    let trace_path = write_issue_78_trace_fixture(vault.path(), SESSION_ID);
    run_capture_trace(vault.path(), &trace_path);

    let retrieve = cli()
        .current_dir(vault.path())
        .args(["retrieve", "--session", SESSION_ID, "--json"])
        .output()
        .expect("run retrieve session");
    assert_eq!(
        retrieve.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&retrieve.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&retrieve.stdout).expect("json");
    let trace = value["policy_trace"].as_array().expect("policy_trace");
    assert!(
        trace.iter().all(|entry| entry["gate"] != "read.rehydrate"),
        "default session retrieval must stay on the fast path: {trace:?}"
    );
}

#[test]
fn retrieve_session_returns_redacted_trace_without_raw_secret() {
    const SESSION_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let vault = tempfile::tempdir().expect("vault");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap");
    let _ = ingest_reference(vault.path(), "seed default issuer before retrieve");
    let trace_path = write_issue_78_trace_fixture(vault.path(), SESSION_ID);
    run_capture_trace(vault.path(), &trace_path);

    let retrieve = cli()
        .current_dir(vault.path())
        .args([
            "retrieve",
            "--session",
            SESSION_ID,
            "--order",
            "asc",
            "--json",
        ])
        .output()
        .expect("run retrieve session");
    assert_eq!(
        retrieve.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&retrieve.stderr)
    );
    let text = String::from_utf8(retrieve.stdout).expect("utf-8");
    assert!(
        !text.contains("alice@example.com"),
        "raw email leaked: {text}"
    );
    assert!(
        text.contains("[REDACTED:email]"),
        "redacted marker missing: {text}"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "single E2E test intentionally exercises the full issue-61 CLI workflow"
)]
fn issue_61_cli_full_workflow_round_trips_across_verbs() {
    const SESSION_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let vault = tempfile::tempdir().expect("vault");
    bootstrap(&BootstrapOpts {
        vault_path: vault.path().to_path_buf(),
        force: false,
    })
    .expect("bootstrap");

    let ingest = cli()
        .current_dir(vault.path())
        .args([
            "ingest",
            "--kind",
            "project",
            "--body",
            "full workflow issue 61 project memory",
            "--json",
        ])
        .output()
        .expect("run ingest");
    assert_eq!(
        ingest.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&ingest.stderr)
    );
    let ingest_json: serde_json::Value = serde_json::from_slice(&ingest.stdout).expect("json");
    assert_eq!(ingest_json["status"], "committed");
    assert!(
        !serde_json::to_string(&ingest_json["policy_trace"])
            .expect("policy trace json")
            .contains("full workflow issue 61 project memory"),
        "policy trace must not echo the ingested body"
    );
    let record_id = ingest_json["data"]["record_id"]
        .as_str()
        .expect("record_id")
        .to_owned();

    let retrieve = cli()
        .current_dir(vault.path())
        .args(["retrieve", &record_id, "--json"])
        .output()
        .expect("run retrieve");
    assert_eq!(
        retrieve.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&retrieve.stderr)
    );
    let resp: Response = serde_json::from_slice(&retrieve.stdout).expect("retrieve response");
    let Some(ResponseData::Retrieve(RetrieveData::Record(data))) = resp.data else {
        panic!("retrieve must return record data");
    };
    assert_eq!(
        data.body.as_deref(),
        Some("full workflow issue 61 project memory")
    );

    let summarize = cli()
        .current_dir(vault.path())
        .args(["summarize", &record_id, "--persist", "--json"])
        .output()
        .expect("run summarize");
    assert_eq!(
        summarize.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&summarize.stderr)
    );
    let resp: Response = serde_json::from_slice(&summarize.stdout).expect("summarize response");
    let Some(ResponseData::Summarize(data)) = resp.data else {
        panic!("summarize must return summary data");
    };
    let summary_id = data.persisted_record_id.expect("persisted summary id").0;
    assert_eq!(summary_id.len(), 26);
    assert!(
        data.digest
            .contains("full workflow issue 61 project memory")
    );

    enable_hook_sensor(vault.path());
    let trace_path = write_issue_61_trace_fixture(vault.path(), SESSION_ID);
    let capture = cli()
        .current_dir(vault.path())
        .args([
            "capture_trace",
            "--from",
            trace_path.to_str().expect("utf-8 trace path"),
            "--json",
        ])
        .output()
        .expect("run capture_trace");
    assert_eq!(
        capture.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&capture.stderr)
    );
    let resp: Response = serde_json::from_slice(&capture.stdout).expect("capture response");
    let Some(ResponseData::CaptureTrace(data)) = resp.data else {
        panic!("capture_trace must return trace data");
    };
    assert!(data.failed_turns.is_empty());

    let session = cli()
        .current_dir(vault.path())
        .args([
            "retrieve",
            "--session",
            SESSION_ID,
            "--limit",
            "3",
            "--order",
            "asc",
            "--json",
        ])
        .output()
        .expect("run retrieve session");
    assert_eq!(
        session.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&session.stderr)
    );
    let resp: Response = serde_json::from_slice(&session.stdout).expect("session response");
    let Some(ResponseData::Retrieve(RetrieveData::Session(data))) = resp.data else {
        panic!("retrieve session must return session data");
    };
    assert_eq!(data.items.len(), 3);
    assert_eq!(
        data.items[2].content.as_deref(),
        Some("third trace message")
    );

    let hot = cli()
        .current_dir(vault.path())
        .args(["assemble_hot", "--budget", "4096", "--json"])
        .output()
        .expect("run assemble_hot");
    assert_eq!(
        hot.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&hot.stderr)
    );
    let hot_json: serde_json::Value = serde_json::from_slice(&hot.stdout).expect("hot json");
    assert_eq!(hot_json["status"], "committed");
    let prefix = hot_json["data"]["prefix"]
        .as_str()
        .expect("hot prefix string");
    assert!(prefix.contains("full workflow issue 61 project memory"));
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
            jsonl_summary: None,
            recording_summary: None,
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
            jsonl_summary: None,
            recording_summary: None,
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

fn ingest_reference(vault: &Path, body: &str) -> String {
    let ingest = cli()
        .current_dir(vault)
        .args(["ingest", "--kind", "reference", "--body", body, "--json"])
        .output()
        .expect("run ingest");
    assert_eq!(
        ingest.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&ingest.stderr)
    );
    let ingest_json: serde_json::Value = serde_json::from_slice(&ingest.stdout).expect("json");
    ingest_json["data"]["record_id"]
        .as_str()
        .expect("record_id")
        .to_owned()
}

fn insert_session_record(vault: &Path, body: &str) -> String {
    let args = cairn_core::generated::verbs::ingest::IngestArgs {
        batch_size: None,
        body: Some(body.to_owned()),
        dry_run: None,
        exclude: None,
        file: None,
        folder: None,
        frontmatter: None,
        human_review: None,
        include: None,
        kind: "reference".to_owned(),
        mode: None,
        no_cache: None,
        no_diff: None,
        recursive: None,
        session_id: None,
        tags: None,
        url: None,
        jsonl: None,
        recording: None,
        harness: None,
        session_id_from: None,
        limit: None,
    };
    let prepared =
        cairn_core::verbs::ingest::prepare_ingest_body(&args, "agt:cairn-cli:default:writer:v1")
            .expect("prepare session record");
    let cairn_core::verbs::ingest::PreparedIngest::Proceed { record, .. } = prepared else {
        panic!("session fixture body should pass filters");
    };
    let mut record = *record;
    record.scope.tenant = Some("default".to_owned());
    record.scope.workspace = Some("my-vault".to_owned());
    record.scope.entity = Some("ingest".to_owned());
    record.scope.session_id = Some("01ARZ3NDEKTSV4RRFFQ69G5FAS".to_owned());
    record.visibility = MemoryVisibility::Session;
    let record_id = record.id.as_str().to_owned();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let store = cairn_store_sqlite::open(vault.join(".cairn/cairn.db"))
            .await
            .expect("open store");
        store.upsert(&record).await.expect("upsert session record");
    });
    record_id
}

fn provision_agent(vault: &Path, slug: &str) -> String {
    let provision = cli()
        .current_dir(vault)
        .args(["identity", "provision", "agent", slug, "--json"])
        .output()
        .expect("provision agent");
    assert_eq!(
        provision.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&provision.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&provision.stdout).expect("json");
    json["provisioned"]
        .as_str()
        .expect("provisioned identity")
        .to_owned()
}

fn run_capture_trace(vault: &Path, trace_path: &Path) {
    enable_hook_sensor(vault);
    let capture = cli()
        .current_dir(vault)
        .args([
            "capture_trace",
            "--from",
            trace_path.to_str().expect("utf-8 trace path"),
            "--json",
        ])
        .output()
        .expect("run capture_trace");
    assert_eq!(
        capture.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&capture.stderr)
    );
}

fn set_retrieve_budget(vault: &Path, chars: usize) {
    std::fs::write(
        vault.join(".cairn/config.yaml"),
        format!("search:\n  max_snippet_chars_per_page: {chars}\n"),
    )
    .expect("write retrieve budget config");
}

fn write_issue_78_trace_fixture(vault: &Path, session_id: &str) -> PathBuf {
    let events = [
        (
            "01ARZ3NDEKTSV4RRFFQ69G5FAD",
            "UserPromptSubmit",
            "turn-1",
            None,
            "turn one user",
            "2026-05-02T00:00:01Z",
        ),
        (
            "01ARZ3NDEKTSV4RRFFQ69G5FAE",
            "PreToolUse",
            "turn-1",
            Some("call-1"),
            "run cargo test",
            "2026-05-02T00:00:02Z",
        ),
        (
            "01ARZ3NDEKTSV4RRFFQ69G5FAF",
            "PostToolUse",
            "turn-1",
            Some("call-1"),
            "cargo test ok",
            "2026-05-02T00:00:03Z",
        ),
        (
            "01ARZ3NDEKTSV4RRFFQ69G5FAG",
            "UserPromptSubmit",
            "turn-2",
            None,
            "turn two user alice@example.com",
            "2026-05-02T00:00:04Z",
        ),
        (
            "01ARZ3NDEKTSV4RRFFQ69G5FAH",
            "UserPromptSubmit",
            "turn-3",
            None,
            "turn three user",
            "2026-05-02T00:00:05Z",
        ),
    ];
    let trace_path = vault.join("issue-78-trace.jsonl");
    let mut jsonl = std::fs::File::create(&trace_path).expect("create trace jsonl");
    for (event_id, hook_name, turn_id, tool_id, body, timestamp) in events {
        let payload_ref = write_trace_source(vault, event_id, body);
        let event = capture_trace_event_with_hook(
            event_id,
            session_id,
            turn_id,
            hook_name,
            tool_id,
            timestamp,
            &payload_ref,
            body,
        );
        writeln!(
            jsonl,
            "{}",
            serde_json::to_string(&event).expect("event json")
        )
        .expect("write trace event");
    }
    trace_path
}

fn write_issue_61_trace_fixture(vault: &Path, session_id: &str) -> PathBuf {
    let events = [
        (
            "01ARZ3NDEKTSV4RRFFQ69G5FAA",
            "first trace message",
            "2026-05-02T00:00:01Z",
        ),
        (
            "01ARZ3NDEKTSV4RRFFQ69G5FAB",
            "second trace message",
            "2026-05-02T00:00:02Z",
        ),
        (
            "01ARZ3NDEKTSV4RRFFQ69G5FAC",
            "third trace message",
            "2026-05-02T00:00:03Z",
        ),
    ];
    let trace_path = vault.join("issue-61-trace.jsonl");
    let mut jsonl = std::fs::File::create(&trace_path).expect("create trace jsonl");
    for (event_id, body, timestamp) in events {
        let payload_ref = write_trace_source(vault, event_id, body);
        let event = capture_trace_event(
            event_id,
            session_id,
            "turn-limit",
            timestamp,
            &payload_ref,
            body,
        );
        writeln!(
            jsonl,
            "{}",
            serde_json::to_string(&event).expect("event json")
        )
        .expect("write trace event");
    }
    trace_path
}

#[allow(clippy::too_many_arguments)]
fn capture_trace_event_with_hook(
    event_id: &str,
    session_id: &str,
    turn_id: &str,
    hook_name: &str,
    tool_id: Option<&str>,
    timestamp: &str,
    payload_ref: &str,
    body: &str,
) -> CaptureEvent {
    let sensor =
        Identity::parse("snr:local:hook:cc-session:v1").expect("invariant: valid sensor id");
    CaptureEvent {
        event_id: CaptureEventId::parse(event_id).expect("invariant: valid ULID"),
        sensor_id: sensor.clone(),
        capture_mode: CaptureMode::Auto,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: sensor,
            at: Rfc3339Timestamp::parse(timestamp).expect("invariant: valid RFC-3339"),
        }],
        refs: Some(CaptureRefs {
            session_id: Some(session_id.to_owned()),
            turn_id: Some(turn_id.to_owned()),
            tool_id: tool_id.map(str::to_owned),
        }),
        payload_hash: PayloadHash::parse(format!("sha256:{}", sha256_hex(body)))
            .expect("invariant: valid payload hash"),
        payload_ref: payload_ref.to_owned(),
        captured_at: Rfc3339Timestamp::parse(timestamp).expect("invariant: valid RFC-3339"),
        payload: CapturePayload::Hook {
            hook_name: hook_name.to_owned(),
            tool_name: None,
        },
        source_family: SourceFamily::Hook,
    }
}

fn write_trace_source(vault: &Path, event_id: &str, body: &str) -> String {
    let dir = vault.join("sources").join("hook");
    std::fs::create_dir_all(&dir).expect("create sources/hook");
    let filename = format!("{event_id}.txt");
    std::fs::write(dir.join(&filename), body).expect("write trace source");
    format!("sources/hook/{filename}")
}

fn capture_trace_event(
    event_id: &str,
    session_id: &str,
    turn_id: &str,
    timestamp: &str,
    payload_ref: &str,
    body: &str,
) -> CaptureEvent {
    let sensor =
        Identity::parse("snr:local:hook:cc-session:v1").expect("invariant: valid sensor id");
    CaptureEvent {
        event_id: CaptureEventId::parse(event_id).expect("invariant: valid ULID"),
        sensor_id: sensor.clone(),
        capture_mode: CaptureMode::Auto,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: sensor,
            at: Rfc3339Timestamp::parse(timestamp).expect("invariant: valid RFC-3339"),
        }],
        refs: Some(CaptureRefs {
            session_id: Some(session_id.to_owned()),
            turn_id: Some(turn_id.to_owned()),
            tool_id: None,
        }),
        payload_hash: PayloadHash::parse(format!("sha256:{}", sha256_hex(body)))
            .expect("invariant: valid payload hash"),
        payload_ref: payload_ref.to_owned(),
        captured_at: Rfc3339Timestamp::parse(timestamp).expect("invariant: valid RFC-3339"),
        payload: CapturePayload::Hook {
            hook_name: "UserPromptSubmit".to_owned(),
            tool_name: None,
        },
        source_family: SourceFamily::Hook,
    }
}

fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}
