//! Integration tests for `cairn lint` projection diagnostics.

use std::future::Future;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::thread;

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

const RECORD_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn write_nexus_config(vault: &std::path::Path) {
    let dir = vault.join(".cairn");
    std::fs::create_dir_all(&dir).expect("create .cairn");
    std::fs::write(
        dir.join("config.yaml"),
        "store:\n  kind: nexus-sandbox\n  nexus:\n    endpoint: http://127.0.0.1:1\n",
    )
    .expect("write config");
}

fn ensure_db(vault: &std::path::Path) {
    let _store = SqliteMemoryStore::open(&vault.join(".cairn/cairn.db")).expect("open sqlite");
}

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("test runtime")
        .block_on(future)
}

fn record_id(raw: &str) -> RecordId {
    RecordId::parse(raw).expect("valid test ULID")
}

fn seed_failed_projection(vault: &std::path::Path) {
    let store = SqliteMemoryStore::open(&vault.join(".cairn/cairn.db")).expect("open sqlite");
    store
        .insert_test_record_with_source(
            RECORD_ID,
            "failed projection record",
            1,
            "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "sources/corrupt.pdf",
            "sha256:pdf-source-corrupt",
        )
        .expect("insert record");
    block_on(store.apply_projection_items(vec![
        ProjectionApplyItem {
            row: ProjectionLedgerRow {
                target: ProjectionTarget::Bm25sLexical,
                cursor: ProjectionCursor {
                    record_id: record_id(RECORD_ID),
                    wal_sequence: 1,
                    record_hash: "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                    source_hash: None,
                },
                state: ProjectionItemState::Current,
                updated_at: "2026-05-19T12:00:00Z".to_owned(),
            },
        },
        ProjectionApplyItem {
            row: ProjectionLedgerRow {
                target: ProjectionTarget::Parser(
                    cairn_core::domain::projection::ParserProjectionKind::PdfText,
                ),
                cursor: ProjectionCursor {
                    record_id: record_id(RECORD_ID),
                    wal_sequence: 1,
                    record_hash: "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                    source_hash: Some("sha256:pdf-source-corrupt".to_owned()),
                },
                state: ProjectionItemState::Failed {
                    reason: "parser failed".to_owned(),
                },
                updated_at: "2026-05-19T12:00:00Z".to_owned(),
            },
        },
    ]))
    .expect("apply failed projection");
}

fn spawn_health_and_apply_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    thread::spawn(move || {
        for stream in listener.incoming().take(2) {
            let mut stream = stream.expect("stream");
            let mut request = [0_u8; 8192];
            let len = stream.read(&mut request).expect("read request");
            let raw = String::from_utf8_lossy(&request[..len]).to_string();
            if raw.starts_with("GET /health HTTP/") {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                    .expect("write health");
                continue;
            }
            let (_headers, body) = raw.split_once("\r\n\r\n").expect("request body");
            let parsed: serde_json::Value = serde_json::from_str(body).expect("request json");
            let items = parsed["items"]
                .as_array()
                .expect("items")
                .iter()
                .map(|item| {
                    serde_json::json!({
                        "record_id": item["record_id"],
                        "record_hash": item["record_hash"],
                        "source_hash": item.get("source_hash").cloned().unwrap_or(serde_json::Value::Null),
                        "state": "current",
                        "reason": null
                    })
                })
                .collect::<Vec<_>>();
            let body = serde_json::json!({ "items": items }).to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).expect("write apply");
        }
    });
    format!("http://{addr}")
}

#[test]
fn lint_json_reports_projection_sidecar_unavailable() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_nexus_config(dir.path());
    ensure_db(dir.path());

    let out = cli()
        .current_dir(dir.path())
        .args(["lint", "--json"])
        .output()
        .expect("cairn lint --json");

    assert_ne!(out.status.code(), Some(78), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(
        stdout.contains("projection_sidecar_unavailable"),
        "{stdout}"
    );
}

#[test]
fn lint_fix_mentions_projection_rebuild() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_nexus_config(dir.path());
    ensure_db(dir.path());

    let out = cli()
        .current_dir(dir.path())
        .args(["lint", "--fix", "--json"])
        .output()
        .expect("cairn lint --fix --json");

    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(stdout.contains("rebuildable:true"), "{stdout}");
}

