//! End-to-end CLI tests for the `cairn mcp` stdio transport.
//!
//! Spawns a real `cairn mcp` subprocess against a synthesized vault
//! (no `cairn bootstrap`, so no embedding-model download - round-1 review
//! #3) and drives JSON-RPC over the child's stdin/stdout.
//!
//! For the `handshake` MCP prelude tool (issue #65), this asserts the
//! production posture:
//!
//!  - When [`cairn_core::status::wiring::REPLAY_CHALLENGE_WIRED`] is
//!    `false` (current state), `handshake` is absent from `tools/list`
//!    even though the sqlite store is wired (brief §15 fail-closed,
//!    round-1 review #1).
//!  - When the flag is `true` (future state), `handshake` is present.
//!
//! For typed core verb envelopes (issue #66), this asserts that actual
//! CLI-served `tools/call` responses carry generated `Response` JSON for
//! malformed known verbs and known-but-unwired valid verbs.
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
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cairn_core::generated::envelope::{Response, ResponseStatus, ResponseVerb};
use cairn_mcp::generated::TOOLS;
use cairn_mcp::graph_tools::GRAPH_TOOLS;
use serde_json::Value;
use tempfile::TempDir;

const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(15);
const FRAME_DEADLINE: Duration = Duration::from_secs(90);

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
    std::fs::write(dir.path().join("purpose.md"), "mcp e2e purpose").expect("write purpose.md");
    std::fs::write(dir.path().join("index.md"), "mcp e2e index").expect("write index.md");
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
    run_mcp_subprocess("e2e-cli", run_tools_list_protocol);
}

#[test]
fn cairn_mcp_subprocess_returns_typed_envelopes_for_core_verb_calls() {
    run_mcp_subprocess("e2e-cli-typed", run_typed_envelope_protocol);
}

#[test]
fn cairn_mcp_subprocess_assemble_hot_commits_prefix() {
    run_mcp_subprocess("e2e-cli-assemble", run_assemble_hot_protocol);
}

#[test]
fn cairn_mcp_subprocess_lists_unique_core_verbs_and_expected_graph_tools() {
    run_mcp_subprocess("e2e-cli-graph", run_graph_tools_protocol);
}

#[test]
fn cairn_mcp_subprocess_advertises_generated_tool_descriptions_byte_for_byte() {
    run_mcp_subprocess("e2e-cli-descriptions", run_tool_description_protocol);
}

#[test]
fn cairn_mcp_subprocess_ingest_search_forget_round_trip() {
    run_mcp_subprocess("e2e-cli-mutations", run_mutation_round_trip_protocol);
}

fn run_mcp_subprocess(tenant: &str, protocol: fn(&mut Child) -> Result<(), String>) {
    let vault = synth_vault(tenant);
    let child = Command::new(cli_bin())
        .args(["--vault"])
        .arg(vault.path())
        .arg("mcp")
        .env_remove("CAIRN_VAULT")
        // Offline file keystore: the signed-ingest path (round-trip test)
        // provisions an identity on first ingest. The default OS keychain
        // serializes through a system daemon, which under full-workspace
        // `nextest` parallelism stalls the subprocess past FRAME_DEADLINE.
        // The file keystore is deterministic, offline, and matches CI's
        // ubuntu config (`CAIRN_KEYSTORE: file`). Harmless for the
        // read-only protocols.
        .env("CAIRN_KEYSTORE", "file")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cairn mcp");
    let mut guard = ChildGuard::new(child);

    // Drive the protocol. Any failure path returns Err here; the
    // ChildGuard's Drop kills the subprocess on panic too.
    if let Err(reason) = protocol(guard.as_mut()) {
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

/// Drive `initialize` + `tools/list`. Returns `Err(reason)` on any failure so
/// the caller's `ChildGuard` can clean up before propagating.
fn run_tools_list_protocol(child: &mut Child) -> Result<(), String> {
    let mut client = ProtocolClient::new(child)?;

    let init_resp = client.initialize("cli-e2e")?;
    let contract = init_resp
        .pointer("/result/capabilities/experimental/cairn.status/contract")
        .and_then(Value::as_str);
    if contract != Some("cairn.mcp.v1") {
        return Err(format!(
            "initialize did not carry cairn.mcp.v1 contract; got {contract:?}\nstderr: {}",
            client.stderr_so_far()
        ));
    }

    client.send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#)?;
    let list_resp = client.recv("tools/list")?;
    let names: Vec<&str> = list_resp
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            format!(
                "tools/list response missing tools array: {list_resp}\nstderr: {}",
                client.stderr_so_far()
            )
        })?
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect();
    let missing: Vec<&str> = TOOLS
        .iter()
        .filter(|tool| {
            !cairn_mcp::federation_tools::is_federation_tool(tool.name)
                && !cairn_mcp::admin_tools::is_admin_tool(tool.name)
        })
        .map(|tool| tool.name)
        .filter(|name| !names.contains(name))
        .collect();

    if !missing.is_empty() {
        return Err(format!(
            "tools/list over real stdio must include every generated core verb; missing {missing:?} from {names:?}"
        ));
    }

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
    client.close_stdin();
    Ok(())
}

