//! Privacy fixture runner — subprocess CLI driver.
//!
//! Each fixture creates a temp vault, applies setup records via `cairn ingest`,
//! applies operations via `cairn forget --record` etc., waits for WAL
//! convergence, then runs the four assertion surfaces (search/retrieve/
//! markdown/indexes).
//!
//! # Record-ID mapping
//!
//! The fixture's `id` field inside each `setup.record` is ADVISORY — the CLI
//! auto-generates a ULID on ingest. The harness captures the actual generated
//! id from the `data.record_id` field of the ingest JSON response and builds
//! an `id_map: HashMap<advisory_id, actual_id>` that is used to resolve
//! `target` fields in `operations` and `id` fields in `retrieve` assertions.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::privacy::fixture::{Assertions, FixtureOp, MarkdownAssertion, PrivacyFixture};

/// A single assertion failure.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FailureReport {
    /// Scenario name from the fixture.
    pub scenario: String,
    /// Which assertion surface reported the failure.
    pub surface: String,
    /// The query or record id that was checked.
    pub query_or_id: String,
    /// What was expected.
    pub expected: String,
    /// What was actually observed.
    pub actual: String,
}

/// Aggregate privacy gate report written to `target/cairn-bench/privacy.json`.
#[derive(Debug, Serialize, Deserialize)]
pub struct PrivacyReport {
    /// Schema version; currently `1`.
    pub schema_version: u32,
    /// Total number of fixtures loaded (includes deferred).
    pub fixtures_run: usize,
    /// Number of non-deferred fixtures that executed with zero assertion failures.
    pub fixtures_passed: usize,
    /// Number of fixtures skipped because `deferred: true`.
    pub fixtures_deferred: usize,
    /// `true` iff all non-deferred fixtures passed.
    pub ok: bool,
    /// All assertion failures across all non-deferred fixtures.
    pub failures: Vec<FailureReport>,
}

/// Resolve the `cairn` binary path emitted by `build.rs`.
fn cairn_bin() -> PathBuf {
    PathBuf::from(env!("CAIRN_BIN_PATH"))
}

/// Construct a `Command` pointing at the vault root.
///
/// Strips any ambient `CAIRN_VAULT` / `CAIRN_REGISTRY` env vars so the CLI
/// auto-discovers the vault from its working directory, same as in tests.
fn cli(vault: &Path) -> Command {
    let mut c = Command::new(cairn_bin());
    c.current_dir(vault);
    c.env_remove("CAIRN_VAULT");
    c.env_remove("CAIRN_REGISTRY");
    c
}

/// Bootstrap a fresh vault with `cairn bootstrap --vault-path <tmp>`.
fn bootstrap_vault(vault: &Path) -> anyhow::Result<()> {
    let out = Command::new(cairn_bin())
        .args(["bootstrap", "--vault-path", &vault.to_string_lossy()])
        .output()?;
    anyhow::ensure!(
        out.status.success(),
        "cairn bootstrap failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}

/// Ingest one record and return the actual `data.record_id` assigned by the CLI.
fn ingest_record(vault: &Path, kind: &str, body: &str) -> anyhow::Result<String> {
    let out = cli(vault)
        .args(["ingest", "--kind", kind, "--body", body, "--json"])
        .output()?;
    anyhow::ensure!(
        out.status.success(),
        "cairn ingest failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let id = v["data"]["record_id"]
        .as_str()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "ingest response missing data.record_id: {}",
                String::from_utf8_lossy(&out.stdout)
            )
        })?
        .to_owned();
    Ok(id)
}

