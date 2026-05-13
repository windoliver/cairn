//! End-to-end CLI test for the rolling-summary `ConsolidationWorkflow`
//! (issue #90). Drives the actual `cairn` binary via `assert_cmd`:
//!
//! 1. `cairn bootstrap` a temp vault.
//! 2. Generate a valid `CaptureEvent` JSONL covering 6 turns × 4 events.
//! 3. `cairn capture_trace --from <jsonl>` — should produce 6 turn-summary
//!    records and enqueue one `consolidation.rolling_summary` job.
//! 4. Build a `Scheduler` programmatically against the live `.cairn/cairn.db`
//!    and let it drain the queue.
//! 5. Verify a `reasoning`-kind record materializes for the session.
//! 6. `cairn forget --record <source-turn-id>` — should tombstone the source
//!    and enqueue a `consolidation.forget_cleanup` job.
//! 7. Drain again, verify the rolling summary is tombstoned.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use assert_cmd::cargo::CommandCargoExt as _;
use cairn_core::config::ConsolidationConfig;
use cairn_core::contract::job_store::JobStore;
use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::domain::taxonomy::MemoryKind;
use cairn_core::domain::{
    ActorChainEntry, CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, CaptureRefs,
    ChainRole, Identity, PayloadHash, Rfc3339Timestamp, SourceFamily,
};
use cairn_workflows::SqliteJobStore;
use cairn_workflows::consolidation::{ConsolidationForgetCleanupHandler, ConsolidationHandler};
use cairn_workflows::scheduler::{
    Clock, HandlerRegistryBuilder, Scheduler, SchedulerConfig, SystemClock,
};
use sha2::{Digest as _, Sha256};

const SESSION_ULID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

fn write_source_file(vault: &Path, family: &str, filename: &str, content: &str) -> String {
    let dir = vault.join("sources").join(family);
    std::fs::create_dir_all(&dir).expect("mkdir sources family");
    let abs = dir.join(filename);
    std::fs::write(&abs, content).expect("write source");
    format!("sources/{family}/{filename}")
}

#[allow(
    clippy::too_many_arguments,
    reason = "test helper mirrors CaptureEvent fields"
)]
fn make_hook_event(
    event_id: &str,
    hook_name: &str,
    session_id: &str,
    turn_id: &str,
    timestamp: &str,
    tool_id: Option<String>,
    payload_ref: &str,
    payload_hash_hex: &str,
) -> CaptureEvent {
    let sensor =
        Identity::parse("snr:local:hook:cc-session:v1").expect("invariant: valid sensor id");
    let hash_str = format!("sha256:{payload_hash_hex}");
    CaptureEvent {
        event_id: CaptureEventId::parse(event_id).expect("invariant: valid ULID"),
        sensor_id: sensor.clone(),
        capture_mode: CaptureMode::Auto,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: sensor,
            at: Rfc3339Timestamp::parse(timestamp).expect("invariant: valid RFC-3339"),
        }],
        refs: Some(CaptureRefs {
            session_id: Some(session_id.to_owned()),
            turn_id: Some(turn_id.to_owned()),
            tool_id,
        }),
        payload_hash: PayloadHash::parse(&hash_str).expect("invariant: valid sha256"),
        payload_ref: payload_ref.to_owned(),
        captured_at: Rfc3339Timestamp::parse(timestamp).expect("invariant: valid RFC-3339"),
        payload: CapturePayload::Hook {
            hook_name: hook_name.to_owned(),
            tool_name: None,
        },
        source_family: SourceFamily::Hook,
    }
}

/// Build N turns × 4 events (`UserPromptSubmit` → `PreToolUse` → `PostToolUse` → `Stop`).
/// Returns JSONL bytes. ULID counter starts at `ulid_offset`.
fn write_multi_turn_jsonl(vault: &Path, jsonl_path: &Path, turns: u32) {
    // Crockford base32 alphabet (no I, L, O, U). Build deterministic ULIDs by
    // appending two base32 chars per event index.
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut events: Vec<CaptureEvent> = Vec::new();
    for turn in 1..=turns {
        let turn_id = format!("turn-{turn}");
        let tool_id = format!("toolu_t{turn}");
        for (j, hook) in ["UserPromptSubmit", "PreToolUse", "PostToolUse", "Stop"]
            .iter()
            .enumerate()
        {
            // Use turn and j to build a unique ULID suffix.
            let idx = (turn - 1) as usize * 4 + j;
            let a = ALPHABET[idx / ALPHABET.len() % ALPHABET.len()] as char;
            let b = ALPHABET[idx % ALPHABET.len()] as char;
            let event_id = format!("01ARZ3NDEKTSV4RRFFQ69G5F{a}{b}");
            let timestamp = format!("2026-05-12T0{turn}:0{j}:00Z");
            let body = format!("turn-{turn} {hook} body");
            let payload_ref = write_source_file(vault, "hook", &format!("{event_id}.txt"), &body);
            let tool = if *hook == "PreToolUse" || *hook == "PostToolUse" {
                Some(tool_id.clone())
            } else {
                None
            };
            events.push(make_hook_event(
                &event_id,
                hook,
                SESSION_ULID,
                &turn_id,
                &timestamp,
                tool,
                &payload_ref,
                &sha256_hex(body.as_bytes()),
            ));
        }
    }

    let mut f = std::fs::File::create(jsonl_path).expect("create jsonl");
    for ev in &events {
        let line = serde_json::to_string(ev).expect("serialize event");
        writeln!(f, "{line}").expect("write line");
    }
}