fn run_typed_envelope_protocol(child: &mut Child) -> Result<(), String> {
    let mut client = ProtocolClient::new(child)?;
    client.initialize("cli-e2e-typed")?;

    client.send(
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search","arguments":{"query":"hello"}}}"#,
    )?;
    let invalid_search = client.recv("invalid search args")?;
    let invalid_search = typed_error_envelope(
        &invalid_search,
        "invalid search args",
        ResponseVerb::Search,
        ResponseStatus::Rejected,
        "InvalidArgs",
    )?;
    let err = invalid_search
        .error
        .as_ref()
        .expect("invalid search args must carry error");
    if err["data"]["field"] != "args" {
        return Err(format!(
            "invalid search args must identify args field; got {err}"
        ));
    }

    client.send(
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"retrieve","arguments":{"target":"record"}}}"#,
    )?;
    let invalid_retrieve = client.recv("invalid retrieve args")?;
    let invalid_retrieve = typed_error_envelope(
        &invalid_retrieve,
        "invalid retrieve args",
        ResponseVerb::Retrieve,
        ResponseStatus::Rejected,
        "InvalidArgs",
    )?;
    let err = invalid_retrieve
        .error
        .as_ref()
        .expect("invalid retrieve args must carry error");
    let reason = err["data"]["reason"].as_str().unwrap_or_default();
    if err["data"]["field"] != "args" || !reason.contains("missing field") {
        return Err(format!(
            "invalid retrieve args must identify missing args field; got {err}"
        ));
    }

    client.send(
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"retrieve","arguments":{"target":"record","id":"01ARZ3NDEKTSV4RRFFQ69G5FAV"}}}"#,
    )?;
    let unwired_retrieve = client.recv("valid unwired retrieve")?;
    let unwired_retrieve = typed_error_envelope(
        &unwired_retrieve,
        "valid unwired retrieve",
        ResponseVerb::Retrieve,
        ResponseStatus::Aborted,
        "Internal",
    )?;
    let err = unwired_retrieve
        .error
        .as_ref()
        .expect("unwired retrieve must carry error");
    if !err["message"]
        .as_str()
        .unwrap_or_default()
        .contains("not yet implemented")
    {
        return Err(format!(
            "unwired retrieve must explain missing MCP implementation; got {err}"
        ));
    }

    // Drop stdin so the server starts shutting down.
    client.close_stdin();
    Ok(())
}