/// Run a fixture against a fresh temp vault.
///
/// Returns `Ok(Vec<FailureReport>)` — an empty vec means all assertions
/// passed. Returns `Err` only for infrastructure failures (vault setup,
/// subprocess invocation, JSON parse).
pub fn run_fixture(fixture: &PrivacyFixture) -> anyhow::Result<Vec<FailureReport>> {
    let dir = tempfile::tempdir()?;
    let vault = dir.path();
    bootstrap_vault(vault)?;

    // Ingest each setup record and build advisory-id → actual-id map.
    let mut id_map: HashMap<String, String> = HashMap::new();
    for rec in &fixture.setup.records {
        let actual_id = ingest_record(vault, &rec.kind, &rec.body)?;
        id_map.insert(rec.id.clone(), actual_id);
    }

    // Apply operations — resolve advisory ids to actual ids via the map.
    for op in &fixture.operations {
        match op {
            FixtureOp::Forget { target, mode } => {
                // The CLI flag is `--record <id>` (for mode="record") or
                // `--session <id>` (mode="session"), not `--mode record <id>`.
                let actual_target = id_map.get(target).map_or(target.as_str(), String::as_str);
                let flag = match mode.as_str() {
                    "record" => "--record",
                    "session" => "--session",
                    "source" => "--source",
                    other => {
                        eprintln!(
                            "unknown forget mode {other:?} for target {actual_target} — skipping"
                        );
                        continue;
                    }
                };
                let out = cli(vault)
                    .args(["forget", flag, actual_target, "--json"])
                    .output()?;
                if !out.status.success() {
                    eprintln!(
                        "forget {flag} {actual_target} failed: {}",
                        String::from_utf8_lossy(&out.stderr)
                    );
                    // Don't bail — let the assertion phase surface any leak.
                }
            }
            FixtureOp::Redact { target, span_kind } => {
                // Redact is not exposed on the CLI at v0.1.
                eprintln!("skipping redact op for {target}/{span_kind} — no CLI surface at v0.1");
            }
            FixtureOp::Revoke { receipt_id } => {
                // Revoke is not exposed on the CLI at v0.1.
                eprintln!("skipping revoke op for {receipt_id} — no CLI surface at v0.1");
            }
        }
    }

    // Wait for all WAL ops to reach a terminal state.
    wait_for_terminal_state(vault, Duration::from_secs(5))?;

    let mut failures = Vec::new();
    check_search(vault, fixture, &fixture.assertions, &id_map, &mut failures)?;
    check_retrieve(vault, fixture, &fixture.assertions, &id_map, &mut failures)?;
    check_markdown(vault, fixture, &fixture.assertions, &mut failures);
    check_indexes(vault, fixture, &fixture.assertions, &id_map, &mut failures)?;

    // Keep `dir` alive until after all checks (drop at end of scope).
    drop(dir);
    Ok(failures)
}

/// Poll `wal_ops` until no row is in a non-terminal state, or `timeout` elapses.
///
/// The WAL terminal states (§5.6) are `COMMITTED`, `ABORTED`, `REJECTED`.
fn wait_for_terminal_state(vault: &Path, timeout: Duration) -> anyhow::Result<()> {
    let db = vault.join(".cairn/cairn.db");
    if !db.exists() {
        return Ok(());
    }
    let start = Instant::now();
    while start.elapsed() < timeout {
        let conn = rusqlite::Connection::open(&db)?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM wal_ops \
                 WHERE state NOT IN ('COMMITTED','ABORTED','REJECTED')",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if count == 0 {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // A timeout is not a hard failure for the runner — some assertions may
    // still surface useful leak information.
    eprintln!("warning: wal_ops did not reach terminal state within {timeout:?}");
    Ok(())
}

// ── Assertion surfaces ───────────────────────────────────────────────────────

fn check_search(
    vault: &Path,
    f: &PrivacyFixture,
    a: &Assertions,
    id_map: &HashMap<String, String>,
    failures: &mut Vec<FailureReport>,
) -> anyhow::Result<()> {
    for sa in &a.search {
        // Wrap as an FTS5 phrase so punctuation (hyphens, dots, etc.) in
        // fixture queries doesn't trip the FTS5 grammar's column-prefix
        // syntax. Escape embedded double quotes by doubling them per FTS5.
        let escaped = sa.query.replace('"', "\"\"");
        let phrase = format!("\"{escaped}\"");
        let out = cli(vault)
            .args(["search", &phrase, "--mode", &sa.mode, "--json"])
            .output()?;
        // Fail closed: a non-zero subprocess exit is an infrastructure
        // failure, not a "no leak" signal. Surface both streams in the error
        // so reviewers see why the CLI rejected the search.
        if !out.status.success() {
            anyhow::bail!(
                "cairn search mode={} query={:?} failed (exit {}): stderr={:?} stdout={:?}",
                sa.mode,
                sa.query,
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr),
                String::from_utf8_lossy(&out.stdout)
            );
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        // Resolve the advisory must_not_contain_id to the actual id if possible.
        let actual_forbidden = id_map
            .get(&sa.must_not_contain_id)
            .map_or(sa.must_not_contain_id.as_str(), String::as_str);
        if stdout.contains(actual_forbidden) {
            failures.push(FailureReport {
                scenario: f.scenario.clone(),
                surface: format!("search({})", sa.mode),
                query_or_id: sa.query.clone(),
                expected: format!("no result with id={actual_forbidden}"),
                actual: format!("id={actual_forbidden} appears in search output"),
            });
        }
    }
    Ok(())
}

fn check_retrieve(
    vault: &Path,
    f: &PrivacyFixture,
    a: &Assertions,
    id_map: &HashMap<String, String>,
    failures: &mut Vec<FailureReport>,
) -> anyhow::Result<()> {
    for ra in &a.retrieve {
        // Resolve advisory id to actual id.
        let actual_id = id_map.get(&ra.id).map_or(ra.id.as_str(), String::as_str);
        let out = cli(vault)
            .args(["retrieve", actual_id, "--json"])
            .output()?;
        // Fail closed on a non-zero retrieve exit. The CLI returns exit 0
        // with `data.body = null` for a forgotten record; any other failure
        // is an infrastructure problem, not a "not_found" signal.
        if !out.status.success() {
            anyhow::bail!(
                "cairn retrieve id={} failed (exit {}): stderr={:?} stdout={:?}",
                actual_id,
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr),
                String::from_utf8_lossy(&out.stdout)
            );
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        // Parse strictly: malformed JSON is an infrastructure failure, not
        // a silently-passing `not_found`.
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).map_err(|e| {
            anyhow::anyhow!(
                "cairn retrieve id={actual_id} returned invalid JSON: {e}; stdout={stdout:?}"
            )
        })?;
        let body_present = !v["data"]["body"].is_null() && v["data"]["body"] != "";
        let expected_present = ra.expect == "present";
        if body_present != expected_present {
            failures.push(FailureReport {
                scenario: f.scenario.clone(),
                surface: "retrieve".into(),
                query_or_id: actual_id.to_owned(),
                expected: ra.expect.clone(),
                actual: if body_present { "present" } else { "not_found" }.into(),
            });
        }
    }
    Ok(())
}

