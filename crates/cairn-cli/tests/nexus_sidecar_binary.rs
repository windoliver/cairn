//! End-to-end coverage for the bundled Cairn Nexus sandbox sidecar stub.

use std::io::{Read, Write};
use std::process::Command;
use std::time::Duration;

use cairn_cli::nexus::{NexusSupervisor, ProjectionProbe, SupervisorConfig};
use serde_json::json;

fn sidecar_binary() -> String {
    std::env::var("CARGO_BIN_EXE_cairn-nexus-sandbox")
        .expect("cargo must expose the cairn-nexus-sandbox binary path")
}

fn reserve_loopback_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn start_sidecar(endpoint: String, tmp: &tempfile::TempDir) -> NexusSupervisor {
    let vault_dir = tmp.path().join("vault");
    let data_dir = vault_dir.join("nexus-data");
    let sqlite_db = vault_dir.join(".cairn").join("cairn.db");
    let mut supervisor = NexusSupervisor::start(SupervisorConfig {
        command: sidecar_binary(),
        args: vec!["sandbox".to_owned(), "serve".to_owned()],
        endpoint,
        health_path: "/health".to_owned(),
        data_dir,
        sqlite_db,
        health_timeout: Duration::from_secs(5),
        shutdown_timeout: Duration::from_millis(500),
    })
    .unwrap();
    assert_eq!(supervisor.wait_until_healthy(), ProjectionProbe::Healthy);
    supervisor
}

fn post_json(endpoint: &str, path: &str, value: &serde_json::Value) -> serde_json::Value {
    let addr = endpoint.strip_prefix("http://").expect("http endpoint");
    let body = value.to_string();
    let mut stream = std::net::TcpStream::connect(addr).expect("connect sidecar");
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).expect("write request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    let (_head, body) = response.split_once("\r\n\r\n").expect("http body");
    serde_json::from_str(body).expect("json response")
}

#[test]
fn sidecar_help_exits_zero() {
    let out = Command::new(sidecar_binary())
        .arg("--help")
        .output()
        .expect("spawn cairn-nexus-sandbox --help");

    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(stdout.contains("sandbox serve"), "{stdout}");
}

#[test]
fn bundled_sidecar_serves_health_through_supervisor() {
    let mut last_probe = None;
    for _ in 0..5 {
        let tmp = tempfile::tempdir().unwrap();
        let vault_dir = tmp.path().join("vault");
        let data_dir = vault_dir.join("nexus-data");
        let sqlite_db = vault_dir.join(".cairn").join("cairn.db");
        let endpoint = format!("http://127.0.0.1:{}", reserve_loopback_port());
        let mut supervisor = NexusSupervisor::start(SupervisorConfig {
            command: sidecar_binary(),
            args: vec!["sandbox".to_owned(), "serve".to_owned()],
            endpoint,
            health_path: "/health".to_owned(),
            data_dir: data_dir.clone(),
            sqlite_db,
            health_timeout: Duration::from_secs(5),
            shutdown_timeout: Duration::from_millis(500),
        })
        .unwrap();

        match supervisor.wait_until_healthy() {
            ProjectionProbe::Healthy => {
                assert!(data_dir.is_dir());
                supervisor.stop().unwrap();
                return;
            }
            probe @ ProjectionProbe::Degraded(_) => {
                last_probe = Some(probe);
                let _ = supervisor.stop();
            }
        }
    }

    panic!("bundled sidecar did not become healthy after retries: {last_probe:?}");
}

#[test]
fn bundled_sidecar_reads_large_projection_apply_body() {
    let tmp = tempfile::tempdir().unwrap();
    let endpoint = format!("http://127.0.0.1:{}", reserve_loopback_port());
    let mut supervisor = start_sidecar(endpoint.clone(), &tmp);
    let large_body = "alpha ".repeat(20_000);

    let response = post_json(
        &endpoint,
        "/projection/apply",
        &json!({
            "operation_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "target": "bm25s_lexical",
            "items": [{
                "record_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "wal_sequence": 1,
                "record_hash": "hash-alpha",
                "body": large_body
            }]
        }),
    );

    assert_eq!(response["items"][0]["state"], "current");
    assert_eq!(response["items"][0]["record_hash"], "hash-alpha");
    supervisor.stop().unwrap();
}