#[test]
fn lint_json_reports_parser_failed_summary() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_nexus_config(dir.path());
    seed_failed_projection(dir.path());

    let out = cli()
        .current_dir(dir.path())
        .args(["lint", "--json"])
        .output()
        .expect("cairn lint --json");

    assert_ne!(out.status.code(), Some(78), "exit: {:?}", out.status);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).expect("lint json");
    let data = &value["data"];
    assert_eq!(data["summary"]["by_kind"]["projection_failed"], 1);
    assert!(data["summary"]["by_kind"]["projection_stale"].is_null());
    let findings = data["findings"].as_array().expect("findings array");
    assert!(findings.iter().any(|finding| {
        finding["kind"] == "projection_parser_failed"
            && finding["entities"]
                .as_array()
                .expect("entities")
                .iter()
                .any(|entity| entity == "source_hash:sha256:pdf-source-corrupt")
    }));
    assert!(
        !findings
            .iter()
            .any(|finding| finding["kind"] == "projection_stale")
    );
}

#[test]
fn lint_json_reports_missing_projection_separately_from_stale() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_nexus_config(dir.path());
    let store = SqliteMemoryStore::open(&dir.path().join(".cairn/cairn.db")).expect("open sqlite");
    store
        .insert_test_record(
            RECORD_ID,
            "missing projection record",
            1,
            "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .expect("insert record");

    let out = cli()
        .current_dir(dir.path())
        .args(["lint", "--json"])
        .output()
        .expect("cairn lint --json");

    assert_ne!(out.status.code(), Some(78), "exit: {:?}", out.status);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).expect("lint json");
    let data = &value["data"];
    assert!(
        data["summary"]["by_kind"]["projection_missing"]
            .as_u64()
            .unwrap_or(0)
            >= 1
    );
    assert!(data["summary"]["by_kind"]["projection_stale"].is_null());
    let findings = data["findings"].as_array().expect("findings array");
    assert!(
        findings
            .iter()
            .any(|finding| finding["kind"] == "projection_missing"
                && finding["entities"]
                    .as_array()
                    .is_some_and(|entities| entities
                        .iter()
                        .any(|entity| entity == "projection_target:bm25s_lexical")))
    );
}

#[test]
fn lint_json_reports_hash_mismatch_projection_finding() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_nexus_config(dir.path());
    let store = SqliteMemoryStore::open(&dir.path().join(".cairn/cairn.db")).expect("open sqlite");
    store
        .insert_test_record(
            RECORD_ID,
            "hash mismatch record",
            1,
            "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .expect("insert record");
    block_on(store.apply_projection_items(vec![ProjectionApplyItem {
            row: ProjectionLedgerRow {
                target: ProjectionTarget::Bm25sLexical,
                cursor: ProjectionCursor {
                    record_id: record_id(RECORD_ID),
                    wal_sequence: 1,
                    record_hash:
                        "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                            .to_owned(),
                    source_hash: None,
                },
                state: ProjectionItemState::Failed {
                    reason: "projection hash mismatch".to_owned(),
                },
                updated_at: "2026-05-19T12:00:00Z".to_owned(),
            },
        }]))
    .expect("apply failed projection");

    let out = cli()
        .current_dir(dir.path())
        .args(["lint", "--json"])
        .output()
        .expect("cairn lint --json");

    assert_ne!(out.status.code(), Some(78), "exit: {:?}", out.status);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).expect("lint json");
    let findings = value["data"]["findings"]
        .as_array()
        .expect("findings array");
    assert!(findings.iter().any(|finding| {
        finding["kind"] == "projection_hash_mismatch"
            && finding["target"]["record_id"] == RECORD_ID
            && finding["entities"]
                .as_array()
                .expect("entities")
                .iter()
                .any(|entity| entity == "projection_target:bm25s_lexical")
    }));
}

#[test]
fn lint_fix_rebuilds_missing_projection_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let endpoint = spawn_health_and_apply_server();
    let cairn_dir = dir.path().join(".cairn");
    std::fs::create_dir_all(&cairn_dir).expect("create .cairn");
    std::fs::write(
        cairn_dir.join("config.yaml"),
        format!("store:\n  kind: nexus-sandbox\n  nexus:\n    endpoint: \"{endpoint}\"\n"),
    )
    .expect("write config");
    let store = SqliteMemoryStore::open(&dir.path().join(".cairn/cairn.db")).expect("open sqlite");
    store
        .insert_test_record(
            RECORD_ID,
            "missing projection record",
            1,
            "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .expect("insert record");

    let out = cli()
        .current_dir(dir.path())
        .args(["lint", "--fix", "--json"])
        .output()
        .expect("cairn lint --fix --json");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let summaries = block_on(store.projection_summaries()).expect("summaries");
    let bm25s = summaries
        .iter()
        .find(|summary| summary.target == ProjectionTarget::Bm25sLexical)
        .expect("bm25s summary");
    assert_eq!(bm25s.current_items, 1);
    assert_eq!(bm25s.lagging_items, 0);
}
