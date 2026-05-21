//! Integration coverage for `cairn admin sre report`.

use std::{future::Future, process::Command};

use cairn_core::{
    contract::memory_store::{MemoryStore, ProjectionApplyItem},
    domain::{
        projection::{
            ProjectionCursor, ProjectionItemState, ProjectionLedgerRow, ProjectionTarget,
        },
        record::RecordId,
    },
};
use cairn_store_sqlite::SqliteMemoryStore;

fn cairn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn bootstrap_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let bootstrap = cairn()
        .args([
            "bootstrap",
            "--vault-path",
            dir.path().to_str().expect("utf8"),
        ])
        .output()
        .expect("bootstrap");
    assert!(
        bootstrap.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&bootstrap.stderr)
    );
    dir
}

fn assert_forbidden_fragments_absent(output: &str) {
    for fragment in [
        "SECRET_PRIVATE_TOKEN",
        "/Users/alice",
        "private body",
        "query text",
    ] {
        assert!(
            !output.contains(fragment),
            "output leaked forbidden fragment {fragment:?}: {output}"
        );
    }
}

fn json_stdout(output: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).expect("json stdout")
}

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("test runtime")
        .block_on(future)
}

fn now_epoch_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_millis(),
    )
    .expect("epoch millis fit i64")
}

fn record_id(raw: &str) -> RecordId {
    RecordId::parse(raw).expect("valid test ULID")
}

fn create_workflow_jobs_table(conn: &rusqlite::Connection) {
    conn.execute_batch(
        "CREATE TABLE workflow_jobs (
            job_id TEXT NOT NULL PRIMARY KEY,
            kind TEXT NOT NULL,
            payload BLOB NOT NULL,
            state TEXT NOT NULL,
            attempts INTEGER NOT NULL,
            delivery_count INTEGER NOT NULL,
            max_attempts INTEGER NOT NULL,
            base_backoff_ms INTEGER NOT NULL,
            backoff_multiplier INTEGER NOT NULL,
            max_backoff_ms INTEGER NOT NULL,
            next_run_at INTEGER NOT NULL,
            enqueued_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            lease_owner TEXT,
            lease_nonce TEXT,
            lease_started INTEGER,
            lease_expires_at INTEGER,
            failure_class TEXT,
            dead_letter_at_ms INTEGER,
            completed_at_ms INTEGER,
            last_error TEXT
        );",
    )
    .expect("create workflow_jobs table");
}

fn insert_workflow_row(
    conn: &rusqlite::Connection,
    job_id: &str,
    kind: &str,
    state: &str,
    next_run_at: i64,
    extras: &[(&str, &dyn rusqlite::ToSql)],
) {
    let payload: Vec<u8> = Vec::new();
    let mut row: Vec<(&str, &dyn rusqlite::ToSql)> = vec![
        ("job_id", &job_id),
        ("kind", &kind),
        ("payload", &payload),
        ("state", &state),
        ("attempts", &0_i64),
        ("delivery_count", &0_i64),
        ("max_attempts", &3_i64),
        ("base_backoff_ms", &1_i64),
        ("backoff_multiplier", &2_i64),
        ("max_backoff_ms", &60_000_i64),
        ("next_run_at", &next_run_at),
        ("enqueued_at", &0_i64),
        ("updated_at", &0_i64),
    ];
    for (name, value) in extras {
        if let Some(slot) = row.iter_mut().find(|(column, _)| column == name) {
            slot.1 = *value;
        } else {
            row.push((name, *value));
        }
    }
    let columns: Vec<&str> = row.iter().map(|(name, _)| *name).collect();
    let placeholders: Vec<String> = (1..=columns.len()).map(|idx| format!("?{idx}")).collect();
    let sql = format!(
        "INSERT INTO workflow_jobs ({}) VALUES ({})",
        columns.join(", "),
        placeholders.join(", ")
    );
    let params: Vec<&dyn rusqlite::ToSql> = row.iter().map(|(_, value)| *value).collect();
    conn.execute(&sql, rusqlite::params_from_iter(params))
        .expect("insert workflow row");
}

fn seed_current_workflow_state(conn: &rusqlite::Connection, now_ms: i64) {
    insert_workflow_row(
        conn,
        "queued-tier",
        "expire.tier",
        "queued",
        now_ms - 742_000,
        &[],
    );
    insert_workflow_row(
        conn,
        "leased-dream",
        "dream.light",
        "leased",
        now_ms,
        &[
            ("lease_owner", &"worker"),
            ("lease_nonce", &"nonce"),
            ("lease_started", &1_i64),
            ("lease_expires_at", &(now_ms - 90_000)),
        ],
    );
    insert_workflow_row(
        conn,
        "done-dream",
        "dream.light",
        "done",
        now_ms,
        &[("completed_at_ms", &(now_ms - 2_500))],
    );
    insert_workflow_row(
        conn,
        "dead-tier",
        "expire.tier",
        "failed",
        now_ms,
        &[
            ("attempts", &3_i64),
            ("delivery_count", &3_i64),
            ("failure_class", &"provider"),
            ("dead_letter_at_ms", &(now_ms - 1_000)),
            (
                "last_error",
                &"SECRET_PRIVATE_TOKEN /Users/alice private body query text",
            ),
        ],
    );
}

