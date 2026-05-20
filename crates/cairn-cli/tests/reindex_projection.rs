//! Integration tests for `cairn reindex` projection dispatch.

use std::{
    future::Future,
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc,
    thread,
};

use cairn_core::{contract::memory_store::MemoryStore, domain::projection::ProjectionTarget};
use cairn_store_sqlite::SqliteMemoryStore;

fn cli() -> std::process::Command {
    std::process::Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn spawn_projection_server() -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for stream in listener.incoming().take(2) {
            let mut stream = stream.expect("stream");
            let mut request = [0_u8; 8192];
            let len = stream.read(&mut request).expect("read request");
            let raw = String::from_utf8_lossy(&request[..len]).to_string();
            tx.send(raw.clone()).expect("send request");
            let (_headers, request_body) = raw.split_once("\r\n\r\n").expect("request body");
            let parsed: serde_json::Value =
                serde_json::from_str(request_body).expect("request json");
            let items = parsed["items"]
                .as_array()
                .expect("items array")
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
            stream.write_all(response.as_bytes()).expect("write");
        }
    });
    (format!("http://{addr}"), rx)
}

fn spawn_incomplete_projection_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut request = [0_u8; 8192];
            let _len = stream.read(&mut request).expect("read request");
            let body = r#"{"items":[]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).expect("write");
        }
    });
    format!("http://{addr}")
}

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("test runtime")
        .block_on(future)
}

#[test]
fn reindex_help_lists_from_db() {
    let out = cli()
        .args(["reindex", "--help"])
        .output()
        .expect("cairn reindex --help");

    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(stdout.contains("--from-db"), "{stdout}");
}

#[test]
fn reindex_from_db_requires_nexus_sandbox() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join(".cairn")).expect("mkdir");
    std::fs::write(
        dir.path().join(".cairn/config.yaml"),
        "store:\n  kind: sqlite\n",
    )
    .expect("config");

    let out = cli()
        .current_dir(dir.path())
        .args(["reindex", "--from-db", "--json"])
        .output()
        .expect("cairn reindex --from-db");

    assert!(!out.status.success(), "expected failure");
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("requires store.kind: nexus-sandbox"),
        "{stderr}"
    );
}

#[test]
fn reindex_from_db_posts_to_projection_endpoint() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (endpoint, request_rx) = spawn_projection_server();
    std::fs::create_dir(dir.path().join(".cairn")).expect("mkdir");
    std::fs::write(
        dir.path().join(".cairn/config.yaml"),
        format!(
            "store:\n  kind: nexus-sandbox\n  nexus:\n    endpoint: \"{endpoint}\"\n    health_path: /health\n"
        ),
    )
    .expect("config");

    let out = cli()
        .current_dir(dir.path())
        .args(["reindex", "--from-db", "--json"])
        .output()
        .expect("cairn reindex --from-db --json");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stderr, b"");
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("json stdout");
    assert_eq!(parsed["target"], "bm25s_lexical");
    assert_eq!(parsed["items"], 0);

    let request = request_rx.recv().expect("projection request");
    assert!(
        request.starts_with("POST /projection/apply HTTP/1.1\r\n"),
        "{request}"
    );
    let (_headers, body) = request.split_once("\r\n\r\n").expect("request body");
    let body: serde_json::Value = serde_json::from_str(body).expect("json request body");
    assert_eq!(body["target"], "bm25s_lexical");
    assert!(body["items"].as_array().expect("items array").is_empty());
}

#[test]
fn reindex_from_db_posts_records_and_updates_projection_ledger() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (endpoint, request_rx) = spawn_projection_server();
    std::fs::create_dir(dir.path().join(".cairn")).expect("mkdir");
    std::fs::write(
        dir.path().join(".cairn/config.yaml"),
        format!(
            "store:\n  kind: nexus-sandbox\n  nexus:\n    endpoint: \"{endpoint}\"\n    health_path: /health\n"
        ),
    )
    .expect("config");
    let db_path = dir.path().join(".cairn/cairn.db");
    let store = SqliteMemoryStore::open(&db_path).expect("open sqlite");
    store
        .insert_test_record(
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "projection rebuild source record",
            1,
            "sha256:record-a",
        )
        .expect("insert record");

    let out = cli()
        .current_dir(dir.path())
        .args(["reindex", "--from-db", "--json"])
        .output()
        .expect("cairn reindex --from-db --json");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json stdout");
    assert_eq!(stdout["items"], 1);
    let request = request_rx.recv().expect("projection request");
    let (_headers, body) = request.split_once("\r\n\r\n").expect("request body");
    let body: serde_json::Value = serde_json::from_str(body).expect("json request body");
    assert_eq!(body["target"], "bm25s_lexical");
    assert_eq!(body["items"][0]["record_id"], "01ARZ3NDEKTSV4RRFFQ69G5FAV");
    assert_eq!(body["items"][0]["body"], "projection rebuild source record");

    let summaries = block_on(store.projection_summaries()).expect("summaries");
    let bm25s = summaries
        .iter()
        .find(|summary| summary.target == ProjectionTarget::Bm25sLexical)
        .expect("bm25s summary");
    assert_eq!(bm25s.current_items, 1);
    assert_eq!(bm25s.lagging_items, 0);
    let body: String = rusqlite::Connection::open(db_path)
        .expect("open db")
        .query_row(
            "SELECT body FROM records WHERE record_id = '01ARZ3NDEKTSV4RRFFQ69G5FAV'",
            [],
            |row| row.get(0),
        )
        .expect("record body");
    assert_eq!(body, "projection rebuild source record");
}

