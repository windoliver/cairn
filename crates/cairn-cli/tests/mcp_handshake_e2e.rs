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
//! Reliability properties (round-3 review #3):
//!  - The protocol helper returns `Result` instead of panicking, so the
//!    main test thread can always reach the cleanup path.
//!  - A `ChildGuard` owns the `Child` and force-kills + waits on drop, so
//!    no failure path can leak a subprocess.
//!  - The shutdown wait polls `try_wait` against an `Instant` deadline
//!    and asserts `ExitStatus::success()` — a non-zero exit after EOF
//!    fails the test instead of being silently accepted.
#![allow(missing_docs)]

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(15);
const FRAME_DEADLINE: Duration = Duration::from_secs(15);

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

    let vault_id = "01J8WSKJ5T0R6XKYV5T2P4ZQVD";
    std::fs::write(cairn_dir.join("vault.id"), vault_id).expect("write vault.id");

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

/// Outcome of the bounded shutdown wait below. Defined at module
/// scope so clippy's `items_after_statements` does not fire inside the
/// test body.
enum WaitOutcome {
    Exited(std::process::ExitStatus),
    WaitErr,
    TimedOut,
}

/// RAII guard that force-kills + waits on the wrapped `Child` when
/// dropped, unless explicitly released. Round-3 review #3: every
/// failure path in the test must reach this drop, so a stuck or
/// panicking subprocess cannot leak across CI runs.
struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self(Some(child))
    }
    fn as_mut(&mut self) -> &mut Child {
        self.0.as_mut().expect("child already taken")
    }
    fn release(mut self) -> Child {
        self.0.take().expect("child already taken")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.take() {
            // Best-effort cleanup; ignore errors. A child that already
            // exited returns Err from kill(), which is fine.
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[test]
fn cairn_mcp_subprocess_advertises_handshake_per_wiring_flag() {
    let vault = synth_vault("e2e-cli");
    let child = Command::new(cli_bin())
        .args(["--vault"])
        .arg(vault.path())
        .arg("mcp")
        .env_remove("CAIRN_VAULT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cairn mcp");
    let mut guard = ChildGuard::new(child);

    // Drive the protocol. Any failure path returns Err here; the
    // ChildGuard's Drop kills the subprocess on panic too.
    if let Err(reason) = run_protocol(guard.as_mut()) {
        // Force-kill before we panic so the assertion message contains
        // the captured stderr without the live subprocess holding
        // resources.
        let _ = guard.as_mut().kill();
        let _ = guard.as_mut().wait();
        drop(vault);
        panic!("protocol failed: {reason}");
    }

    // Bounded shutdown wait. The child should exit a beat after stdin
    // EOF (relay drain + rmcp service teardown); poll `try_wait` so a
    // shutdown regression times out as a panic rather than wedging
    // CI. ChildGuard cleans up on every exit path.
    let deadline = Instant::now() + SHUTDOWN_DEADLINE;
    let outcome = loop {
        match guard.as_mut().try_wait() {
            Ok(Some(status)) => break WaitOutcome::Exited(status),
            Err(_) => break WaitOutcome::WaitErr,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = guard.as_mut().kill();
                    let _ = guard.as_mut().wait();
                    break WaitOutcome::TimedOut;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    // Take the (already-waited) child out of the guard and explicitly
    // call `wait()` so clippy's `zombie_processes` lint sees a
    // syntactic wait on the `spawn()` result. After `try_wait`
    // returned Some/Err we know `wait()` cannot block; in the timeout
    // arm we already called `kill()+wait()` above.
    let mut released = guard.release();
    let _ = released.wait();
    drop(vault);

    match outcome {
        WaitOutcome::Exited(status) => assert!(
            status.success(),
            "cairn mcp must exit zero after stdin EOF; got {status}"
        ),
        WaitOutcome::WaitErr => panic!("wait on cairn mcp child failed"),
        WaitOutcome::TimedOut => {
            panic!("cairn mcp did not exit within {SHUTDOWN_DEADLINE:?} after stdin EOF")
        }
    }
}

/// Drive the wire protocol. Returns `Err(reason)` on any failure so the
/// caller's `ChildGuard` can clean up before propagating.
fn run_protocol(child: &mut Child) -> Result<(), String> {
    let mut stdin = child.stdin.take().ok_or("child stdin already taken")?;
    let stdout = child.stdout.take().ok_or("child stdout already taken")?;
    let stderr = child.stderr.take().ok_or("child stderr already taken")?;

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
    )?;
    let init_resp = recv_frame(&rx_out, &stderr_so_far)?;
    let contract = init_resp
        .pointer("/result/capabilities/experimental/cairn.status/contract")
        .and_then(Value::as_str);
    if contract != Some("cairn.mcp.v1") {
        return Err(format!(
            "initialize did not carry cairn.mcp.v1 contract; got {contract:?}\nstderr: {}",
            stderr_so_far()
        ));
    }

    send_frame(
        &mut stdin,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    )?;

    send_frame(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    )?;
    let list_resp = recv_frame(&rx_out, &stderr_so_far)?;
    let names: Vec<&str> = list_resp
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            format!(
                "tools/list response missing tools array: {list_resp}\nstderr: {}",
                stderr_so_far()
            )
        })?
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect();

    if cairn_core::status::wiring::REPLAY_CHALLENGE_WIRED {
        if !names.contains(&"handshake") {
            return Err(format!(
                "REPLAY_CHALLENGE_WIRED=true must list handshake; got: {names:?}"
            ));
        }
    } else if names.contains(&"handshake") {
        return Err(format!(
            "REPLAY_CHALLENGE_WIRED=false must hide handshake (round-1 review #1); got: {names:?}"
        ));
    }

    // Drop stdin so the server starts shutting down.
    drop(stdin);
    Ok(())
}

fn send_frame(stdin: &mut impl Write, json: &str) -> Result<(), String> {
    stdin
        .write_all(json.as_bytes())
        .map_err(|e| format!("write frame: {e}"))?;
    stdin
        .write_all(b"\n")
        .map_err(|e| format!("write nl: {e}"))?;
    stdin.flush().map_err(|e| format!("flush: {e}"))?;
    Ok(())
}

fn recv_frame(
    rx: &mpsc::Receiver<Value>,
    stderr_so_far: &dyn Fn() -> String,
) -> Result<Value, String> {
    rx.recv_timeout(FRAME_DEADLINE).map_err(|e| {
        format!(
            "timed out waiting for response frame: {e}\n--- captured stderr ---\n{}",
            stderr_so_far()
        )
    })
}