fn seed_projection_ledger_state(vault: &std::path::Path) {
    let store = SqliteMemoryStore::open(&vault.join(".cairn/cairn.db")).expect("open sqlite");
    for (record_id, body, sequence, hash) in [
        (
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "current projection",
            1,
            "sha256:record-current",
        ),
        (
            "01BRZ3NDEKTSV4RRFFQ69G5FAV",
            "stale projection",
            2,
            "sha256:record-stale-new",
        ),
        (
            "01CRZ3NDEKTSV4RRFFQ69G5FAV",
            "missing projection",
            3,
            "sha256:record-missing",
        ),
        (
            "01DRZ3NDEKTSV4RRFFQ69G5FAV",
            "failed projection",
            4,
            "sha256:record-failed",
        ),
    ] {
        store
            .insert_test_record(record_id, body, sequence, hash)
            .expect("insert projection test record");
    }
    block_on(store.apply_projection_items(vec![
        ProjectionApplyItem {
            row: ProjectionLedgerRow {
                target: ProjectionTarget::Bm25sLexical,
                cursor: ProjectionCursor {
                    record_id: record_id("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
                    wal_sequence: 1,
                    record_hash: "sha256:record-current".to_owned(),
                    source_hash: None,
                },
                state: ProjectionItemState::Current,
                updated_at: "2026-05-19T12:00:00Z".to_owned(),
            },
        },
        ProjectionApplyItem {
            row: ProjectionLedgerRow {
                target: ProjectionTarget::Bm25sLexical,
                cursor: ProjectionCursor {
                    record_id: record_id("01BRZ3NDEKTSV4RRFFQ69G5FAV"),
                    wal_sequence: 1,
                    record_hash: "sha256:record-stale-old".to_owned(),
                    source_hash: None,
                },
                state: ProjectionItemState::Current,
                updated_at: "2026-05-19T12:00:00Z".to_owned(),
            },
        },
        ProjectionApplyItem {
            row: ProjectionLedgerRow {
                target: ProjectionTarget::Bm25sLexical,
                cursor: ProjectionCursor {
                    record_id: record_id("01DRZ3NDEKTSV4RRFFQ69G5FAV"),
                    wal_sequence: 4,
                    record_hash: "sha256:record-failed".to_owned(),
                    source_hash: None,
                },
                state: ProjectionItemState::Failed {
                    reason: "projection failed".to_owned(),
                },
                updated_at: "2026-05-19T12:00:00Z".to_owned(),
            },
        },
    ]))
    .expect("apply projection rows");
}

#[test]
fn admin_sre_report_json_is_body_free() {
    let dir = bootstrap_vault();

    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"rehydration_completed","ts_ms":1,"target":"session","source_tier":"cold","restored_tier":"warm","status":"committed","latency_ms":2100,"bytes_restored":1000,"record_count":2,"error":null,"raw_query":"query text","source_path":"/Users/alice/private body"}
{"event":"search_completed","ts_ms":2,"mode":"semantic","hit_count":3,"latency_ms":42,"degradation_state":"partial","error":"SECRET_PRIVATE_TOKEN"}
"#,
    )
    .expect("write metrics");

    let output = cairn()
        .current_dir(dir.path())
        .args(["admin", "sre", "report", "--json"])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"rehydration\""));
    assert!(stdout.contains("\"sample_count\":1"));
    assert!(stdout.contains("\"mode\":\"semantic\""));
    assert_forbidden_fragments_absent(&stdout);
}

#[test]
fn admin_sre_report_hashes_vault_ids_without_leaking_raw_identity() {
    let first = bootstrap_vault();
    let second = bootstrap_vault();
    let first_id = std::fs::read_to_string(first.path().join(".cairn/vault.id"))
        .expect("first vault id")
        .trim()
        .to_owned();
    let second_id = std::fs::read_to_string(second.path().join(".cairn/vault.id"))
        .expect("second vault id")
        .trim()
        .to_owned();
    assert_ne!(first_id, second_id, "bootstrap should create distinct ids");

    let run_report = |dir: &tempfile::TempDir| {
        cairn()
            .current_dir(dir.path())
            .args(["admin", "sre", "report", "--json"])
            .output()
            .expect("run sre report")
    };
    let first_output = run_report(&first);
    let second_output = run_report(&second);
    assert!(
        first_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first_output.stderr)
    );
    assert!(
        second_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&second_output.stderr)
    );

    let first_stdout = String::from_utf8_lossy(&first_output.stdout);
    let second_stdout = String::from_utf8_lossy(&second_output.stdout);
    let first_json = json_stdout(&first_output);
    let second_json = json_stdout(&second_output);
    let first_hash = first_json["vault"]["id_hash"].as_str().expect("first hash");
    let second_hash = second_json["vault"]["id_hash"]
        .as_str()
        .expect("second hash");
    assert_ne!(first_hash, second_hash);
    for hash in [first_hash, second_hash] {
        assert!(hash.starts_with("sha256:"), "hash: {hash}");
        assert_eq!(hash.len(), "sha256:".len() + 64, "hash: {hash}");
    }
    assert_eq!(first_json["vault"]["name"], "local_vault");
    assert_eq!(second_json["vault"]["name"], "local_vault");
    for (stdout, raw_id, dir) in [
        (&first_stdout, first_id.as_str(), first.path()),
        (&second_stdout, second_id.as_str(), second.path()),
    ] {
        assert!(!stdout.contains(raw_id), "stdout leaked vault id: {stdout}");
        assert!(
            !stdout.contains(dir.to_string_lossy().as_ref()),
            "stdout leaked vault path: {stdout}"
        );
        if let Some(name) = dir.file_name().and_then(|name| name.to_str()) {
            assert!(
                !stdout.contains(name),
                "stdout leaked vault directory name: {stdout}"
            );
        }
    }
}

