//! Integration tests for `cairn reindex` projection dispatch.

use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

fn cli() -> std::process::Command {
    std::process::Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn spawn_projection_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    thread::spawn(move || {
        for stream in listener.incoming().take(1) {
            let mut stream = stream.expect("stream");
            let mut request = [0_u8; 8192];
            let _ = stream.read(&mut request);
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
    let endpoint = spawn_projection_server();
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
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(stdout.contains("\"target\":\"bm25s_lexical\""), "{stdout}");
}
