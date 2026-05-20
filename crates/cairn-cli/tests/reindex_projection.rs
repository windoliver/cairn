//! Integration tests for `cairn reindex` projection dispatch.

use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc,
    thread,
};

fn cli() -> std::process::Command {
    std::process::Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn spawn_projection_server() -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for stream in listener.incoming().take(1) {
            let mut stream = stream.expect("stream");
            let mut request = [0_u8; 8192];
            let len = stream.read(&mut request).expect("read request");
            tx.send(String::from_utf8_lossy(&request[..len]).to_string())
                .expect("send request");
            let body = r#"{"items":[]}"#;
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
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("projection fixture json");
        assert!(value["projection_target"].as_str().is_some(), "{name}: {value:#}");
        assert!(value["source_hash"].as_str().is_some(), "{name}: {value:#}");
        assert!(value["expected_state"].as_str().is_some(), "{name}: {value:#}");
    }
}