#[test]
fn admin_sre_report_surfaces_metric_parse_errors_safely() {
    let dir = bootstrap_vault();
    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"rehydration_completed","ts_ms":1,"target":"session","source_tier":"cold","restored_tier":"warm","status":"committed","latency_ms":2100,"bytes_restored":1000,"record_count":2,"error":null}
not json SECRET_PRIVATE_TOKEN /Users/alice private body query text
"#,
    )
    .expect("write metrics");

    let output = cairn()
        .current_dir(dir.path())
        .args(["admin", "sre", "report", "--json"])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json = json_stdout(&output);
    let gates = json["gates"]["gates"].as_array().expect("gates");
    let parse_gate = gates
        .iter()
        .find(|gate| gate["name"] == "metric_parse_errors")
        .expect("metric_parse_errors gate");
    assert_eq!(parse_gate["status"], "warning");
    assert_eq!(parse_gate["measured"], 1.0);
    assert_forbidden_fragments_absent(&stdout);
}

#[test]
fn admin_sre_report_counts_search_verb_invocation_failures() {
    let dir = bootstrap_vault();
    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"verb_invocation","ts_ms":1,"verb":"search","surface":"mcp","mode":"semantic","status":"rejected","latency_ms":77,"error":"provider_unavailable","budget_used_ratio":null,"degradation_state":"partial","private":"SECRET_PRIVATE_TOKEN /Users/alice private body query text"}
"#,
    )
    .expect("write metrics");

    let output = cairn()
        .current_dir(dir.path())
        .args(["admin", "sre", "report", "--json"])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json = json_stdout(&output);
    let modes = json["search"]["modes"].as_array().expect("search modes");
    let semantic = modes
        .iter()
        .find(|mode| mode["mode"] == "semantic")
        .expect("semantic mode");
    assert_eq!(semantic["invocations"], 1);
    assert_eq!(semantic["failed"], 1);
    assert_eq!(semantic["degraded"], 1);
    assert_eq!(semantic["status"], "fail");
    assert_forbidden_fragments_absent(&stdout);
}

#[test]
fn admin_sre_report_counts_successful_mcp_search_invocations() {
    let dir = bootstrap_vault();
    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"verb_invocation","ts_ms":1,"verb":"search","surface":"mcp","mode":"semantic","status":"committed","latency_ms":77,"error":null,"budget_used_ratio":null,"degradation_state":"none"}
"#,
    )
    .expect("write metrics");

    let output = cairn()
        .current_dir(dir.path())
        .args(["admin", "sre", "report", "--json"])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = json_stdout(&output);
    let modes = json["search"]["modes"].as_array().expect("search modes");
    let semantic = modes
        .iter()
        .find(|mode| mode["mode"] == "semantic")
        .expect("semantic mode");
    assert_eq!(semantic["invocations"], 1);
    assert_eq!(semantic["failed"], 0);
    assert_eq!(semantic["degraded"], 0);
    assert_eq!(semantic["p95_latency_ms"], 77.0);
    assert_eq!(semantic["status"], "ok");
}

#[test]
fn admin_sre_report_does_not_double_count_completed_cli_search() {
    let dir = bootstrap_vault();
    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"search_completed","ts_ms":1,"mode":"keyword","hit_count":1,"latency_ms":41,"degradation_state":"none","error":null}
{"event":"verb_invocation","ts_ms":1,"verb":"search","surface":"cli","mode":"keyword","status":"committed","latency_ms":41,"error":null,"budget_used_ratio":null,"degradation_state":"none"}
"#,
    )
    .expect("write metrics");

    let output = cairn()
        .current_dir(dir.path())
        .args(["admin", "sre", "report", "--json"])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = json_stdout(&output);
    let modes = json["search"]["modes"].as_array().expect("search modes");
    let keyword = modes
        .iter()
        .find(|mode| mode["mode"] == "keyword")
        .expect("keyword mode");
    assert_eq!(keyword["invocations"], 1);
}

#[test]
fn admin_sre_report_does_not_double_count_failed_cli_search() {
    let dir = bootstrap_vault();
    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"search_completed","ts_ms":1000,"mode":"semantic","hit_count":0,"latency_ms":41,"degradation_state":"failed","error":"provider_unavailable"}
{"event":"verb_invocation","ts_ms":1200,"verb":"search","surface":"cli","mode":"semantic","status":"aborted","latency_ms":55,"error":"provider_unavailable","budget_used_ratio":null,"degradation_state":"failed"}
"#,
    )
    .expect("write metrics");

    let output = cairn()
        .current_dir(dir.path())
        .args(["admin", "sre", "report", "--json"])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = json_stdout(&output);
    let modes = json["search"]["modes"].as_array().expect("search modes");
    let semantic = modes
        .iter()
        .find(|mode| mode["mode"] == "semantic")
        .expect("semantic mode");
    assert_eq!(semantic["invocations"], 1);
    assert_eq!(semantic["failed"], 1);
    assert_eq!(semantic["degraded"], 1);
    assert_eq!(semantic["status"], "fail");
}

#[test]
fn admin_sre_report_counts_standalone_cli_search_preflight_failures() {
    let dir = bootstrap_vault();
    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"search_completed","ts_ms":1000,"mode":"semantic","hit_count":0,"latency_ms":41,"degradation_state":"failed","error":"provider_unavailable"}
{"event":"verb_invocation","ts_ms":4500,"verb":"search","surface":"cli","mode":"semantic","status":"rejected","latency_ms":12,"error":"capability_unavailable","budget_used_ratio":null,"degradation_state":"partial"}
"#,
    )
    .expect("write metrics");

    let output = cairn()
        .current_dir(dir.path())
        .args(["admin", "sre", "report", "--json"])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = json_stdout(&output);
    let modes = json["search"]["modes"].as_array().expect("search modes");
    let semantic = modes
        .iter()
        .find(|mode| mode["mode"] == "semantic")
        .expect("semantic mode");
    assert_eq!(semantic["invocations"], 2);
    assert_eq!(semantic["failed"], 2);
    assert_eq!(semantic["degraded"], 2);
    assert_eq!(semantic["status"], "fail");
}

