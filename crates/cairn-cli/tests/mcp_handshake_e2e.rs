//! End-to-end CLI test for the `handshake` MCP prelude tool (issue #65).
//!
//! Spawns a real `cairn mcp` subprocess against a freshly-bootstrapped vault
//! with `single_tenant: true` + a configured principal, drives the wire path
//! with raw JSON-RPC frames over the subprocess's stdin/stdout, and asserts:
//!
//!  - `tools/list` includes `handshake`.
//!  - Two `tools/call` invocations of `handshake` return distinct nonces.
//!  - The persisted nonces show up keyed by `issuer` in
//!    `outstanding_challenges`, exactly as the next signed mutation will
//!    redeem them (brief §4.2).
//!
//! This complements `cairn-mcp/tests/handshake_tool.rs`, which exercises the
//! same logic in-process via `tokio::io::duplex`. Together they pin both the
//! handler internals and the CLI subcommand wiring.
#![allow(missing_docs)]

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;
use tempfile::TempDir;

fn cli_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cairn")
}

/// Bootstrap a vault under a fresh tempdir and return the path. The
/// bootstrap subcommand also fetches the embedding model on first run; CI
/// runners pre-cache it so this stays cheap.
fn bootstrap_vault() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let status = Command::new(cli_bin())
        .args(["bootstrap", "--vault-path"])
        .arg(dir.path())
        .env_remove("CAIRN_VAULT")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn cairn bootstrap");
    assert!(status.success(), "bootstrap failed: {status}");
    dir
}

/// Patch the freshly-bootstrapped config to enable `single_tenant: true`
/// and configure a principal so `cairn mcp` opens the sqlite store and
/// advertises `handshake`.
///
/// The bootstrap writes the `mcp.stdio.single_tenant: false` line buried
/// among other top-level config sections, so we insert `principal:` inline
/// at the same indentation as the `single_tenant:` line — appending at
/// EOF would land outside the `mcp:` mapping and break YAML parsing.
fn enable_single_tenant(vault: &Path, tenant: &str) {
    let config_path = vault.join(".cairn/config.yaml");
    let original = std::fs::read_to_string(&config_path).expect("read config");
    let mut out = String::with_capacity(original.len() + 64);
    let mut inserted = false;
    for line in original.lines() {
        if !inserted && line.starts_with("    single_tenant:") {
            out.push_str("    single_tenant: true\n");
            out.push_str("    principal:\n");
            writeln!(out, "      tenant: {tenant}").expect("write to String");
            inserted = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    assert!(
        inserted,
        "expected `    single_tenant:` line in bootstrapped config; got:\n{original}"
    );
    std::fs::write(&config_path, out).expect("write patched config");
}

fn send_frame(stdin: &mut impl Write, json: &str) {
    stdin.write_all(json.as_bytes()).expect("write frame");
    stdin.write_all(b"\n").expect("write newline");
    stdin.flush().expect("flush");
}

fn recv_frame(
    reader: &mut BufReader<impl std::io::Read>,
    stderr_so_far: &dyn Fn() -> String,
) -> Value {
    let mut line = String::new();
    let n = reader.read_line(&mut line).expect("read frame");
    assert!(
        n > 0,
        "child closed stdout before sending a response frame\n--- captured stderr ---\n{}",
        stderr_so_far()
    );
    serde_json::from_str(line.trim()).unwrap_or_else(|e| {
        panic!(
            "response is not JSON: {e}\nline: {line}\n--- captured stderr ---\n{}",
            stderr_so_far()
        );
    })
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "wire-protocol e2e — keeping the full \
    subprocess setup, frame send/recv, list+call, and post-mortem persistence \
    probe in one function makes the failure trace single-shot"
)]
fn cairn_mcp_subprocess_advertises_and_persists_handshake() {
    let vault = bootstrap_vault();
    enable_single_tenant(vault.path(), "e2e-cli");

    let mut child = Command::new(cli_bin())
        .args(["--vault"])
        .arg(vault.path())
        .arg("mcp")
        .env_remove("CAIRN_VAULT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cairn mcp");

    let mut stdin = child.stdin.take().expect("child stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("child stdout"));
    let stderr_handle = child.stderr.take().expect("child stderr");
    let stderr_collected = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let stderr_clone = stderr_collected.clone();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr_handle);
        let mut buf = String::new();
        while reader.read_line(&mut buf).unwrap_or(0) > 0 {
            stderr_clone.lock().unwrap().push_str(&buf);
            buf.clear();
        }
    });
    let stderr_so_far = || stderr_collected.lock().unwrap().clone();

    // Drive the wire protocol.
    send_frame(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"cli-e2e","version":"0.0.0"}}}"#,
    );
    let init_resp = recv_frame(&mut stdout, &stderr_so_far);
    let contract = init_resp
        .pointer("/result/capabilities/experimental/cairn.status/contract")
        .and_then(Value::as_str);
    assert_eq!(contract, Some("cairn.mcp.v1"));

    send_frame(
        &mut stdin,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    );

    send_frame(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    );
    let list_resp = recv_frame(&mut stdout, &stderr_so_far);
    let names: Vec<&str> = list_resp
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .expect("tools array")
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect();
    assert!(
        names.contains(&"handshake"),
        "tools/list must include `handshake` over the CLI subprocess; got: {names:?}"
    );

    // Mint two challenges keyed by the same issuer; they must be distinct.
    let mut nonces: Vec<String> = Vec::with_capacity(2);
    for id in 3..=4 {
        send_frame(
            &mut stdin,
            &format!(
                r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"handshake","arguments":{{"issuer":"hmn:e2e-cli"}}}}}}"#
            ),
        );
        let resp = recv_frame(&mut stdout, &stderr_so_far);
        let text = resp
            .pointer("/result/content/0/text")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("missing content text in: {resp}"));
        let payload: Value =
            serde_json::from_str(text).expect("handshake payload must be valid JSON");
        assert_eq!(
            payload.pointer("/contract").and_then(Value::as_str),
            Some("cairn.mcp.v1")
        );
        let nonce = payload
            .pointer("/challenge/nonce")
            .and_then(Value::as_str)
            .expect("challenge.nonce")
            .to_owned();
        nonces.push(nonce);
    }
    assert_eq!(nonces.len(), 2);
    assert_ne!(
        nonces[0], nonces[1],
        "two back-to-back handshake calls must return distinct nonces (§8.0.a-d)"
    );

    // Close stdin so the server shuts down cleanly, then wait.
    drop(stdin);
    let status = child.wait().expect("wait for cairn mcp");
    assert!(status.success(), "cairn mcp exited non-zero: {status}");

    // Verify the persistence half end-to-end: open the same sqlite file
    // (read-only) and count outstanding challenges keyed by issuer. This
    // proves the wire response we just received corresponds to a row in
    // `outstanding_challenges` that the next signed envelope can redeem.
    let db = vault.path().join(".cairn/cairn.db");
    let conn =
        rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open cairn.db read-only");
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM outstanding_challenges WHERE issuer = ?1",
            ["hmn:e2e-cli"],
            |row| row.get(0),
        )
        .expect("count outstanding_challenges");
    assert_eq!(
        count, 2,
        "expected exactly two persisted challenges for the issuer; got {count}"
    );
}
