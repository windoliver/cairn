//! Search projection CLI integration tests.

use std::{
    future::Future,
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc,
    thread,
};

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
const TEST_VAULT_ID: &str = "01HQZX9F5N0000000000000000";
const TEST_BODY_HASH: &str =
    "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn cli() -> std::process::Command {
    std::process::Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn write_config(dir: &tempfile::TempDir, body: &str) {
    let cairn_dir = dir.path().join(".cairn");
    std::fs::create_dir_all(&cairn_dir).expect("mkdir");
    std::fs::write(cairn_dir.join("vault.id"), TEST_VAULT_ID).expect("vault id");
    std::fs::write(cairn_dir.join("config.yaml"), body).expect("config");
}

fn seed_sqlite_record(dir: &tempfile::TempDir) {
    let store =
        SqliteMemoryStore::open(&dir.path().join(".cairn/cairn.db")).expect("open sqlite store");
    store
        .insert_test_record(
            RECORD_ID,
            "projection search maps sqlite ranking signals",
            1,
            TEST_BODY_HASH,
        )
        .expect("insert test record");
}

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("test runtime")
        .block_on(future)
}

fn seed_current_bm25s_projection(dir: &tempfile::TempDir) {
    let store =
        SqliteMemoryStore::open(&dir.path().join(".cairn/cairn.db")).expect("open sqlite store");
    block_on(store.apply_projection_items(vec![ProjectionApplyItem {
        row: ProjectionLedgerRow {
            target: ProjectionTarget::Bm25sLexical,
            cursor: ProjectionCursor {
                record_id: RecordId::parse(RECORD_ID).expect("record id"),
                wal_sequence: 1,
                record_hash: TEST_BODY_HASH.to_owned(),
                source_hash: None,
            },
            state: ProjectionItemState::Current,
            updated_at: "2026-05-19T12:00:00Z".to_owned(),
        },
    }]))
    .expect("apply bm25s projection");
}

fn spawn_bm25s_search_server() -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut request = [0_u8; 8192];
            let len = stream.read(&mut request).expect("read request");
            let raw = String::from_utf8_lossy(&request[..len]).to_string();
            tx.send(raw.clone()).expect("send request");
            let (_headers, body) = raw.split_once("\r\n\r\n").expect("request body");
            let parsed: serde_json::Value = serde_json::from_str(body).expect("request json");
            let candidate = &parsed["candidates"][0];
            let body = serde_json::json!({
                "hits": [{
                    "record_id": candidate["record_id"],
                    "record_hash": candidate["record_hash"],
                    "score": 2.5
                }]
            })
            .to_string();
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

#[test]
fn search_json_returns_empty_hits_for_empty_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_config(&dir, "store:\n  kind: sqlite\n");

    let out = cli()
        .current_dir(dir.path())
        .args([
            "search",
            "projection",
            "--mode",
            "keyword",
            "--bm25s",
            "disabled",
            "--json",
        ])
        .output()
        .expect("cairn search");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).expect("search json");
    assert!(
        value["data"]["hits"]
            .as_array()
            .expect("hits array")
            .is_empty()
    );
}

#[test]
fn search_json_maps_sqlite_ranking_signal() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_config(&dir, "store:\n  kind: sqlite\n");
    seed_sqlite_record(&dir);

    let out = cli()
        .current_dir(dir.path())
        .args([
            "search",
            "projection",
            "--mode",
            "keyword",
            "--bm25s",
            "disabled",
            "--json",
        ])
        .output()
        .expect("cairn search");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).expect("search json");
    assert_eq!(value["data"]["hits"][0]["record_id"], RECORD_ID);
    assert_eq!(
        value["data"]["hits"][0]["ranking_signals"][0]["name"],
        "sqlite_fts5"
    );
    assert_eq!(value["data"]["hits"][0]["ranking_signals"][0]["used"], true);
    assert!(value["data"]["hits"][0]["ranking_signals"][0]["score"].is_number());
}

#[test]
fn search_required_bm25s_uses_nexus_signal_when_available() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (endpoint, request_rx) = spawn_bm25s_search_server();
    write_config(
        &dir,
        &format!("store:\n  kind: nexus-sandbox\n  nexus:\n    endpoint: \"{endpoint}\"\n"),
    );
    seed_sqlite_record(&dir);
    seed_current_bm25s_projection(&dir);

    let out = cli()
        .current_dir(dir.path())
        .args([
            "search",
            "projection",
            "--mode",
            "keyword",
            "--bm25s",
            "required",
            "--json",
        ])
        .output()
        .expect("cairn search");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let request = request_rx.recv().expect("bm25s search request");
    assert!(request.starts_with("POST /projection/search HTTP/1.1\r\n"));
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).expect("search json");
    let signals = value["data"]["hits"][0]["ranking_signals"]
        .as_array()
        .expect("ranking signals");
    assert!(signals.iter().any(|signal| {
        signal["name"] == "nexus_bm25s"
            && signal["used"] == true
            && signal["score"].as_f64() == Some(2.5)
    }));
}

#[test]
fn search_required_bm25s_fails_closed_when_projection_ledger_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (endpoint, _request_rx) = spawn_bm25s_search_server();
    write_config(
        &dir,
        &format!("store:\n  kind: nexus-sandbox\n  nexus:\n    endpoint: \"{endpoint}\"\n"),
    );
    seed_sqlite_record(&dir);

    let out = cli()
        .current_dir(dir.path())
        .args([
            "search",
            "projection",
            "--mode",
            "keyword",
            "--bm25s",
            "required",
            "--json",
        ])
        .output()
        .expect("cairn search");

    assert!(!out.status.success(), "expected fail-closed search");
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(stdout.contains("CapabilityUnavailable"), "{stdout}");
    assert!(stdout.contains("projection not current"), "{stdout}");
}

#[test]
fn search_required_bm25s_fails_closed_without_nexus() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_config(&dir, "store:\n  kind: sqlite\n");

    let out = cli()
        .current_dir(dir.path())
        .args([
            "search",
            "projection",
            "--mode",
            "keyword",
            "--bm25s",
            "required",
            "--json",
        ])
        .output()
        .expect("cairn search");

    assert!(!out.status.success(), "expected fail-closed search");
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(stdout.contains("CapabilityUnavailable"), "{stdout}");
}

#[test]
fn search_required_bm25s_fails_closed_when_nexus_ranker_unavailable() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_config(
        &dir,
        "store:\n  kind: nexus-sandbox\n  nexus:\n    endpoint: http://127.0.0.1:1\n",
    );
    seed_sqlite_record(&dir);

    let out = cli()
        .current_dir(dir.path())
        .args([
            "search",
            "projection",
            "--mode",
            "keyword",
            "--bm25s",
            "required",
            "--json",
        ])
        .output()
        .expect("cairn search");

    assert!(!out.status.success(), "expected fail-closed search");
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(stdout.contains("CapabilityUnavailable"), "{stdout}");
}