#[test]
fn admin_sre_report_tolerates_unknown_metric_events() {
    let dir = bootstrap_vault();
    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"future_metric","private":"SECRET_PRIVATE_TOKEN /Users/alice private body query text"}
{"event":"rehydration_completed","ts_ms":1,"target":"session","source_tier":"cold","restored_tier":"warm","status":"committed","latency_ms":2100,"bytes_restored":1000,"record_count":2,"error":null}
"#,
    )
    .expect("write metrics");

    let output = cairn()
        .current_dir(dir.path())
        .args(["admin", "sre", "report", "--json"])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json = json_stdout(&output);
    let gates = json["gates"]["gates"].as_array().expect("gates");
    assert!(
        gates
            .iter()
            .all(|gate| gate["name"] != "metric_parse_errors"),
        "json: {json}"
    );
    assert_forbidden_fragments_absent(&stdout);
}

#[test]
fn admin_sre_report_marks_unobserved_search_unknown() {
    let dir = bootstrap_vault();

    let output = cairn()
        .current_dir(dir.path())
        .args(["admin", "sre", "report", "--json"])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = json_stdout(&output);
    assert_eq!(json["search"]["status"], "unknown");
    assert!(
        json["search"]["modes"]
            .as_array()
            .expect("search modes")
            .iter()
            .any(|mode| mode["invocations"] == 0 && mode["status"] == "unknown"),
        "json: {json}"
    );
}

#[test]
fn admin_sre_report_marks_unadvertised_search_modes() {
    let dir = bootstrap_vault();
    let config = dir.path().join(".cairn/config.yaml");
    let raw = std::fs::read_to_string(&config).expect("read config");
    assert!(
        raw.contains("local_embeddings: true"),
        "bootstrap config: {raw}"
    );
    std::fs::write(
        &config,
        raw.replace("local_embeddings: true", "local_embeddings: false"),
    )
    .expect("write config");

    let output = cairn()
        .current_dir(dir.path())
        .args(["admin", "sre", "report", "--json"])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = json_stdout(&output);
    let modes = json["search"]["modes"].as_array().expect("search modes");
    let advertised = |name: &str| {
        modes
            .iter()
            .find(|mode| mode["mode"] == name)
            .expect("mode")["advertised"]
            .as_bool()
            .expect("advertised bool")
    };
    assert!(advertised("keyword"));
    assert!(!advertised("semantic"));
    assert!(!advertised("hybrid"));
}

#[test]
fn admin_sre_report_summarizes_workflow_metrics_safely() {
    let dir = bootstrap_vault();
    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"workflow_job_started","ts_ms":1000,"job_id":"job-SECRET_PRIVATE_TOKEN","kind":"dream.light","attempts":1,"queue_lag_ms":120,"dedupe_key":"/Users/alice private body query text"}
{"event":"workflow_job_completed","ts_ms":2000,"job_id":"job-SECRET_PRIVATE_TOKEN","kind":"dream.light","attempts":1,"duration_ms":40}
{"event":"workflow_job_started","ts_ms":2500,"job_id":"job-tier","kind":"expire.tier","attempts":1,"queue_lag_ms":742000,"dedupe_key":null}
{"event":"workflow_job_failed","ts_ms":3000,"job_id":"job-SECRET_PRIVATE_TOKEN","kind":"SECRET_PRIVATE_TOKEN private body query text","attempts":2,"disposition":"permanent","failure_class":"provider_error","last_error":"/Users/alice private body query text","will_retry_at_ms":null}
"#,
    )
    .expect("write metrics");

    let output = cairn()
        .current_dir(dir.path())
        .args(["admin", "sre", "report", "--json"])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json = json_stdout(&output);
    assert_eq!(json["workflow"]["status"], "warning");
    assert_eq!(json["workflow"]["oldest_queued_age_ms"], 742_000);
    assert_eq!(json["workflow"]["dead_letter_count"], 1);
    let kinds = json["workflow"]["kinds"]
        .as_array()
        .expect("workflow kinds");
    let dream = kinds
        .iter()
        .find(|kind| kind["kind"] == "dream.light")
        .expect("dream.light kind");
    assert_eq!(dream["leased"], 0);
    assert_eq!(dream["done_recent"], 1);
    assert!(dream["oldest_queued_age_ms"].is_null());
    let expire = kinds
        .iter()
        .find(|kind| kind["kind"] == "expire.tier")
        .expect("expire.tier kind");
    assert_eq!(expire["leased"], 1);
    assert_eq!(expire["oldest_queued_age_ms"], 742_000);
    assert_eq!(expire["status"], "warning");
    let redacted = kinds
        .iter()
        .find(|kind| kind["kind"] == "redacted_workflow")
        .expect("redacted workflow kind");
    assert_eq!(redacted["failed_recent"], 1);
    assert_eq!(redacted["status"], "warning");
    assert_forbidden_fragments_absent(&stdout);
}