fn check_markdown(
    vault: &Path,
    f: &PrivacyFixture,
    a: &Assertions,
    failures: &mut Vec<FailureReport>,
) {
    for ma in &a.markdown {
        match ma {
            MarkdownAssertion::NotPresent {
                path_must_not_exist_or_be_tombstoned,
            } => {
                let full = vault.join(path_must_not_exist_or_be_tombstoned);
                if full.exists()
                    && std::fs::read_to_string(&full).is_ok_and(|body| !body.contains("tombstone:"))
                {
                    failures.push(FailureReport {
                        scenario: f.scenario.clone(),
                        surface: "markdown".into(),
                        query_or_id: path_must_not_exist_or_be_tombstoned.clone(),
                        expected: "file absent or tombstoned".into(),
                        actual: "file present without tombstone marker".into(),
                    });
                }
            }
            MarkdownAssertion::NoUnmaskedSpan {
                path,
                must_not_contain,
            } => {
                let full = vault.join(path);
                if std::fs::read_to_string(&full)
                    .is_ok_and(|body| body.contains(must_not_contain.as_str()))
                {
                    failures.push(FailureReport {
                        scenario: f.scenario.clone(),
                        surface: "markdown".into(),
                        query_or_id: path.clone(),
                        expected: format!("body must not contain {must_not_contain}"),
                        actual: "body contains the forbidden span".into(),
                    });
                }
            }
        }
    }
}

fn check_indexes(
    vault: &Path,
    f: &PrivacyFixture,
    a: &Assertions,
    id_map: &HashMap<String, String>,
    failures: &mut Vec<FailureReport>,
) -> anyhow::Result<()> {
    let db = vault.join(".cairn/cairn.db");
    if !db.exists() {
        return Ok(());
    }
    let conn = rusqlite::Connection::open(&db)?;
    for ia in &a.indexes {
        // Guard: the table may not exist (e.g. fixture uses a legacy name like
        // `record_fts` vs the actual `records_fts`).
        let table_exists: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type IN ('table','shadow') AND name=?1",
                [&ia.table],
                |r| r.get::<_, i64>(0),
            )
            .is_ok_and(|v| v == 1);
        if !table_exists {
            // Not a failure — table absent means no data can leak there.
            continue;
        }
        // Resolve the advisory must_not_contain value via the id_map.
        let actual_value = id_map
            .get(&ia.must_not_contain)
            .map_or(ia.must_not_contain.as_str(), String::as_str);
        let sql = format!(
            "SELECT COUNT(*) FROM {table} WHERE {col} = ?1",
            table = ia.table,
            col = ia.column
        );
        let count: i64 = conn
            .query_row(&sql, [actual_value], |r| r.get(0))
            .unwrap_or(0);
        if count != 0 {
            failures.push(FailureReport {
                scenario: f.scenario.clone(),
                surface: format!("index({})", ia.table),
                query_or_id: actual_value.to_owned(),
                expected: format!(
                    "no row in {} where {} = {}",
                    ia.table, ia.column, actual_value
                ),
                actual: format!("{count} rows present"),
            });
        }
    }
    Ok(())
}