fn run_assemble_hot_protocol(child: &mut Child) -> Result<(), String> {
    let mut client = ProtocolClient::new(child)?;
    client.initialize("cli-e2e-assemble")?;

    client.send(
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"assemble_hot","arguments":{"budget":64}}}"#,
    )?;
    let call_resp = client.recv("assemble_hot")?;
    let is_error = call_resp
        .pointer("/result/isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if is_error {
        return Err(format!("assemble_hot should commit; got {call_resp}"));
    }

    let text = first_text_content(&call_resp)
        .ok_or_else(|| format!("assemble_hot response missing text content: {call_resp}"))?;
    let envelope: Value = serde_json::from_str(text)
        .map_err(|e| format!("assemble_hot text must parse as JSON: {e}; text={text}"))?;
    if envelope["contract"] != "cairn.mcp.v1"
        || envelope["verb"] != "assemble_hot"
        || envelope["status"] != "committed"
    {
        return Err(format!(
            "assemble_hot must return committed cairn envelope; got {envelope}"
        ));
    }
    let prefix = envelope
        .pointer("/data/prefix")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !prefix.contains("mcp e2e purpose") {
        return Err(format!(
            "assemble_hot prefix should include purpose.md content; got {envelope}"
        ));
    }
    if envelope
        .pointer("/data/bytes")
        .and_then(Value::as_u64)
        .is_none_or(|bytes| bytes > 64)
    {
        return Err(format!(
            "assemble_hot bytes should respect requested budget; got {envelope}"
        ));
    }
    if !envelope
        .pointer("/data/segments")
        .is_some_and(Value::is_array)
    {
        return Err(format!("assemble_hot must emit segments; got {envelope}"));
    }

    client.close_stdin();
    Ok(())
}

/// Committed-envelope helper for the mutation round-trip: assert the call
/// is a non-error result whose text content parses as a committed cairn
/// envelope for `verb`, and return the parsed envelope JSON.
fn committed_envelope(call_resp: &Value, label: &str, verb: &str) -> Result<Value, String> {
    let is_error = call_resp
        .pointer("/result/isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if is_error {
        return Err(format!("{label} should commit; got {call_resp}"));
    }
    let text = first_text_content(call_resp)
        .ok_or_else(|| format!("{label} response missing text content: {call_resp}"))?;
    let envelope: Value = serde_json::from_str(text)
        .map_err(|e| format!("{label} text must parse as JSON: {e}; text={text}"))?;
    if envelope["contract"] != "cairn.mcp.v1"
        || envelope["verb"] != verb
        || envelope["status"] != "committed"
    {
        return Err(format!(
            "{label} must return a committed cairn envelope; got {envelope}"
        ));
    }
    Ok(envelope)
}