#[test]
fn admin_sre_report_prefers_db_workflow_current_state() {
    let dir = bootstrap_vault();
    let now_ms = now_epoch_ms();
    let conn =
        rusqlite::Connection::open(dir.path().join(".cairn/cairn.db")).expect("open cairn db");
    create_workflow_jobs_table(&conn);
    seed_current_workflow_state(&conn, now_ms);
    drop(conn);
    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"workflow_job_started","ts_ms":1000,"job_id":"metric-job","kind":"dream.light","attempts":1,"queue_lag_ms":999999,"dedupe_key":null}
{"event":"workflow_job_completed","ts_ms":2000,"job_id":"metric-job","kind":"dream.light","attempts":1,"duration_ms":40}
"#,
    )
    .expect("write metrics");

    let output = cairn()
        .current_dir(dir.path())
        .args(["admin", "sre", "report", "--json"])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json = json_stdout(&output);
    assert_eq!(json["workflow"]["status"], "warning");
    assert_eq!(json["workflow"]["dead_letter_count"], 1);
    assert!(
        json["workflow"]["longest_held_lease_ms"]
            .as_i64()
            .expect("longest held lease")
            >= 90_000
    );
    assert!(
        json["workflow"]["oldest_queued_age_ms"]
            .as_i64()
            .expect("oldest queued age")
            >= 742_000
    );
    let kinds = json["workflow"]["kinds"]
        .as_array()
        .expect("workflow kinds");
    let dream = kinds
        .iter()
        .find(|kind| kind["kind"] == "dream.light")
        .expect("dream.light kind");
    assert_eq!(dream["leased"], 1);
    assert_eq!(dream["queued"], 0);
    assert_eq!(dream["done_recent"], 1);
    assert!(
        dream["last_success_age_ms"]
            .as_i64()
            .expect("last success age")
            >= 2_500
    );
    let expire = kinds
        .iter()
        .find(|kind| kind["kind"] == "expire.tier")
        .expect("expire.tier kind");
    assert_eq!(expire["queued"], 1);
    assert_eq!(expire["leased"], 0);
    assert_eq!(expire["failed_recent"], 0);
    assert_eq!(expire["status"], "warning");
    assert_forbidden_fragments_absent(&stdout);
}

#[test]
fn admin_sre_report_fails_safely_when_workflow_db_unavailable() {
    let parent = tempfile::tempdir().expect("parent dir");
    let vault = parent
        .path()
        .join("SECRET_PRIVATE_TOKEN private body query text");
    std::fs::create_dir(&vault).expect("create vault dir");
    let bootstrap = cairn()
        .args(["bootstrap", "--vault-path", vault.to_str().expect("utf8")])
        .output()
        .expect("bootstrap");
    assert!(
        bootstrap.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&bootstrap.stderr)
    );
    std::fs::write(vault.join(".cairn/cairn.db"), b"not a sqlite database")
        .expect("write corrupt db");

    let output = cairn()
        .current_dir(&vault)
        .args(["admin", "sre", "report", "--json"])
        .output()
        .expect("run sre report");

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(69));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("workflow state unavailable"),
        "stderr: {stderr}"
    );
    assert_forbidden_fragments_absent(&stderr);
    assert!(
        !stderr.contains(vault.to_string_lossy().as_ref()),
        "stderr leaked path: {stderr}"
    );
}

#[test]
fn admin_sre_try_build_report_returns_workflow_db_error() {
    let dir = bootstrap_vault();
    std::fs::write(dir.path().join(".cairn/cairn.db"), b"not a sqlite database")
        .expect("write corrupt db");
    let config = cairn_cli::config::load(dir.path(), &cairn_cli::config::CliOverrides::default())
        .expect("load config");

    let err = cairn_cli::sre::try_build_report(dir.path(), &config)
        .expect_err("corrupt workflow db should be reported");

    assert_eq!(
        err,
        cairn_cli::sre::SreReportBuildError::WorkflowStateUnavailable
    );
}

#[test]
fn admin_sre_report_summarizes_projection_metrics_safely() {
    let dir = bootstrap_vault();
    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"projection_rebuild","ts_ms":1000,"projection":"sqlite.from_db","status":"committed","latency_ms":88,"records_rebuilt":7,"queue_lag_ms":40,"retry_count":0,"error":null,"degradation_state":"none"}
{"event":"projection_rebuild","ts_ms":2000,"projection":"SECRET_PRIVATE_TOKEN private body query text","status":"aborted","latency_ms":99,"records_rebuilt":0,"queue_lag_ms":55,"retry_count":1,"error":"/Users/alice private body query text","degradation_state":"partial"}
"#,
    )
    .expect("write metrics");

    let output = cairn()
        .current_dir(dir.path())
        .args(["admin", "sre", "report", "--json"])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json = json_stdout(&output);
    assert_eq!(json["projection"]["status"], "warning");
    assert_eq!(json["projection"]["nexus_state"], "degraded");
    assert_eq!(
        json["projection"]["nexus_reason"],
        "projection_rebuild_warning"
    );
    let targets = json["projection"]["targets"]
        .as_array()
        .expect("projection targets");
    let sqlite = targets
        .iter()
        .find(|target| target["target"] == "sqlite.from_db")
        .expect("sqlite target");
    assert_eq!(sqlite["current"], 7);
    assert_eq!(sqlite["failed"], 0);
    assert_eq!(sqlite["max_lag_ms"], 40);
    assert_eq!(sqlite["last_rebuild_latency_ms"], 88);
    let redacted = targets
        .iter()
        .find(|target| target["target"] == "redacted_projection")
        .expect("redacted projection target");
    assert_eq!(redacted["failed"], 1);
    assert_eq!(redacted["max_lag_ms"], 55);
    assert_eq!(redacted["last_rebuild_latency_ms"], 99);
    assert_eq!(redacted["status"], "warning");
    assert_forbidden_fragments_absent(&stdout);
}