fn cairn_bin() -> Command {
    Command::cargo_bin("cairn").expect("locate cairn binary")
}

/// What the predicate is looking for, evaluated under async.
enum WaitFor {
    /// A `Reasoning`-kind record exists for the session.
    ReasoningRecordPresent,
    /// `get(summary_id)` returns `Ok(None)` (tombstoned).
    SummaryTombstoned {
        summary_id: cairn_core::domain::RecordId,
    },
}

/// Spin up an in-process scheduler against the live vault DB, run for up to
/// `max_wait`, then shut down. Returns once the predicate becomes true or
/// the deadline expires. Returns the (memory store, success) pair so the
/// caller can do final assertions without re-opening.
async fn drain_until(
    vault_root: &Path,
    max_wait: Duration,
    target: WaitFor,
) -> Arc<cairn_store_sqlite::SqliteMemoryStore> {
    let db_path = vault_root.join(".cairn/cairn.db");
    let store = Arc::new(
        cairn_store_sqlite::open(&db_path)
            .await
            .expect("open memory store"),
    );
    let jobs_conn = cairn_store_sqlite::open_sync(&db_path).expect("open jobs conn");
    let jobs: Arc<dyn JobStore> = Arc::new(SqliteJobStore::new(jobs_conn).expect("job store"));
    let cfg = ConsolidationConfig::default();
    let registry = HandlerRegistryBuilder::default()
        .with(Arc::new(ConsolidationHandler::with_job_store(
            store.clone(),
            cfg,
            jobs.clone(),
        )))
        .with(Arc::new(ConsolidationForgetCleanupHandler::new(
            store.clone(),
        )))
        .build();
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let scheduler = Scheduler::start(
        "e2e-inc",
        jobs.clone(),
        &registry,
        clock,
        SchedulerConfig::p0(),
    );

    let start = std::time::Instant::now();
    while start.elapsed() < max_wait {
        let done = match &target {
            WaitFor::ReasoningRecordPresent => !read_reasoning_records(&store).await.is_empty(),
            WaitFor::SummaryTombstoned { summary_id } => {
                matches!(store.get(summary_id).await, Ok(None))
            }
        };
        if done {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    scheduler.shutdown().await;
    store
}

fn count_workflow_jobs(db_path: &Path, kind: &str) -> i64 {
    let conn = rusqlite::Connection::open(db_path).expect("conn");
    conn.query_row(
        "SELECT COUNT(*) FROM workflow_jobs WHERE kind = ?1",
        rusqlite::params![kind],
        |row| row.get::<_, i64>(0),
    )
    .expect("count jobs")
}

async fn read_reasoning_records(
    store: &cairn_store_sqlite::SqliteMemoryStore,
) -> Vec<cairn_core::domain::record::MemoryRecord> {
    let args = cairn_core::contract::memory_store::ListArgs {
        limit: 100,
        ..Default::default()
    };
    let page = store.list(&args).await.expect("list");
    page.records
        .into_iter()
        .filter(|r| r.kind == MemoryKind::Reasoning)
        .collect()
}

async fn read_turn_summary_record_ids(
    store: &cairn_store_sqlite::SqliteMemoryStore,
) -> Vec<String> {
    store
        .list_trace_turns(SESSION_ULID, 0, 100)
        .await
        .expect("list_trace_turns")
        .into_iter()
        .map(|h| h.record_id)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_capture_trace_then_forget_propagates_to_summary() {
    let vault = tempfile::tempdir().expect("tempdir");
    let vault_root = vault.path();

    // ── Step 1: bootstrap ───────────────────────────────────────────────────
    let out = cairn_bin()
        .arg("bootstrap")
        .arg("--vault-path")
        .arg(vault_root)
        .arg("--json")
        .output()
        .expect("spawn cairn bootstrap");
    assert!(
        out.status.success(),
        "bootstrap failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // ── Step 2: generate JSONL with 6 turns ─────────────────────────────────
    let jsonl_path: PathBuf = vault_root.join("trace.jsonl");
    write_multi_turn_jsonl(vault_root, &jsonl_path, 6);
    let lines = std::fs::read_to_string(&jsonl_path).expect("read jsonl");
    assert_eq!(
        lines.lines().count(),
        24,
        "should be 6 turns × 4 events = 24 lines"
    );

    // ── Step 3: capture_trace ───────────────────────────────────────────────
    let out = cairn_bin()
        .arg("capture_trace")
        .arg("--from")
        .arg(&jsonl_path)
        .arg("--vault")
        .arg(vault_root)
        .arg("--json")
        .output()
        .expect("spawn cairn capture_trace");
    assert!(
        out.status.success(),
        "capture_trace failed: stderr={}\nstdout={}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );

    // ── Step 4: verify a consolidation job was enqueued ─────────────────────
    let db_path = vault_root.join(".cairn/cairn.db");
    let queued = count_workflow_jobs(&db_path, "consolidation.rolling_summary");
    assert!(
        queued >= 1,
        "expected at least one consolidation.rolling_summary job, got {queued}"
    );

    // Also confirm the turn summary records are in place.
    let store_for_check = cairn_store_sqlite::open(&db_path)
        .await
        .expect("open store");
    let turn_ids = read_turn_summary_record_ids(&store_for_check).await;
    assert_eq!(turn_ids.len(), 6, "should have 6 turn-summary records");
    drop(store_for_check);

    // ── Step 5: drain scheduler until a reasoning record appears ────────────
    let store_after_drain = drain_until(
        vault_root,
        Duration::from_secs(15),
        WaitFor::ReasoningRecordPresent,
    )
    .await;
    let reasoning = read_reasoning_records(&store_after_drain).await;
    assert!(
        !reasoning.is_empty(),
        "scheduler should have produced a reasoning record within 15s"
    );
    let summary_id = reasoning[0].id.clone();
    drop(store_after_drain);

    // ── Step 6: forget one of the source turns ──────────────────────────────
    let forget_target = turn_ids.first().expect("at least one turn summary").clone();
    let out = cairn_bin()
        .arg("forget")
        .arg("--record")
        .arg(&forget_target)
        .arg("--vault")
        .arg(vault_root)
        .arg("--json")
        .output()
        .expect("spawn cairn forget");
    assert!(
        out.status.success(),
        "forget failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // ── Step 7: verify forget_cleanup job was enqueued ──────────────────────
    let cleanup_count = count_workflow_jobs(&db_path, "consolidation.forget_cleanup");
    assert!(
        cleanup_count >= 1,
        "expected forget_cleanup job, got {cleanup_count}"
    );

    // ── Step 8: drain scheduler, verify summary is tombstoned ───────────────
    let store_after_drain2 = drain_until(
        vault_root,
        Duration::from_secs(15),
        WaitFor::SummaryTombstoned {
            summary_id: summary_id.clone(),
        },
    )
    .await;
    let still_alive = store_after_drain2
        .get(&summary_id)
        .await
        .expect("get summary");
    assert!(
        still_alive.is_none(),
        "summary should be tombstoned after forget propagation"
    );
}

/// Companion test: validate that `cairn mcp serve` (subprocess) boots the
/// scheduler and drains queued consolidation jobs. The first test above
/// runs the scheduler programmatically; this one closes the loop and
/// proves the production code path (Task 17 wiring) actually works when
/// spawned the way an operator would spawn it.
///
/// Flow:
///   1. Bootstrap a real vault.
///   2. Edit `.cairn/config.yaml` to enable `single_tenant + principal`
///      (the gate that boots the scheduler inside `cairn mcp`).
///   3. Run `cairn capture_trace --from <jsonl>` to enqueue a
///      consolidation job.
///   4. Spawn `cairn mcp serve` as a real subprocess.
///   5. Poll the DB until a `reasoning` record materializes (proves the
///      scheduler leased the job and the handler ran successfully).
///   6. Close stdin so the subprocess shuts down cleanly.
///   7. Assert the summary exists and links back to its source turns.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "linear e2e test reads naturally without extraction"
)]
async fn cairn_mcp_serve_subprocess_drains_queued_consolidation_job() {
    use std::process::{Command, Stdio};
    use std::time::Instant;

    let vault = tempfile::tempdir().expect("tempdir");
    let vault_root = vault.path();

    // ── Step 1: bootstrap a real vault (writes config.yaml + vault.id) ─────
    let out = cairn_bin()
        .arg("bootstrap")
        .arg("--vault-path")
        .arg(vault_root)
        .arg("--json")
        .output()
        .expect("spawn cairn bootstrap");
    assert!(
        out.status.success(),
        "bootstrap failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // ── Step 2: enable single_tenant in config.yaml ────────────────────────
    let config_path = vault_root.join(".cairn/config.yaml");
    let mut cfg = std::fs::read_to_string(&config_path).expect("read config");
    // Bootstrap writes an `mcp:` block with `stdio:` underneath whose
    // single_tenant defaults to false. Replace the whole block with a
    // single_tenant=true version + a principal.
    if cfg.contains("mcp:") {
        // Replace the existing block. The bootstrap config has known shape;
        // append a complete mcp block at the end and let the later block
        // win (yaml_serde::from_str takes the last duplicate). Simpler:
        // strip the existing block by truncating the file at the first
        // "\nmcp:" line and re-append our own.
        if let Some(idx) = cfg.find("\nmcp:") {
            cfg.truncate(idx);
        }
    }
    cfg.push_str(
        "\nmcp:\n  stdio:\n    single_tenant: true\n    principal:\n      tenant: e2e-mcp\n",
    );
    std::fs::write(&config_path, cfg).expect("write config");

    // ── Step 3: generate JSONL + run capture_trace to enqueue a job ────────
    let jsonl_path = vault_root.join("trace.jsonl");
    write_multi_turn_jsonl(vault_root, &jsonl_path, 6);
    let out = cairn_bin()
        .arg("capture_trace")
        .arg("--from")
        .arg(&jsonl_path)
        .arg("--vault")
        .arg(vault_root)
        .arg("--json")
        .output()
        .expect("spawn cairn capture_trace");
    assert!(
        out.status.success(),
        "capture_trace failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let db_path = vault_root.join(".cairn/cairn.db");
    let queued = count_workflow_jobs(&db_path, "consolidation.rolling_summary");
    assert!(
        queued >= 1,
        "expected at least one consolidation job before mcp serve"
    );

    // ── Step 4: spawn `cairn mcp serve` as a real subprocess ───────────────
    let mut child = Command::new(env!("CARGO_BIN_EXE_cairn"))
        .arg("--vault")
        .arg(vault_root)
        .arg("mcp")
        .env_remove("CAIRN_VAULT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cairn mcp");
    let child_stdin = child.stdin.take().expect("stdin pipe");

    // ── Step 5: poll DB until a reasoning record appears ───────────────────
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut reasoning_record_id: Option<cairn_core::domain::RecordId> = None;
    while Instant::now() < deadline {
        // Open via the sync helper so we don't fight tokio-rusqlite's pool
        // while the subprocess is also touching the DB.
        let Ok(conn) = rusqlite::Connection::open(&db_path) else {
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        };
        let row: rusqlite::Result<String> = conn.query_row(
            "SELECT record_id FROM records
             WHERE kind = 'reasoning' AND active = 1 AND tombstoned = 0
             LIMIT 1",
            [],
            |r| r.get(0),
        );
        drop(conn);
        if let Ok(id) = row {
            reasoning_record_id =
                Some(cairn_core::domain::RecordId::parse(&id).expect("parse record_id"));
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // ── Step 6: shut down `cairn mcp` cleanly ──────────────────────────────
    // Drop stdin to signal EOF; the subprocess should exit on its own.
    drop(child_stdin);
    let shutdown_deadline = Instant::now() + Duration::from_secs(15);
    let mut exited = false;
    while Instant::now() < shutdown_deadline {
        match child.try_wait() {
            Ok(Some(_)) => {
                exited = true;
                break;
            }
            Ok(None) => tokio::time::sleep(Duration::from_millis(50)).await,
            Err(_) => break,
        }
    }
    if !exited {
        let _ = child.kill();
        let _ = child.wait();
        panic!("cairn mcp serve did not exit within 15s after stdin EOF");
    }
    let _ = child.wait();

    // ── Step 7: assert that the subprocess produced a reasoning record ─────
    let record_id = reasoning_record_id.expect(
        "cairn mcp serve subprocess never produced a reasoning record within 20s — \
         scheduler boot or handler dispatch is broken",
    );

    // Verify the source-link is present (rules out false positives).
    let conn = rusqlite::Connection::open(&db_path).expect("conn");
    let extra: String = conn
        .query_row(
            "SELECT extra_frontmatter FROM records WHERE record_id = ?1",
            rusqlite::params![record_id.as_str()],
            |r| r.get(0),
        )
        .expect("query frontmatter");
    let parsed: serde_json::Value = serde_json::from_str(&extra).expect("parse frontmatter");
    let sources = parsed
        .pointer("/consolidation/source_record_ids")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        !sources.is_empty(),
        "reasoning record must carry consolidation.source_record_ids"
    );
}