/// Full mutating round-trip over the real `cairn mcp` subprocess —
/// the exact flow the desk-daemon `CairnMemoryAdapter` drives:
/// `ingest` commits a record, `search` finds it, `forget` tombstones it,
/// and a second `search` comes back empty.
fn run_mutation_round_trip_protocol(child: &mut Child) -> Result<(), String> {
    let mut client = ProtocolClient::new(child)?;
    client.initialize("cli-e2e-mutations")?;

    // ── ingest ───────────────────────────────────────────────────────────
    client.send(
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"ingest","arguments":{"kind":"reference","body":"Acme renewal terms land in Q3.","frontmatter":{"source":"e2e"}}}}"#,
    )?;
    let ingest_env = committed_envelope(&client.recv("ingest")?, "ingest", "ingest")?;
    let record_id = ingest_env
        .pointer("/data/record_id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("ingest envelope missing data.record_id: {ingest_env}"))?
        .to_owned();
    if record_id.len() != 26 {
        return Err(format!("ingest record_id must be a ULID; got {record_id}"));
    }

    // ── search round-trip ────────────────────────────────────────────────
    client.send(
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search","arguments":{"query":"acme","mode":"keyword","limit":10}}}"#,
    )?;
    let search_env = committed_envelope(&client.recv("search")?, "search", "search")?;
    let hit_ids: Vec<&str> = search_env
        .pointer("/data/hits")
        .and_then(Value::as_array)
        .map(|hits| {
            hits.iter()
                .filter_map(|h| h.get("record_id").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    if !hit_ids.contains(&record_id.as_str()) {
        return Err(format!(
            "keyword search must find the ingested record {record_id}; got hits {hit_ids:?} in {search_env}"
        ));
    }

    // ── forget ───────────────────────────────────────────────────────────
    let forget_call = format!(
        r#"{{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{{"name":"forget","arguments":{{"mode":"record","record_id":"{record_id}"}}}}}}"#
    );
    client.send(&forget_call)?;
    let forget_env = committed_envelope(&client.recv("forget")?, "forget", "forget")?;
    if forget_env
        .pointer("/data/deleted_count")
        .and_then(Value::as_u64)
        .is_none_or(|n| n < 1)
    {
        return Err(format!(
            "forget must report at least one deleted version; got {forget_env}"
        ));
    }

    // ── search again: record is gone ─────────────────────────────────────
    client.send(
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"search","arguments":{"query":"acme","mode":"keyword","limit":10}}}"#,
    )?;
    let search_env = committed_envelope(&client.recv("search-after-forget")?, "search", "search")?;
    let hits_after = search_env
        .pointer("/data/hits")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    if hits_after != 0 {
        return Err(format!(
            "search after forget must return no hits; got {search_env}"
        ));
    }

    client.close_stdin();
    Ok(())
}

fn run_graph_tools_protocol(child: &mut Child) -> Result<(), String> {
    let mut client = ProtocolClient::new(child)?;
    client.initialize("cli-e2e-graph")?;

    client.send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#)?;
    let list_resp = client.recv("tools/list graph tools")?;
    let names: Vec<&str> = list_resp
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            format!(
                "tools/list response missing tools array: {list_resp}\nstderr: {}",
                client.stderr_so_far()
            )
        })?
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect();

    for core in TOOLS
        .iter()
        .filter(|tool| {
            !cairn_mcp::federation_tools::is_federation_tool(tool.name)
                && !cairn_mcp::admin_tools::is_admin_tool(tool.name)
        })
        .map(|tool| tool.name)
    {
        let count = names.iter().filter(|name| **name == core).count();
        if count != 1 {
            return Err(format!(
                "core verb {core} must appear exactly once in tools/list; got count={count} from {names:?}"
            ));
        }
    }

    let mut expected_graph: Vec<&str> = GRAPH_TOOLS.iter().map(|tool| tool.name).collect();
    expected_graph.sort_unstable();
    let mut actual_graph: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| name.starts_with("graph."))
        .collect();
    actual_graph.sort_unstable();

    if actual_graph != expected_graph {
        return Err(format!(
            "single-tenant stdio tools/list must advertise the expected graph tool set; got {actual_graph:?}, expected {expected_graph:?}"
        ));
    }

    client.close_stdin();
    Ok(())
}

fn run_tool_description_protocol(child: &mut Child) -> Result<(), String> {
    let mut client = ProtocolClient::new(child)?;
    client.initialize("cli-e2e-descriptions")?;

    client.send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#)?;
    let list_resp = client.recv("tools/list descriptions")?;
    let tools = list_resp
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            format!(
                "tools/list response missing tools array: {list_resp}\nstderr: {}",
                client.stderr_so_far()
            )
        })?;

    for decl in TOOLS.iter().filter(|d| {
        !cairn_mcp::federation_tools::is_federation_tool(d.name)
            && !cairn_mcp::admin_tools::is_admin_tool(d.name)
    }) {
        let advertised = tools
            .iter()
            .find(|tool| tool.get("name").and_then(Value::as_str) == Some(decl.name))
            .ok_or_else(|| format!("tools/list omitted {}", decl.name))?;
        let description = advertised.get("description").and_then(Value::as_str);
        if description != Some(decl.description) {
            return Err(format!(
                "real cairn mcp subprocess description drifted for {}; got {description:?}, expected {:?}",
                decl.name, decl.description
            ));
        }
        let description = description.expect("checked Some above");
        if description.contains('\n') || description.trim() != description {
            return Err(format!(
                "real cairn mcp subprocess description for {} must be one trimmed paragraph; got {description:?}",
                decl.name
            ));
        }
    }

    client.close_stdin();
    Ok(())
}