#[test]
fn admin_sre_report_reads_projection_ledger_state() {
    let dir = bootstrap_vault();
    seed_projection_ledger_state(dir.path());

    let output = cairn()
        .current_dir(dir.path())
        .args(["admin", "sre", "report", "--json"])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = json_stdout(&output);
    let targets = json["projection"]["targets"]
        .as_array()
        .expect("projection targets");
    let bm25s = targets
        .iter()
        .find(|target| target["target"] == "bm25s_lexical")
        .expect("bm25s target");
    assert_eq!(bm25s["current"], 1);
    assert_eq!(bm25s["stale"], 1);
    assert_eq!(bm25s["missing"], 1);
    assert_eq!(bm25s["failed"], 1);
    assert_eq!(bm25s["status"], "warning");
    assert_eq!(json["projection"]["status"], "warning");
    assert_eq!(json["projection"]["nexus_state"], "degraded");
}

#[test]
fn admin_sre_report_human_summarizes_sections() {
    let dir = bootstrap_vault();
    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"search_completed","ts_ms":2,"mode":"semantic","hit_count":0,"latency_ms":42,"degradation_state":"none","error":"SECRET_PRIVATE_TOKEN /Users/alice private body query text"}
"#,
    )
    .expect("write metrics");

    let output = cairn()
        .current_dir(dir.path())
        .args(["admin", "sre", "report"])
        .output()
        .expect("run sre report");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("SRE status:"));
    assert!(stdout.contains("workflow:"));
    assert!(stdout.contains("rehydration:"));
    assert!(stdout.contains("projection:"));
    assert!(stdout.contains("search:"));
    assert!(stdout.contains("gates:"));
    assert_forbidden_fragments_absent(&stdout);
}

#[test]
fn admin_sre_report_human_reports_warning_when_degraded_search_present() {
    let dir = bootstrap_vault();
    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"search_completed","ts_ms":2,"mode":"semantic","hit_count":0,"latency_ms":42,"degradation_state":"partial","error":null}
"#,
    )
    .expect("write metrics");

    let output = cairn()
        .current_dir(dir.path())
        .args(["admin", "sre", "report"])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("SRE status: warning"), "stdout: {stdout}");
}

#[test]
fn admin_sre_report_search_warns_when_observed_mode_degrades() {
    let dir = bootstrap_vault();
    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"search_completed","ts_ms":2,"mode":"semantic","hit_count":0,"latency_ms":42,"degradation_state":"partial","error":null}
"#,
    )
    .expect("write metrics");

    let output = cairn()
        .current_dir(dir.path())
        .args(["admin", "sre", "report", "--json"])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = json_stdout(&output);
    assert_eq!(json["search"]["status"], "warning");
}

#[test]
fn admin_sre_report_human_reports_unknown_when_sections_unknown() {
    let dir = bootstrap_vault();

    let output = cairn()
        .current_dir(dir.path())
        .args(["admin", "sre", "report"])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("SRE status: unknown"), "stdout: {stdout}");
}

#[test]
fn admin_sre_report_rejects_bad_bench_report_dir() {
    let dir = bootstrap_vault();
    let missing = dir.path().join("missing-bench-reports");

    let output = cairn()
        .current_dir(dir.path())
        .args([
            "admin",
            "sre",
            "report",
            "--bench-report-dir",
            missing.to_str().expect("utf8"),
        ])
        .output()
        .expect("run sre report");

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(78));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("bench-report-dir"), "stderr: {stderr}");
}

#[test]
fn admin_sre_report_bad_bench_dir_error_is_path_free() {
    let dir = bootstrap_vault();
    let missing = dir
        .path()
        .join("SECRET_PRIVATE_TOKEN private body query text");

    let output = cairn()
        .current_dir(dir.path())
        .args([
            "admin",
            "sre",
            "report",
            "--bench-report-dir",
            missing.to_str().expect("utf8"),
        ])
        .output()
        .expect("run sre report");

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(78));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("bench-report-dir"), "stderr: {stderr}");
    assert_forbidden_fragments_absent(&stderr);
}

#[test]
fn admin_sre_report_unbound_vault_error_is_path_free() {
    let parent = tempfile::tempdir().expect("parent dir");
    let vault = parent
        .path()
        .join("SECRET_PRIVATE_TOKEN private body query text");
    std::fs::create_dir(&vault).expect("create vault dir");

    let output = cairn()
        .args([
            "--vault",
            vault.to_str().expect("utf8"),
            "admin",
            "sre",
            "report",
        ])
        .output()
        .expect("run sre report");

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(78));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cairn admin sre report:"),
        "stderr: {stderr}"
    );
    assert_forbidden_fragments_absent(&stderr);
}

#[test]
fn admin_sre_report_vault_resolution_error_is_path_free() {
    let dir = tempfile::tempdir().expect("tempdir");

    let output = cairn()
        .current_dir(dir.path())
        .args([
            "--vault",
            "SECRET_PRIVATE_TOKEN private body query text",
            "admin",
            "sre",
            "report",
        ])
        .output()
        .expect("run sre report");

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(78));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("vault resolution error") || stderr.contains("cairn admin sre report"));
    assert_forbidden_fragments_absent(&stderr);
}

#[test]
fn admin_sre_report_uses_bench_sre_gates() {
    let dir = bootstrap_vault();
    let bench = tempfile::tempdir().expect("bench dir");
    std::fs::write(
        bench.path().join("sre.json"),
        r#"{"checks":[{"name":"migration_backlog","status":"fail","measured":742000,"threshold":600000,"unit":"ms","detail":"fixture"}],"private":"/Users/alice SECRET_PRIVATE_TOKEN private body query text"}"#,
    )
    .expect("write bench sre report");

    let output = cairn()
        .current_dir(dir.path())
        .args([
            "admin",
            "sre",
            "report",
            "--json",
            "--bench-report-dir",
            bench.path().to_str().expect("utf8"),
        ])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"migration_backlog\""));
    assert!(stdout.contains("\"status\":\"fail\""));
    assert_forbidden_fragments_absent(&stdout);
}