#[test]
fn reindex_from_db_fails_when_projection_response_omits_requested_record() {
    let dir = tempfile::tempdir().expect("tempdir");
    let endpoint = spawn_incomplete_projection_server();
    std::fs::create_dir(dir.path().join(".cairn")).expect("mkdir");
    std::fs::write(
        dir.path().join(".cairn/config.yaml"),
        format!(
            "store:\n  kind: nexus-sandbox\n  nexus:\n    endpoint: \"{endpoint}\"\n    health_path: /health\n"
        ),
    )
    .expect("config");
    let store = SqliteMemoryStore::open(&dir.path().join(".cairn/cairn.db")).expect("open sqlite");
    store
        .insert_test_record(
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "projection rebuild source record",
            1,
            "sha256:record-a",
        )
        .expect("insert record");

    let out = cli()
        .current_dir(dir.path())
        .args(["reindex", "--from-db", "--json"])
        .output()
        .expect("cairn reindex --from-db --json");

    assert_eq!(out.status.code(), Some(69));
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(stderr.contains("missing projection response"), "{stderr}");
}

#[test]
fn reindex_from_db_posts_parser_projection_for_source_records() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (endpoint, request_rx) = spawn_projection_server();
    std::fs::create_dir(dir.path().join(".cairn")).expect("mkdir");
    std::fs::write(
        dir.path().join(".cairn/config.yaml"),
        format!(
            "store:\n  kind: nexus-sandbox\n  nexus:\n    endpoint: \"{endpoint}\"\n    health_path: /health\n"
        ),
    )
    .expect("config");
    let db_path = dir.path().join(".cairn/cairn.db");
    let store = SqliteMemoryStore::open(&db_path).expect("open sqlite");
    store
        .insert_test_record_with_source(
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "parser projection source",
            1,
            "sha256:record-a",
            "sources/sample.pdf",
            "sha256:pdf-source-a",
        )
        .expect("insert record");

    let out = cli()
        .current_dir(dir.path())
        .args(["reindex", "--from-db", "--json"])
        .output()
        .expect("cairn reindex --from-db --json");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let first = request_rx.recv().expect("bm25s request");
    let second = request_rx.recv().expect("parser request");
    let request_bodies = [first, second].map(|request| {
        let (_headers, body) = request.split_once("\r\n\r\n").expect("request body");
        serde_json::from_str::<serde_json::Value>(body).expect("json request body")
    });
    let bm25s_request = request_bodies
        .iter()
        .find(|body| body["target"] == "bm25s_lexical")
        .expect("bm25s request");
    assert!(bm25s_request["items"][0].get("source_path").is_none());
    assert!(bm25s_request["items"][0].get("source_hash").is_none());
    let body = request_bodies
        .iter()
        .find(|body| body["target"] == "parser_pdf_text")
        .expect("parser request");
    assert_eq!(body["target"], "parser_pdf_text");
    assert_eq!(body["items"][0]["source_path"], "sources/sample.pdf");
    assert_eq!(body["items"][0]["source_hash"], "sha256:pdf-source-a");

    let summaries = block_on(store.projection_summaries()).expect("summaries");
    let parser = summaries
        .iter()
        .find(|summary| summary.target == ProjectionTarget::from_key("parser_pdf_text").unwrap())
        .expect("parser summary");
    assert_eq!(parser.current_items, 1);
    assert_eq!(parser.lagging_items, 0);
}

#[test]
fn reindex_from_db_reports_projection_endpoint_unavailable() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join(".cairn")).expect("mkdir");
    std::fs::write(
        dir.path().join(".cairn/config.yaml"),
        "store:\n  kind: nexus-sandbox\n  nexus:\n    endpoint: \"http://127.0.0.1:9\"\n    health_path: /health\n",
    )
    .expect("config");

    let out = cli()
        .current_dir(dir.path())
        .args(["reindex", "--from-db", "--json"])
        .output()
        .expect("cairn reindex unavailable");

    assert_eq!(out.status.code(), Some(69));
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(stderr.contains("projection endpoint"), "{stderr}");
}

#[test]
fn projection_parser_fixtures_are_present_and_typed() {
    let fixture_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("fixtures/v0/projection");
    for name in [
        "pdf-text.json",
        "docx-text.json",
        "video-frame.json",
        "vision-caption.json",
        "parser-failure.json",
    ] {
        let bytes = std::fs::read(fixture_dir.join(name)).expect("read projection fixture");
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).expect("projection fixture json");
        assert!(
            value["projection_target"].as_str().is_some(),
            "{name}: {value:#}"
        );
        assert!(value["source_hash"].as_str().is_some(), "{name}: {value:#}");
        assert!(
            value["expected_state"].as_str().is_some(),
            "{name}: {value:#}"
        );
    }
}