#[test]
fn bundled_sidecar_search_uses_applied_bm25s_projection() {
    let tmp = tempfile::tempdir().unwrap();
    let endpoint = format!("http://127.0.0.1:{}", reserve_loopback_port());
    let mut supervisor = start_sidecar(endpoint.clone(), &tmp);
    let apply = post_json(
        &endpoint,
        "/projection/apply",
        &json!({
            "operation_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "target": "bm25s_lexical",
            "items": [
                {
                    "record_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    "wal_sequence": 1,
                    "record_hash": "hash-alpha",
                    "body": "alpha alpha projection"
                },
                {
                    "record_id": "01ARZ3NDEKTSV4RRFFQ69G5FAW",
                    "wal_sequence": 2,
                    "record_hash": "hash-beta",
                    "body": "beta projection"
                }
            ]
        }),
    );
    assert_eq!(apply["items"].as_array().expect("items").len(), 2);

    let search = post_json(
        &endpoint,
        "/projection/search",
        &json!({
            "operation_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "query": "alpha",
            "limit": 10,
            "candidates": [
                {
                    "record_id": "01ARZ3NDEKTSV4RRFFQ69G5FAW",
                    "record_hash": "hash-beta"
                },
                {
                    "record_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    "record_hash": "hash-alpha"
                }
            ]
        }),
    );

    assert_eq!(search["hits"][0]["record_id"], "01ARZ3NDEKTSV4RRFFQ69G5FAV");
    assert!(search["hits"][0]["score"].as_f64().expect("score") > 0.0);
    supervisor.stop().unwrap();
}

#[test]
fn bundled_sidecar_search_requires_current_projected_hash() {
    let tmp = tempfile::tempdir().unwrap();
    let endpoint = format!("http://127.0.0.1:{}", reserve_loopback_port());
    let mut supervisor = start_sidecar(endpoint.clone(), &tmp);
    let apply = post_json(
        &endpoint,
        "/projection/apply",
        &json!({
            "operation_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "target": "bm25s_lexical",
            "items": [{
                "record_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "wal_sequence": 1,
                "record_hash": "hash-old",
                "body": "alpha projection"
            }]
        }),
    );
    assert_eq!(apply["items"][0]["state"], "current");

    let search = post_json(
        &endpoint,
        "/projection/search",
        &json!({
            "operation_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "query": "alpha",
            "limit": 10,
            "candidates": [{
                "record_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "record_hash": "hash-new"
            }]
        }),
    );

    assert!(search["hits"].as_array().expect("hits").is_empty());
    supervisor.stop().unwrap();
}

#[test]
fn bundled_sidecar_full_bm25s_rebuild_removes_omitted_documents() {
    let tmp = tempfile::tempdir().unwrap();
    let endpoint = format!("http://127.0.0.1:{}", reserve_loopback_port());
    let mut supervisor = start_sidecar(endpoint.clone(), &tmp);
    let first_apply = post_json(
        &endpoint,
        "/projection/apply",
        &json!({
            "operation_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "target": "bm25s_lexical",
            "items": [
                {
                    "record_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    "wal_sequence": 1,
                    "record_hash": "hash-alpha",
                    "body": "alpha projection"
                },
                {
                    "record_id": "01ARZ3NDEKTSV4RRFFQ69G5FAW",
                    "wal_sequence": 2,
                    "record_hash": "hash-beta",
                    "body": "beta projection"
                }
            ]
        }),
    );
    assert_eq!(first_apply["items"].as_array().expect("items").len(), 2);
    let second_apply = post_json(
        &endpoint,
        "/projection/apply",
        &json!({
            "operation_id": "01ARZ3NDEKTSV4RRFFQ69G5FAW",
            "target": "bm25s_lexical",
            "items": [{
                "record_id": "01ARZ3NDEKTSV4RRFFQ69G5FAW",
                "wal_sequence": 2,
                "record_hash": "hash-beta",
                "body": "beta projection"
            }]
        }),
    );
    assert_eq!(second_apply["items"].as_array().expect("items").len(), 1);

    let search = post_json(
        &endpoint,
        "/projection/search",
        &json!({
            "operation_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "query": "alpha",
            "limit": 10,
            "candidates": [{
                "record_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "record_hash": "hash-alpha"
            }]
        }),
    );

    assert!(search["hits"].as_array().expect("hits").is_empty());
    supervisor.stop().unwrap();
}