#[test]
fn admin_sre_report_keeps_projection_lag_fixture_gate_name() {
    let dir = bootstrap_vault();
    let bench = tempfile::tempdir().expect("bench dir");
    std::fs::write(
        bench.path().join("sre.json"),
        r#"{"checks":[{"name":"projection_lag_fixture","status":"warning","measured":2,"threshold":0,"unit":"count","detail":"fixture"}]}"#,
    )
    .expect("write bench sre report");

    let output = cairn()
        .current_dir(dir.path())
        .args([
            "admin",
            "sre",
            "report",
            "--json",
            "--bench-report-dir",
            bench.path().to_str().expect("utf8"),
        ])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = json_stdout(&output);
    let gates = json["gates"]["gates"].as_array().expect("gates");
    let projection_gate = gates
        .iter()
        .find(|gate| gate["name"] == "projection_lag_fixture")
        .expect("projection lag gate");
    assert_eq!(projection_gate["status"], "warning");
}

#[test]
fn admin_sre_report_scrubs_stable_looking_untrusted_labels() {
    let parent = tempfile::tempdir().expect("parent dir");
    let vault = parent.path().join("payroll-vault");
    std::fs::create_dir(&vault).expect("create vault dir");
    let bootstrap = cairn()
        .args(["bootstrap", "--vault-path", vault.to_str().expect("utf8")])
        .output()
        .expect("bootstrap");
    assert!(
        bootstrap.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&bootstrap.stderr)
    );
    let bench = tempfile::tempdir().expect("bench dir");
    std::fs::write(
        bench.path().join("sre.json"),
        r#"{"checks":[{"name":"customer_acme_board","status":"fail","measured":1,"threshold":0,"unit":"session_01JABC","detail":"fixture"}]}"#,
    )
    .expect("write bench sre report");

    let output = cairn()
        .current_dir(&vault)
        .args([
            "admin",
            "sre",
            "report",
            "--json",
            "--bench-report-dir",
            bench.path().to_str().expect("utf8"),
        ])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"name\":\"local_vault\""),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("\"name\":\"redacted_gate\""),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("\"unit\":\"redacted\""), "stdout: {stdout}");
    assert!(!stdout.contains("payroll-vault"), "stdout: {stdout}");
    assert!(!stdout.contains("customer_acme_board"), "stdout: {stdout}");
    assert!(!stdout.contains("session_01JABC"), "stdout: {stdout}");
}

#[test]
fn admin_sre_report_redacts_stable_looking_imported_gate_detail() {
    let dir = bootstrap_vault();
    let bench = tempfile::tempdir().expect("bench dir");
    std::fs::write(
        bench.path().join("sre.json"),
        r#"{"checks":[{"name":"migration_backlog","status":"fail","measured":1,"threshold":0,"unit":"ms","detail":"customer_acme_board"}]}"#,
    )
    .expect("write bench sre report");

    let output = cairn()
        .current_dir(dir.path())
        .args([
            "admin",
            "sre",
            "report",
            "--json",
            "--bench-report-dir",
            bench.path().to_str().expect("utf8"),
        ])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("customer_acme_board"), "stdout: {stdout}");
    assert!(
        stdout.contains("\"detail\":\"redacted\""),
        "stdout: {stdout}"
    );
}

#[test]
fn admin_sre_report_rejects_malformed_bench_sre_json() {
    let dir = bootstrap_vault();
    let bench = tempfile::tempdir().expect("bench dir");
    std::fs::write(bench.path().join("sre.json"), r#"{"checks":["#)
        .expect("write malformed bench sre report");

    let output = cairn()
        .current_dir(dir.path())
        .args([
            "admin",
            "sre",
            "report",
            "--bench-report-dir",
            bench.path().to_str().expect("utf8"),
        ])
        .output()
        .expect("run sre report");

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(78));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("sre.json"), "stderr: {stderr}");
}

#[test]
fn admin_sre_report_rejects_schema_invalid_sre_json() {
    let dir = bootstrap_vault();
    let parent = tempfile::tempdir().expect("bench parent");
    let bench = parent
        .path()
        .join("SECRET_PRIVATE_TOKEN private body query text");
    std::fs::create_dir(&bench).expect("create bench dir");

    for body in [r"{}", r#"{"checks":[{}]}"#] {
        std::fs::write(bench.join("sre.json"), body).expect("write schema-invalid report");
        let output = cairn()
            .current_dir(dir.path())
            .args([
                "admin",
                "sre",
                "report",
                "--bench-report-dir",
                bench.to_str().expect("utf8"),
            ])
            .output()
            .expect("run sre report");

        assert!(!output.status.success(), "body: {body}");
        assert_eq!(output.status.code(), Some(78), "body: {body}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("sre.json"), "stderr: {stderr}");
        assert_forbidden_fragments_absent(&stderr);
    }
}

#[test]
fn admin_sre_report_preserves_unknown_gate_rollup() {
    let dir = bootstrap_vault();
    let bench = tempfile::tempdir().expect("bench dir");
    std::fs::write(
        bench.path().join("sre.json"),
        r#"{"checks":[{"name":"future_gate","status":"unknown","measured":1,"threshold":2,"unit":"ms","detail":"fixture"}]}"#,
    )
    .expect("write bench sre report");

    let output = cairn()
        .current_dir(dir.path())
        .args([
            "admin",
            "sre",
            "report",
            "--json",
            "--bench-report-dir",
            bench.path().to_str().expect("utf8"),
        ])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"name\":\"redacted_gate\""),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("\"gates\":{\"status\":\"unknown\""),
        "stdout: {stdout}"
    );
}

