//! End-to-end CLI test for the `handshake` MCP prelude tool (issue #65).
//!
//! Spawns a real `cairn mcp` subprocess against a synthesized vault
//! (no `cairn bootstrap`, so no embedding-model download — round-1 review
//! #3) and drives `initialize` + `tools/list` over the child's stdin/stdout
//! to assert the production posture for `handshake`:
//!
//!  - When [`cairn_core::status::wiring::REPLAY_CHALLENGE_WIRED`] is
//!    `false` (current state), `handshake` is absent from `tools/list`
//!    even though the sqlite store is wired (brief §15 fail-closed,
//!    round-1 review #1).
//!  - When the flag is `true` (future state), `handshake` is present.
//!
//! Wire frames are read with channel-based deadlines and the subprocess
//! is killed-on-timeout so a regression in the CLI subcommand surfaces
//! as a bounded protocol failure rather than a hung CI job (round-1
//! review #3).
//!
//! Complements `cairn-mcp/tests/handshake_tool.rs` direct-dispatch tests
//! that exercise the mint / persistence path against a forced
//! `replay_challenge_wired = true` parameter, keeping the implementation
//! covered while the wire-level posture stays gated.
#![allow(missing_docs)]

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

fn cli_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cairn")
}

/// Synthesize the minimum vault layout `cairn mcp` needs:
/// `.cairn/vault.id` with a valid ULID and `.cairn/config.yaml` with
/// `mcp.stdio.single_tenant: true` plus a configured principal.
///
/// We deliberately do NOT call `cairn bootstrap` because its first-run
/// path fetches the embedding model from the network (~25 MB), which
/// makes the e2e fragile on cold CI caches and offline runners (round-1
/// review #3). The store auto-creates `.cairn/cairn.db` on first open
/// inside the subprocess, so this minimal layout is sufficient for the
/// `tools/list` assertion below.
fn synth_vault(tenant: &str) -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let cairn_dir = dir.path().join(".cairn");
    std::fs::create_dir_all(&cairn_dir).expect("mkdir .cairn");

    // ULID — 26 chars, Crockford base32 alphabet, monotonic over time.
    // Hand-fixed value to avoid a fresh dep just for this test; the
    // contents only need to parse as a valid ULID.
    let vault_id = "01J8WSKJ5T0R6XKYV5T2P4ZQVD";
    std::fs::write(cairn_dir.join("vault.id"), vault_id).expect("write vault.id");

    // Minimal config.yaml exercising the mcp.stdio path. Shape must
    // match what `cairn-core::config::CairnConfig` deserializes; keep
    // it small to avoid drift when unrelated config sections change.
    // `local_embeddings: false` disables the embedding-model probe
    // entirely so this test is offline-clean.
    let config = format!(
        "search:\n  \
           local_embeddings: false\n\
mcp:\n  \
  stdio:\n    \
    single_tenant: true\n    \
    principal:\n      \
      tenant: {tenant}\n"
    );
    std::fs::write(cairn_dir.join("config.yaml"), config).expect("write config.yaml");
    dir
}

#[test]
fn cairn_mcp_subprocess_advertises_handshake_per_wiring_flag() {
    let vault = synth_vault("e2e-cli");
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

    // Run the protocol with the child borrowed mutably; never moves the
    // handle out of the test scope so the post-protocol shutdown wait
    // below always has something to kill on deadline (round-2 review #1).
    run_protocol(&mut child);

    // Bounded shutdown wait. The child should exit a beat after stdin
    // EOF (relay drain + rmcp service teardown); poll `try_wait` so a
    // shutdown regression times out as a panic rather than wedging the
    // CI job.
    let deadline = Instant::now() + SHUTDOWN_DEADLINE;
    let timed_out = loop {
        match child.try_wait() {
            // Either exited cleanly (Some) or the wait itself failed
            // (Err) — both are terminal.
            Ok(Some(_)) | Err(_) => break false,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break true;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    drop(vault);
    assert!(
        !timed_out,
        "cairn mcp did not exit within {SHUTDOWN_DEADLINE:?} after stdin EOF"
    );
}

const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(15);

fn run_protocol(child: &mut Child) {
    let mut stdin = child.stdin.take().expect("child stdin");
    let stdout = child.stdout.take().expect("child stdout");
    let stderr = child.stderr.take().expect("child stderr");

    let (tx_out, rx_out) = mpsc::channel::<Value>();
    let _stdout_thread = std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        while reader.read_line(&mut line).unwrap_or(0) > 0 {
            if let Ok(v) = serde_json::from_str::<Value>(line.trim())
                && tx_out.send(v).is_err()
            {
                break;
            }
            line.clear();
        }
    });

    let stderr_collected = Arc::new(Mutex::new(String::new()));
    let stderr_clone = stderr_collected.clone();
    let _stderr_thread = std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut buf = String::new();
        while reader.read_line(&mut buf).unwrap_or(0) > 0 {
            stderr_clone.lock().expect("lock").push_str(&buf);
            buf.clear();
        }
    });
    let stderr_so_far = || stderr_collected.lock().expect("lock").clone();

    send_frame(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"cli-e2e","version":"0.0.0"}}}"#,
    );
    let init_resp = recv_frame(&rx_out, &stderr_so_far);
    assert_eq!(
        init_resp
            .pointer("/result/capabilities/experimental/cairn.status/contract")
            .and_then(Value::as_str),
        Some("cairn.mcp.v1"),
        "initialize must carry cairn.mcp.v1 contract; stderr: {}",
        stderr_so_far()
    );

    send_frame(
        &mut stdin,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    );

    send_frame(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    );
    let list_resp = recv_frame(&rx_out, &stderr_so_far);
    let names: Vec<&str> = list_resp
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .expect("tools array")
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect();

    if cairn_core::status::wiring::REPLAY_CHALLENGE_WIRED {
        assert!(
            names.contains(&"handshake"),
            "REPLAY_CHALLENGE_WIRED=true must list handshake over the CLI subprocess; got: {names:?}"
        );
    } else {
        assert!(
            !names.contains(&"handshake"),
            "REPLAY_CHALLENGE_WIRED=false must hide handshake (round-1 review #1); got: {names:?}"
        );
    }

    // Drop stdin so the server starts shutting down.
    drop(stdin);
}

fn send_frame(stdin: &mut impl Write, json: &str) {
    stdin.write_all(json.as_bytes()).expect("write frame");
    stdin.write_all(b"\n").expect("write newline");
    stdin.flush().expect("flush");
}

fn recv_frame(rx: &mpsc::Receiver<Value>, stderr_so_far: &dyn Fn() -> String) -> Value {
    rx.recv_timeout(Duration::from_secs(15))
        .unwrap_or_else(|e| {
            panic!(
                "timed out waiting for response frame: {e}\n--- captured stderr ---\n{}",
                stderr_so_far()
            )
        })
}