struct ProtocolClient {
    stdin: Option<ChildStdin>,
    rx_out: mpsc::Receiver<Value>,
    stderr_collected: Arc<Mutex<String>>,
}

impl ProtocolClient {
    fn new(child: &mut Child) -> Result<Self, String> {
        let stdin = child.stdin.take().ok_or("child stdin already taken")?;
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

        Ok(Self {
            stdin: Some(stdin),
            rx_out,
            stderr_collected,
        })
    }

    fn send(&mut self, json: &str) -> Result<(), String> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| "child stdin already closed".to_owned())?;
        send_frame(stdin, json)
    }

    fn recv(&self, label: &str) -> Result<Value, String> {
        recv_frame(label, &self.rx_out, &|| self.stderr_so_far())
    }

    fn initialize(&mut self, client_name: &str) -> Result<Value, String> {
        let init = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {
                    "name": client_name,
                    "version": "0.0.0",
                },
            },
        });
        self.send(&init.to_string())?;
        let resp = self.recv("initialize")?;
        self.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)?;
        Ok(resp)
    }

    fn close_stdin(&mut self) {
        self.stdin.take();
    }

    fn stderr_so_far(&self) -> String {
        self.stderr_collected.lock().expect("lock").clone()
    }
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
    label: &str,
    rx: &mpsc::Receiver<Value>,
    stderr_so_far: &dyn Fn() -> String,
) -> Result<Value, String> {
    rx.recv_timeout(FRAME_DEADLINE).map_err(|e| {
        format!(
            "timed out waiting for {label} response frame: {e}\n--- captured stderr ---\n{}",
            stderr_so_far()
        )
    })
}

fn typed_error_envelope(
    call_resp: &Value,
    label: &str,
    expected_verb: ResponseVerb,
    expected_status: ResponseStatus,
    expected_code: &str,
) -> Result<Response, String> {
    if call_resp.get("result").is_none() {
        return Err(format!(
            "{label} must yield a result frame; got {call_resp}"
        ));
    }
    let is_error = call_resp
        .pointer("/result/isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !is_error {
        return Err(format!("{label} must set isError=true; got {call_resp}"));
    }
    let text = first_text_content(call_resp)
        .ok_or_else(|| format!("{label} response must include text content; got {call_resp}"))?;
    let envelope: Response = serde_json::from_str(text)
        .map_err(|e| format!("{label} text must parse as Response JSON: {e}; text={text}"))?;
    if envelope.contract != "cairn.mcp.v1" {
        return Err(format!(
            "{label} must carry cairn.mcp.v1 contract; got {}",
            envelope.contract
        ));
    }
    if envelope.status != expected_status {
        return Err(format!(
            "{label} status mismatch; expected {:?}, got {:?}",
            expected_status, envelope.status
        ));
    }
    if envelope.verb != expected_verb {
        return Err(format!(
            "{label} verb mismatch; expected {:?}, got {:?}",
            expected_verb, envelope.verb
        ));
    }
    if envelope.operation_id.0.len() != 26 {
        return Err(format!(
            "{label} operation_id must be a ULID; got {}",
            envelope.operation_id.0
        ));
    }
    if envelope.data.is_some() {
        return Err(format!("{label} error envelope must not carry data"));
    }
    let err = envelope
        .error
        .as_ref()
        .ok_or_else(|| format!("{label} must carry error object"))?;
    if err["code"] != expected_code {
        return Err(format!(
            "{label} code mismatch; expected {expected_code}, got {err}"
        ));
    }
    Ok(envelope)
}

fn first_text_content(call_resp: &Value) -> Option<&str> {
    call_resp
        .pointer("/result/content")
        .and_then(Value::as_array)?
        .first()?
        .get("text")
        .and_then(Value::as_str)
}