#[test]
fn admin_sre_report_prioritizes_unknown_over_warning_gate_rollup() {
    let dir = bootstrap_vault();
    let bench = tempfile::tempdir().expect("bench dir");
    std::fs::write(
        bench.path().join("sre.json"),
        r#"{"checks":[{"name":"future_gate","status":"unknown","measured":1,"threshold":2,"unit":"ms","detail":"fixture"},{"name":"migration_backlog","status":"warning","measured":500000,"threshold":600000,"unit":"ms","detail":"fixture"}]}"#,
    )
    .expect("write bench sre report");

    let output = cairn()
        .current_dir(dir.path())
        .args([
            "admin",
            "sre",
            "report",
            "--json",
            "--bench-report-dir",
            bench.path().to_str().expect("utf8"),
        ])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"gates\":{\"status\":\"unknown\""),
        "stdout: {stdout}"
    );
}

#[test]
fn admin_sre_report_scrubs_imported_gate_labels() {
    let dir = bootstrap_vault();
    let bench = tempfile::tempdir().expect("bench dir");
    std::fs::write(
        bench.path().join("sre.json"),
        r#"{"checks":[{"name":"/Users/alice/private body","status":"fail","measured":1,"threshold":0,"unit":"SECRET_PRIVATE_TOKEN","detail":"query text from /Users/alice"}]}"#,
    )
    .expect("write bench sre report");

    let output = cairn()
        .current_dir(dir.path())
        .args([
            "admin",
            "sre",
            "report",
            "--json",
            "--bench-report-dir",
            bench.path().to_str().expect("utf8"),
        ])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("redacted"), "stdout: {stdout}");
    assert_forbidden_fragments_absent(&stdout);
}

#[test]
fn admin_sre_report_scrubs_unsafe_vault_label() {
    let parent = tempfile::tempdir().expect("parent dir");
    let vault = parent
        .path()
        .join("SECRET_PRIVATE_TOKEN private body query text");
    std::fs::create_dir(&vault).expect("create vault dir");
    let bootstrap = cairn()
        .args(["bootstrap", "--vault-path", vault.to_str().expect("utf8")])
        .output()
        .expect("bootstrap");
    assert!(
        bootstrap.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&bootstrap.stderr)
    );

    let output = cairn()
        .current_dir(&vault)
        .args(["admin", "sre", "report", "--json"])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"name\":\"local_vault\""),
        "stdout: {stdout}"
    );
    assert_forbidden_fragments_absent(&stdout);
}

#[test]
fn admin_sre_report_human_includes_safe_actionable_details() {
    let dir = bootstrap_vault();
    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"search_completed","ts_ms":2,"mode":"semantic","hit_count":0,"latency_ms":42,"degradation_state":"partial","error":null}
"#,
    )
    .expect("write metrics");
    let bench = tempfile::tempdir().expect("bench dir");
    std::fs::write(
        bench.path().join("sre.json"),
        r#"{"checks":[{"name":"migration_backlog","status":"fail","measured":742000,"threshold":600000,"unit":"ms","detail":"fixture"}],"private":"/Users/alice SECRET_PRIVATE_TOKEN private body query text"}"#,
    )
    .expect("write bench sre report");

    let output = cairn()
        .current_dir(dir.path())
        .args([
            "admin",
            "sre",
            "report",
            "--bench-report-dir",
            bench.path().to_str().expect("utf8"),
        ])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("semantic"), "stdout: {stdout}");
    assert!(stdout.contains("degraded 1/1"), "stdout: {stdout}");
    assert!(stdout.contains("migration_backlog"), "stdout: {stdout}");
    assert_forbidden_fragments_absent(&stdout);
}

#[test]
fn admin_sre_report_human_shows_search_failures_and_degradations() {
    let dir = bootstrap_vault();
    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"search_completed","ts_ms":1,"mode":"semantic","hit_count":0,"latency_ms":42,"degradation_state":"partial","error":null}
{"event":"search_completed","ts_ms":2,"mode":"semantic","hit_count":0,"latency_ms":43,"degradation_state":"partial","error":"provider_unavailable"}
"#,
    )
    .expect("write metrics");

    let output = cairn()
        .current_dir(dir.path())
        .args(["admin", "sre", "report"])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("semantic"), "stdout: {stdout}");
    assert!(stdout.contains("failed 1/2"), "stdout: {stdout}");
    assert!(stdout.contains("degraded 2/2"), "stdout: {stdout}");
    assert_forbidden_fragments_absent(&stdout);
}

#[test]
fn admin_sre_report_human_rolls_up_rehydration_failures() {
    let dir = bootstrap_vault();
    let metrics = dir.path().join(".cairn/metrics.jsonl");
    std::fs::write(
        &metrics,
        r#"{"event":"rehydration_completed","ts_ms":1,"target":"session","source_tier":"cold","restored_tier":"warm","status":"committed","latency_ms":5000,"bytes_restored":1000,"record_count":2,"error":null}
"#,
    )
    .expect("write metrics");

    let output = cairn()
        .current_dir(dir.path())
        .args(["admin", "sre", "report"])
        .output()
        .expect("run sre report");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("SRE status: fail"), "stdout: {stdout}");
}
